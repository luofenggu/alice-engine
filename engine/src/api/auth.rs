//! Authentication middleware and login/logout handlers.
//!
//! Uses EngineState for auth configuration (session_token, auth_secret, skip_auth).

use std::sync::Arc;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::state::EngineState;

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
        || path == "/login.html"
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
                if let Some(value) = cookie.strip_prefix(&format!("{}=", state.session_cookie_name)) {
                    if value == state.session_token {
                        return next.run(req).await;
                    }
                }
            }
        }
    }

    Redirect::to("/login").into_response()
}

/// Login page — redirects to login.html (static file).
pub async fn handle_login_page() -> Response {
    use axum::response::Redirect;
    Redirect::to("/login.html").into_response()
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
            state.session_cookie_name, state.session_token
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, Redirect::to("/")).into_response()
    } else {
        Redirect::to("/login?error=1").into_response()
    }
}

/// Logout — clears session cookie.
pub async fn handle_logout(
    State(state): State<Arc<EngineState>>,
) -> Response {
    use axum::http::{header, HeaderMap};
    use axum::response::Redirect;

    let cookie = format!("{}=; Path=/; HttpOnly; Max-Age=0", state.session_cookie_name);
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
            state.session_cookie_name, state.session_token
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, StatusCode::OK).into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}
