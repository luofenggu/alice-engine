//! Mad Hatter — HTTP service framework that eliminates route string literals.
//!
//! # Example
//!
//! ```ignore
//! use mad_hatter::http_service;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize)]
//! struct User { id: u64, name: String }
//!
//! #[derive(Deserialize)]
//! struct CreateUserReq { name: String }
//!
//! http_service! {
//!     service UserApi {
//!         GET    "users/{id}" => get_user(id: u64) -> Json<User>;
//!         POST   "users"     => create_user(body: CreateUserReq) -> Json<User>;
//!         DELETE "users/{id}" => delete_user(id: u64);
//!     }
//! }
//!
//! struct App;
//!
//! impl UserApiService for App {
//!     async fn get_user(&self, id: u64) -> mad_hatter::Result<axum::Json<User>> {
//!         Ok(axum::Json(User { id, name: "Alice".into() }))
//!     }
//!     async fn create_user(&self, body: CreateUserReq) -> mad_hatter::Result<axum::Json<User>> {
//!         Ok(axum::Json(User { id: 1, name: body.name }))
//!     }
//!     async fn delete_user(&self, id: u64) -> mad_hatter::Result<()> {
//!         Ok(())
//!     }
//! }
//! ```

// Re-export the proc macro
pub use mad_hatter_macros::http_service;
pub use mad_hatter_macros::bind_http;
pub use mad_hatter_macros::ToMarkdown;
pub use mad_hatter_macros::FromMarkdown;
pub use mad_hatter_macros::tunnel_service;
pub use async_trait::async_trait;

pub mod llm;
pub mod tunnel;
pub use llm::{LlmChannel, OpenAiChannel, StructInput, StructOutput, infer, infer_with_on_text, stream_infer, stream_infer_with_on_text, StreamInfer};

// Re-export axum types that generated code needs
pub use axum;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// HTTP error type for service handlers.
///
/// Automatically converts to an HTTP response with the given status code and message.
#[derive(Debug, Clone)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
}

impl HttpError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status.as_u16(), self.message)
    }
}

impl std::error::Error for HttpError {}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": {
                "code": self.status.as_u16(),
                "message": self.message,
            }
        });
        (self.status, axum::Json(body)).into_response()
    }
}

/// Result type alias for service handlers.
pub type Result<T> = std::result::Result<T, HttpError>;


pub mod dao;
