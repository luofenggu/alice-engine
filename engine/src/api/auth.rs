//! Authentication middleware and login/logout handlers.
//!
//! Uses EngineState for auth configuration (session_token, auth_secret, skip_auth).

use std::sync::Arc;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use route_macro::*;

use crate::api::http_protocol;
use crate::api::state::EngineState;

#[derive(serde::Deserialize)]
pub struct LoginForm {
    password: String,
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

    // Whitelist: login page, route-annotated public endpoints, static files, public prefix
    if path == http_protocol::LOGIN_PATH
        || path == ROUTE_HANDLE_LEGACY_LOGIN
        || path == ROUTE_HANDLE_FRONTEND_ERROR
        || http_protocol::AUTH_WHITELIST_STATIC.contains(&path.as_str())
        || http_protocol::AUTH_WHITELIST_PREFIXES.iter().any(|p| path.starts_with(p))
    {
        return next.run(req).await;
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

/// Legacy login endpoint — accepts password as plain text body, sets cookie.
/// Compatible with old HTML frontend's `/api/auth` POST.
#[post("/api/auth")]
pub async fn handle_legacy_login(
    State(state): State<Arc<EngineState>>,
    body: String,
) -> Response {
    use axum::http::{header, HeaderMap, StatusCode};

    let auth_secret = &state.env_config.auth_secret;
    if body.trim() == auth_secret {
        let cookie = http_protocol::build_session_cookie(&state.session_cookie_name, &state.session_token);
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        (headers, StatusCode::OK).into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}
