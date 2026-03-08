//! Hub proxy — transparent API forwarding to slave engines.
//!
//! When a request targets an instance that lives on a slave engine,
//! the hub proxies the request transparently. The frontend is unaware
//! that multiple engines exist.

use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};


use super::InstanceRoute;
use crate::api::http_protocol;

/// Proxy a request to a slave engine.
///
/// Rewrites the URL, injects auth cookie, forwards headers and body,
/// then returns the response.
pub async fn proxy_to_engine(
    client: &reqwest::Client,
    route: &InstanceRoute,
    auth_cookie: &str,
    method: Method,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let mut url = format!("{}{}", route.endpoint, path);
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }

    let req_method = http_protocol::to_reqwest_method(&method);
    let mut req = client.request(req_method, &url);

    // Forward headers (skip hop-by-hop)
    for (name, value) in headers.iter() {
        if !http_protocol::is_hop_by_hop_header(name.as_str()) {
            if let Ok(v) = value.to_str() {
                // Replace cookie header with auth cookie for slave
                if name.as_str().eq_ignore_ascii_case("cookie") {
                    continue; // We'll set our own cookie
                }
                req = req.header(name.as_str(), v);
            }
        }
    }

    // Inject auth cookie
    req = req.header("Cookie", auth_cookie);

    if !body.is_empty() {
        req = req.body(body.to_vec());
    }

    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut out_headers = HeaderMap::new();

            for (name, value) in resp.headers().iter() {
                if let Ok(val_str) = value.to_str() {
                    let name_str = name.as_str().to_lowercase();
                    // Strip hop-by-hop and set-cookie (don't leak slave cookies to client)
                    if http_protocol::is_response_hop_by_hop(&name_str) {
                        continue;
                    }
                    if name_str == "set-cookie" {
                        continue; // Don't forward slave session cookies
                    }
                    if let Ok(hv) = val_str.parse() {
                        out_headers.append(name.clone(), hv);
                    }
                }
            }

            let resp_body = resp.bytes().await.unwrap_or_default();
            (status, out_headers, resp_body).into_response()
        }
        Err(e) => {
            tracing::warn!("[HUB] Proxy error to {}: {}", route.endpoint, e);
            let body = serde_json::json!({ "error": format!("Hub proxy error: {}", e) });
            (StatusCode::BAD_GATEWAY, axum::Json(body)).into_response()
        }
    }
}

