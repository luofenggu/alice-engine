//! HTTP protocol constants and utilities — guardian-exempt zone for HTTP-specific literals.

use axum::http::HeaderMap;

/// MIME type mapping from file extension to content type.
pub fn content_type_for_extension(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webp" => "image/webp",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// Check if a header is a hop-by-hop header that should not be forwarded in proxy.
pub fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "host" | "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
    )
}

/// Check if a response header is a hop-by-hop header that should be stripped.
pub fn is_response_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer"
    )
}

/// Convert axum Method to reqwest Method.
pub fn to_reqwest_method(method: &axum::http::Method) -> reqwest::Method {
    match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "PATCH" => reqwest::Method::PATCH,
        "HEAD" => reqwest::Method::HEAD,
        "OPTIONS" => reqwest::Method::OPTIONS,
        _ => reqwest::Method::GET,
    }
}

/// Forward request headers, filtering out hop-by-hop headers.
pub fn forward_request_headers(
    headers: &HeaderMap,
    builder: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut req = builder;
    for (name, value) in headers.iter() {
        if !is_hop_by_hop_header(name.as_str()) {
            if let Ok(v) = value.to_str() {
                req = req.header(name.as_str(), v);
            }
        }
    }
    req
}

/// Proxy path prefix constant.
pub const PROXY_PATH_PREFIX: &str = "/proxy/";

/// Minimum allowed proxy port.
pub const PROXY_PORT_MIN: u16 = 1024;

/// Public directory prefix for access control.
pub const PUBLIC_DIR_PREFIX: &str = "apps/";

/// Rewrite a Set-Cookie header's path to include the proxy prefix.
pub fn rewrite_cookie_path(cookie: &str, proxy_prefix: &str) -> String {
    let lower = cookie.to_lowercase();
    if let Some(idx) = lower.find("path=/") {
        let path_start = idx + 5;
        let path_end = cookie[path_start..]
            .find(';')
            .map(|i| path_start + i)
            .unwrap_or(cookie.len());
        let original_path = &cookie[path_start..path_end];
        if !original_path.starts_with(proxy_prefix) {
            let new_path = format!("{}{}", proxy_prefix, original_path);
            return format!(
                "{}{}{}",
                &cookie[..path_start],
                new_path,
                &cookie[path_end..]
            );
        }
    }
    cookie.to_string()
}

/// API key masking parameters.
pub const API_KEY_MASK_PREFIX_LEN: usize = 4;
pub const API_KEY_MASK_SUFFIX_LEN: usize = 4;
pub const API_KEY_MASK_MIN_LEN: usize = 8;
pub const API_KEY_FIELD_NAME: &str = "api_key";

/// Mask an API key value, showing only prefix and suffix.
pub fn mask_api_key(s: &str) -> String {
    if s.len() > API_KEY_MASK_MIN_LEN {
        format!(
            "{}...{}",
            &s[..API_KEY_MASK_PREFIX_LEN],
            &s[s.len() - API_KEY_MASK_SUFFIX_LEN..]
        )
    } else {
        s.to_string()
    }
}

/// Default message query limit.
pub const DEFAULT_MESSAGE_LIMIT: i64 = 50;

// ── Proxy URL construction ──

/// Parse proxy path into (port, target_path).
/// Input: the path after "/proxy/" prefix has been stripped.
/// Returns None if port is invalid.
pub fn parse_proxy_target(rest: &str) -> Option<(u16, &str)> {
    let (port_str, target_path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, "/"),
    };
    let port: u16 = port_str.parse().ok()?;
    if port >= PROXY_PORT_MIN {
        Some((port, target_path))
    } else {
        None
    }
}

/// Build the target URL for proxy forwarding.
pub fn build_proxy_url(port: u16, path: &str, query: Option<&str>) -> String {
    let mut url = format!("http://localhost:{}{}", port, path);
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    url
}

/// Build the proxy prefix string for a given port.
pub fn build_proxy_prefix(port: u16) -> String {
    format!("{}{}", PROXY_PATH_PREFIX, port)
}

/// Process a response header for proxy forwarding.
/// Returns the (possibly rewritten) header value, or None if the header should be stripped.
pub fn process_proxy_response_header(
    name: &str,
    value: &str,
    proxy_prefix: &str,
) -> Option<String> {
    let name_lower = name.to_lowercase();
    match name_lower.as_str() {
        "location" => {
            let rewritten = if value.starts_with('/') && !value.starts_with(proxy_prefix) {
                format!("{}{}", proxy_prefix, value)
            } else {
                value.to_string()
            };
            Some(rewritten)
        }
        "set-cookie" => Some(rewrite_cookie_path(value, proxy_prefix)),
        n if is_response_hop_by_hop(n) => None,
        _ => Some(value.to_string()),
    }
}

// ── Auth cookie utilities ──

/// Session cookie name prefix.
pub const SESSION_COOKIE_PREFIX: &str = "alice_session_";

/// Build a session cookie name from an identifier.
pub fn build_session_cookie_name(id: &str) -> String {
    format!("{}{}", SESSION_COOKIE_PREFIX, id)
}

/// Build a Set-Cookie header value for session creation.
pub fn build_session_cookie(name: &str, token: &str) -> String {
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800",
        name, token
    )
}

/// Build a Set-Cookie header value for session deletion (logout).
pub fn build_clear_cookie(name: &str) -> String {
    format!("{}=; Path=/; HttpOnly; Max-Age=0", name)
}

/// Extract session token from a Cookie header value.
pub fn extract_session_token<'a>(cookies: &'a str, cookie_name: &str) -> Option<&'a str> {
    let prefix = format!("{}=", cookie_name);
    for cookie in cookies.split(';') {
        let cookie = cookie.trim();
        if let Some(value) = cookie.strip_prefix(&prefix) {
            return Some(value);
        }
    }
    None
}

// ── Auth whitelist paths ──

/// Static file paths that bypass auth (not route-annotated handlers).
pub const SETUP_PAGE_FILE: &str = "/setup.html";

pub const AUTH_WHITELIST_STATIC: &[&str] = &["/login.html", "/setup.html", "/error-reporter.js", "/api/hub/ws"];

/// Path prefixes that bypass auth.
pub const AUTH_WHITELIST_PREFIXES: &[&str] = &["/public/"];

/// Login redirect path.
pub const LOGIN_PATH: &str = "/login";

/// Login error redirect path.
pub const LOGIN_ERROR_PATH: &str = "/login?error=1";

/// Login page static file redirect.
pub const LOGIN_PAGE_FILE: &str = "/login.html";

/// Root path (post-login redirect).
pub const ROOT_PATH: &str = "/";
