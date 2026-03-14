//! HTTP proxy trait — tunneled HTTP request forwarding between hub nodes.
//!
//! Defines the contract for proxying HTTP requests through the tunnel.
//! Host sends requests to slave, slave forwards to local instances.

use std::collections::HashMap;
use mad_hatter::tunnel_service;
use serde::{Deserialize, Serialize};

/// HTTP request to be proxied through the tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpProxyRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    /// Base64-encoded body
    pub body: Option<String>,
}

/// HTTP response returned from the proxied request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpProxyResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    /// Base64-encoded body
    pub body: Option<String>,
}

/// HTTP proxy service for forwarding requests through the tunnel.
///
/// The `#[tunnel_service]` macro automatically generates HttpProxyProxy
/// (for tunnel RPC calls) and HttpProxyDispatcher (for local dispatch).
#[tunnel_service]
pub trait HttpProxy: Send + Sync {
    /// Proxy an HTTP request to a local instance and return the response.
    async fn proxy_http(&self, request: HttpProxyRequest) -> Result<HttpProxyResponse, String>;
}