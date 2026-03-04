//! Authentication middleware and login/logout handlers.
//!
//! Uses EngineState for auth configuration (session_token, auth_secret, skip_auth).

use std::sync::Arc;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::state::EngineState;

const SESSION_COOKIE_NAME: &str = "alice_session";

#[derive(serde::Deserialize)]
pub struct LoginForm {
    password: String,
}

/// Auth check middleware — redirects unauthenticated requests to /login.
pub async fn check_auth(
    State(state): State<Arc<EngineState>>,
    req: Request,
    next: Next,
) -> Response {
    use axum::response::Redirect;

    if state.env_config.skip_auth {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();

    // Whitelist: login page, error reporter, public files
    if path == "/login"
        || path == "/api/auth"
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

/// Login page — serves a simple HTML form.
pub async fn handle_login_page(req: Request) -> axum::response::Html<String> {
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
        <form method="POST" action="login">
            <input type="password" name="password" placeholder="Enter password" autofocus/>
            <button type="submit">Login</button>
        </form>
    </div>
</body>
</html>"#,
        error_html
    ))
}

/// Login POST — validates password and sets session cookie.
pub async fn handle_login_post(
    State(state): State<Arc<EngineState>>,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    use axum::http::{header, HeaderMap};
    use axum::response::Redirect;

    let auth_secret = &state.env_config.auth_secret;
    if form.password.trim() == auth_secret {
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800",
            SESSION_COOKIE_NAME, state.session_token
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, Redirect::to("/")).into_response()
    } else {
        Redirect::to("/login?error=1").into_response()
    }
}

/// Logout — clears session cookie.
pub async fn handle_logout() -> Response {
    use axum::http::{header, HeaderMap};
    use axum::response::Redirect;

    let cookie = format!("{}=; Path=/; HttpOnly; Max-Age=0", SESSION_COOKIE_NAME);
    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
    (headers, Redirect::to("/login")).into_response()
}

/// Frontend error reporter — logs browser errors to file.
pub async fn handle_frontend_error(
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::http::StatusCode {
    let error_type = payload["error_type"].as_str().unwrap_or("unknown");
    let message = payload["message"].as_str().unwrap_or("");
    let source = payload["source"].as_str().unwrap_or("");

    tracing::warn!("[FRONTEND-ERROR] [{}] {} | source: {}", error_type, message, source);
    axum::http::StatusCode::OK
}

/// Legacy login endpoint — accepts password as plain text body, sets cookie.
/// Compatible with old HTML frontend's `/api/auth` POST.
pub async fn handle_legacy_login(
    State(state): State<Arc<EngineState>>,
    body: String,
) -> Response {
    use axum::http::{header, HeaderMap, StatusCode};

    let auth_secret = &state.env_config.auth_secret;
    if body.trim() == auth_secret {
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800",
            SESSION_COOKIE_NAME, state.session_token
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, StatusCode::OK).into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}
