//! Demo: a minimal HTTP service using Mad Hatter.
//!
//! Run with: `cargo run --example demo`
//! Test with:
//!   curl http://localhost:3000/users/42
//!   curl -X POST http://localhost:3000/users -H 'Content-Type: application/json' -d '{"name":"Alice"}'
//!   curl -X DELETE http://localhost:3000/users/42

use mad_hatter::http_service;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

// --- Domain types ---

#[derive(Serialize, Clone)]
struct User {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
struct CreateUserReq {
    name: String,
}

// --- Service definition (the only place route strings appear) ---

http_service! {
    service UserApi {
        GET    "users/{id}" => get_user(id: u64) -> Json<User>;
        POST   "users"      => create_user(body: CreateUserReq) -> Json<User>;
        DELETE "users/{id}"  => delete_user(id: u64);
    }
}

// --- Application state ---

struct App {
    next_id: AtomicU64,
}

// --- Implement the generated trait ---

impl UserApiService for App {
    async fn get_user(&self, id: u64) -> mad_hatter::Result<axum::Json<User>> {
        // Stub: return a fake user
        Ok(axum::Json(User {
            id,
            name: format!("User#{id}"),
        }))
    }

    async fn create_user(&self, body: CreateUserReq) -> mad_hatter::Result<axum::Json<User>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(axum::Json(User {
            id,
            name: body.name,
        }))
    }

    async fn delete_user(&self, id: u64) -> mad_hatter::Result<()> {
        println!("[USER] Deleted user {id}");
        Ok(())
    }
}

// --- Main ---

#[tokio::main]
async fn main() {
    let app = App {
        next_id: AtomicU64::new(1),
    };

    let router = UserApi::router(app);

    let addr = "0.0.0.0:3000";
    println!("[SERVER] Listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, router).await.unwrap();
}

