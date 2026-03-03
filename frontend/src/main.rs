#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::middleware::{self, Next};
    use axum::routing::{get, post};
    use axum::Router;
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    use tower_http::compression::CompressionLayer;

    use alice_frontend::app::*;
    use alice_frontend::api;

    // ── Auth Setup ──

    let auth_secret =
        std::env::var("ALICE_AUTH_SECRET").unwrap_or_else(|_| "alice-local-default".to_string());

    let session_token = {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:alice-session-salt", auth_secret).as_bytes());
        hex::encode(hasher.finalize())
    };

    let skip_auth = std::env::var("ALICE_SKIP_AUTH")
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);

    println!(
        "[AUTH] secret={} (len={}), skip={}",
        if auth_secret == "alice-local-default" { "default" } else { "custom" },
        auth_secret.len(),
        skip_auth
    );

    let auth_state = Arc::new(AuthState {
        session_token,
        auth_secret,
        skip_auth,
    });

    // ── Server Function Registration ──

    server_fn::axum::register_explicit::<GetInstances>();
    server_fn::axum::register_explicit::<GetMessages>();
    server_fn::axum::register_explicit::<SendChatMessage>();
    server_fn::axum::register_explicit::<ReportFrontendError>();
    server_fn::axum::register_explicit::<GetRepliesAfter>();
    server_fn::axum::register_explicit::<ObserveInstance>();
    server_fn::axum::register_explicit::<InterruptInstance>();

    server_fn::axum::register_explicit::<CreateInstanceFn>();
    server_fn::axum::register_explicit::<DeleteInstanceFn>();

    // ── Leptos Setup ──

    let conf = get_configuration(Some("Cargo.toml")).unwrap();
    let addr = conf.leptos_options.site_addr;
    let leptos_options = conf.leptos_options;
    let routes = generate_route_list(App);

    // ── API State ──

    let instances_dir = std::env::var("ALICE_INSTANCES_DIR")
        .unwrap_or_else(|_| "/opt/alice/instances".to_string());
    let rpc_socket = std::env::var("ALICE_RPC_SOCKET")
        .unwrap_or_else(|_| "/opt/alice/engine/alice-rpc.sock".to_string());

    let api_state = Arc::new(api::ApiState {
        instances_dir: std::path::PathBuf::from(&instances_dir),
        rpc_socket,
    });

    println!("[API] instances_dir={}, rpc_socket={}", instances_dir, api_state.rpc_socket);

    // ── Router ──
    // Auth middleware uses a closure to capture auth_state (avoids state type mismatch with LeptosOptions)
    let auth = auth_state.clone();
    let app = Router::new()
        // Public API routes (no auth required)
        .merge(api::public_api_routes().with_state(api_state.clone()))
        // Public routes (before auth middleware)
        .route("/login", get({
            let _auth = auth_state.clone();
            move |req: axum::extract::Request| handle_login_page(req)
        }).post({
            let auth = auth_state.clone();
            move |form: axum::Form<LoginForm>| handle_login_post(auth, form)
        }))
        .route("/logout", get(handle_logout))
        // Frontend error reporting
        .route("/api/frontend-error", post(handle_frontend_error))
        // Authenticated API routes
        .merge(api::authenticated_api_routes().with_state(api_state.clone()))
        // Server Functions catch-all
        .route("/api/{*fn_name}", post(leptos_axum::handle_server_fns))
        // Leptos routes (SSR pages)
        .leptos_routes(&leptos_options, routes, {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        // Auth middleware — closure captures auth_state
        .layer(middleware::from_fn(move |req: axum::extract::Request, next: Next| {
            let auth = auth.clone();
            async move { check_auth(auth, req, next).await }
        }))
        // Compression (gzip) — WASM 1.3MB → ~470KB
        .layer(CompressionLayer::new())
        // Cache headers for static assets
        .layer(middleware::from_fn(cache_static_assets))
        .with_state(leptos_options);

    println!("🚀 Alice Frontend listening on http://{}", &addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

// ── Cache Static Assets ──

#[cfg(feature = "ssr")]
async fn cache_static_assets(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let mut response = next.run(req).await;

    // /pkg/ files have hashed names — cache aggressively
    if path.starts_with("/pkg/") {
        response.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            "public, max-age=60".parse().unwrap(), // 1 min, re-validate on refresh
        );
    }

    response
}

// ── Auth Types ──

#[cfg(feature = "ssr")]
pub struct AuthState {
    pub session_token: String,
    pub auth_secret: String,
    pub skip_auth: bool,
}

#[cfg(feature = "ssr")]
const SESSION_COOKIE_NAME: &str = "alice_session";

#[cfg(feature = "ssr")]
#[derive(serde::Deserialize)]
pub struct LoginForm {
    password: String,
}

// ── Auth Check ──

#[cfg(feature = "ssr")]
async fn check_auth(
    state: std::sync::Arc<AuthState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    if state.skip_auth {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();

    // Whitelist: login page, static assets, error reporter, public files
    if path == "/login"
        || path.starts_with("/pkg/")
        || path == "/error-reporter.js"
        || path == "/api/frontend-error"
        || path.starts_with("/public/")
    {
        return next.run(req).await;
    }

    // Check cookie
    if let Some(cookie_header) = req.headers().get(axum::http::header::COOKIE) {
        if let Ok(cookies) = cookie_header.to_str() {
            for cookie in cookies.split(';') {
                let cookie = cookie.trim();
                if let Some(value) = cookie.strip_prefix(&format!("{}=", SESSION_COOKIE_NAME)) {
                    if value == state.session_token {
                        return next.run(req).await;
                    }
                }
            }
        }
    }

    Redirect::to("/login").into_response()
}

// ── Login Page (pure HTML) ──

#[cfg(feature = "ssr")]
async fn handle_login_page(req: axum::extract::Request) -> axum::response::Html<String> {
    let query = req.uri().query().unwrap_or("");
    let show_error = query.contains("error=1");

    let error_html = if show_error {
        r#"<div class="login-error">密码错误</div>"#
    } else {
        ""
    };

    axum::response::Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
    <title>Alice - Login</title>
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
            background: #1a1a2e; color: #e0e0e0; height: 100vh;
            display: flex; align-items: center; justify-content: center;
        }}
        .login-box {{
            background: #16213e; border: 1px solid #2a2a4a;
            border-radius: 12px; padding: 40px; width: 360px; text-align: center;
        }}
        .login-box h1 {{ font-size: 28px; color: #a0a0ff; margin-bottom: 8px; }}
        .login-box .subtitle {{ font-size: 14px; color: #888; margin-bottom: 24px; }}
        .login-box input {{
            width: 100%; padding: 12px 16px; background: #1a1a2e;
            border: 1px solid #2a2a4a; border-radius: 8px; color: #e0e0e0;
            font-size: 16px; outline: none; margin-bottom: 16px; box-sizing: border-box;
        }}
        .login-box input:focus {{ border-color: #4a6fa5; }}
        .login-box button {{
            width: 100%; padding: 12px; background: #2d5aa0; color: #fff;
            border: none; border-radius: 8px; font-size: 16px; cursor: pointer;
        }}
        .login-box button:hover {{ background: #3a6fb5; }}
        .login-error {{
            background: #4a1a1a; border: 1px solid #ff6b6b; color: #ff6b6b;
            padding: 10px; border-radius: 8px; margin-bottom: 16px; font-size: 14px;
        }}
    </style>
</head>
<body>
    <div class="login-box">
        <h1>Alice</h1>
        <div class="subtitle">Authentication Required</div>
        {}
        <form method="POST" action="/login">
            <input type="password" name="password" placeholder="Enter password" autofocus/>
            <button type="submit">Login</button>
        </form>
    </div>
</body>
</html>"#,
        error_html
    ))
}

// ── Login POST ──

#[cfg(feature = "ssr")]
async fn handle_login_post(
    auth: std::sync::Arc<AuthState>,
    axum::Form(form): axum::Form<LoginForm>,
) -> axum::response::Response {
    use axum::http::{header, HeaderMap};
    use axum::response::{IntoResponse, Redirect};

    if form.password.trim() == auth.auth_secret {
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800",
            SESSION_COOKIE_NAME, auth.session_token
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, Redirect::to("/")).into_response()
    } else {
        Redirect::to("/login?error=1").into_response()
    }
}

// ── Logout ──

#[cfg(feature = "ssr")]
async fn handle_logout() -> axum::response::Response {
    use axum::http::{header, HeaderMap};
    use axum::response::{IntoResponse, Redirect};

    let cookie = format!("{}=; Path=/; HttpOnly; Max-Age=0", SESSION_COOKIE_NAME);
    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
    (headers, Redirect::to("/login")).into_response()
}

// ── Frontend Error Handler ──

#[cfg(feature = "ssr")]
async fn handle_frontend_error(
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::http::StatusCode {
    use std::io::Write;

    let error_type = payload["error_type"].as_str().unwrap_or("unknown");
    let message = payload["message"].as_str().unwrap_or("");
    let source = payload["source"].as_str().unwrap_or("");

    let log_path = "/opt/alice/logs/frontend-error.log";
    let timestamp = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let ts = secs + 8 * 3600;
        let h = (ts % 86400) / 3600;
        let m = (ts % 3600) / 60;
        let s = ts % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    };

    let line = format!("[{}] [{}] {} | source: {}\n", timestamp, error_type, message, source);

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true).append(true).open(log_path)
    {
        let _ = file.write_all(line.as_bytes());
    }

    eprintln!("[FRONTEND-ERROR] {}", line.trim());
    axum::http::StatusCode::OK
}

#[cfg(not(feature = "ssr"))]
pub fn main() {
    // hydrate entry point is in lib.rs
}
