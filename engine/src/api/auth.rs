//! Authentication middleware and login/logout/setup handlers.
//!
//! Uses EngineState for auth configuration (session_token, auth_secret, skip_auth).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use axum::extract::{Request, State};
use axum::extract::connect_info::ConnectInfo;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use route_macro::*;

use crate::api::http_protocol;
use crate::api::state::EngineState;
use crate::persist::Settings;

#[derive(serde::Deserialize)]
pub struct LoginForm {
    password: String,
}

#[derive(serde::Deserialize)]
pub struct SetupPayload {
    api_key: String,
    model: String,
}

#[derive(serde::Deserialize)]
pub struct FrontendErrorPayload {
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// Auth check middleware — redirects unauthenticated requests to /login.
///
/// Flow: skip_auth → whitelist → setup check → default-secret skip → cookie check → redirect login
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

    // Whitelist: login, setup, frontend-error, static files, public prefix
    if path == http_protocol::LOGIN_PATH
        || path == ROUTE_HANDLE_FRONTEND_ERROR
        || path == ROUTE_HANDLE_SETUP
        || http_protocol::AUTH_WHITELIST_STATIC.contains(&path.as_str())
        || http_protocol::AUTH_WHITELIST_PREFIXES.iter().any(|p| path.starts_with(p))
    {
        return next.run(req).await;
    }

    // Setup not completed — redirect to setup page
    if !state.setup_completed.load(Ordering::Relaxed) {
        return Redirect::to(http_protocol::SETUP_PAGE_FILE).into_response();
    }

    // Default auth secret — skip auth (local play mode, no password needed)
    if state.env_config.auth_secret == crate::policy::EnvConfig::DEFAULT_AUTH_SECRET {
        return next.run(req).await;
    }

    // Localhost — skip auth (same-machine access, e.g. agent script calling API)
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        if addr.ip().is_loopback() {
            return next.run(req).await;
        }
    }

    // Check cookie
    if let Some(cookie_header) = req.headers().get(axum::http::header::COOKIE) {
        if let Ok(cookies) = cookie_header.to_str() {
            if let Some(token) = http_protocol::extract_session_token(cookies, &state.session_cookie_name) {
                if token == state.session_token {
                    return next.run(req).await;
                }
            }
        }
    }

    Redirect::to(http_protocol::LOGIN_PATH).into_response()
}

/// Login page — redirects to login.html (static file).
#[get("/login")]
pub async fn handle_login_page() -> Response {
    use axum::response::Redirect;
    Redirect::to(http_protocol::LOGIN_PAGE_FILE).into_response()
}

/// Login POST — validates password and sets session cookie.
#[post("/login")]
pub async fn handle_login_post(
    State(state): State<Arc<EngineState>>,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    use axum::http::{header, HeaderMap};
    use axum::response::Redirect;

    let auth_secret = &state.env_config.auth_secret;
    if form.password.trim() == auth_secret {
        let cookie = http_protocol::build_session_cookie(&state.session_cookie_name, &state.session_token);
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, Redirect::to(http_protocol::ROOT_PATH)).into_response()
    } else {
        Redirect::to(http_protocol::LOGIN_ERROR_PATH).into_response()
    }
}

/// Logout — clears session cookie.
#[get("/api/logout")]
pub async fn handle_logout(
    State(state): State<Arc<EngineState>>,
) -> Response {
    use axum::http::{header, HeaderMap};
    use axum::response::Redirect;

    let cookie = http_protocol::build_clear_cookie(&state.session_cookie_name);
    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
    (headers, Redirect::to(http_protocol::LOGIN_PATH)).into_response()
}

/// Frontend error reporter — logs browser errors to file.
#[post("/api/frontend-error")]
pub async fn handle_frontend_error(
    axum::Json(payload): axum::Json<FrontendErrorPayload>,
) -> axum::http::StatusCode {
    let error_type = payload.error_type.as_deref().unwrap_or_default();
    let message = payload.message.as_deref().unwrap_or_default();
    let source = payload.source.as_deref().unwrap_or_default();

    tracing::warn!("[FRONTEND-ERROR] [{}] {} | source: {}", error_type, message, source);
    axum::http::StatusCode::OK
}

/// Setup handler — saves initial configuration (API key + model).
#[post("/api/setup")]
pub async fn handle_setup(
    State(state): State<Arc<EngineState>>,
    axum::Json(payload): axum::Json<SetupPayload>,
) -> Response {
    let update = Settings {
        api_key: Some(payload.api_key),
        model: Some(payload.model),
        ..Default::default()
    };
    state.update_global_settings(update).await;
    state.setup_completed.store(true, Ordering::Relaxed);
    tracing::info!("[SETUP] Initial configuration saved");
    axum::Json(serde_json::json!({"status": "ok"})).into_response() // log: setup response
}


