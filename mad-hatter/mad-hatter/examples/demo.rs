//! Demo: a comprehensive HTTP service using Mad Hatter.
//!
//! Run with: `cargo run --example demo`
//! Test with:
//!   curl http://localhost:3000/status                          # P4: no path params
//!   curl http://localhost:3000/users?limit=10                  # P1: query params + P2: same path GET
//!   curl -X POST http://localhost:3000/users -H 'Content-Type: application/json' -d '{"name":"Alice"}'  # P2: same path POST
//!   curl http://localhost:3000/users/42                        # path param + Json response
//!   curl http://localhost:3000/users/42/avatar                 # P0: Response return type
//!   curl -X PUT http://localhost:3000/users/42/bio -H 'Content-Type: text/plain' -d 'Hello world'  # P3: raw String body
//!   curl -X DELETE http://localhost:3000/users/42              # no return (204)

use mad_hatter::http_service;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// --- Domain types ---

#[derive(Serialize, Clone)]
struct User {
    id: u64,
    name: String,
}

#[derive(Serialize)]
struct StatusInfo {
    version: String,
    uptime: u64,
}

#[derive(Deserialize)]
struct CreateUserReq {
    name: String,
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<u64>,
    offset: Option<u64>,
}

// --- Service definition (the only place route strings appear) ---

http_service! {
    service DemoApi {
        // P4: no path params
        GET "status" => get_status() -> Json<StatusInfo>;

        // Original: path param + Json response
        GET "users/{id}" => get_user(id: u64) -> Json<User>;

        // P2: same path, different methods (GET + POST)
        // P1: query params on GET
        GET  "users" => list_users(query: ListQuery) -> Json<Vec<User>>;
        POST "users" => create_user(body: CreateUserReq) -> Json<User>;

        // P0: Response return type
        GET "users/{id}/avatar" => get_avatar(id: u64) -> Response;

        // P3: raw String body + P0: Response return
        PUT "users/{id}/bio" => update_bio(id: u64, body: String) -> Response;

        // Original: no return (204)
        DELETE "users/{id}" => delete_user(id: u64);
    }
}

// --- Application state ---

struct App {
    next_id: AtomicU64,
}

// --- Implement the generated trait ---

impl DemoApiService for App {
    // P4: no path params
    async fn get_status(&self) -> mad_hatter::Result<axum::Json<StatusInfo>> {
        Ok(axum::Json(StatusInfo {
            version: "1.0.0".into(),
            uptime: 42,
        }))
    }

    // path param + Json
    async fn get_user(&self, id: u64) -> mad_hatter::Result<axum::Json<User>> {
        Ok(axum::Json(User {
            id,
            name: format!("User#{id}"),
        }))
    }

    // P1: query params + P2: same path GET
    async fn list_users(&self, query: ListQuery) -> mad_hatter::Result<axum::Json<Vec<User>>> {
        let limit = query.limit.unwrap_or(10);
        let offset = query.offset.unwrap_or(0);
        let users: Vec<User> = (0..limit)
            .map(|i| User {
                id: offset + i,
                name: format!("User#{}", offset + i),
            })
            .collect();
        Ok(axum::Json(users))
    }

    // P2: same path POST
    async fn create_user(&self, body: CreateUserReq) -> mad_hatter::Result<axum::Json<User>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(axum::Json(User {
            id,
            name: body.name,
        }))
    }

    // P0: Response return type
    async fn get_avatar(&self, id: u64) -> mad_hatter::Result<axum::response::Response> {
        use axum::response::IntoResponse;
        // Return a simple text response as a placeholder
        let body = format!("PNG-AVATAR-DATA-FOR-USER-{id}");
        Ok((
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            body,
        )
            .into_response())
    }

    // P3: raw String body
    async fn update_bio(&self, id: u64, body: String) -> mad_hatter::Result<axum::response::Response> {
        use axum::response::IntoResponse;
        println!("[USER] Updated bio for user {id}: {body}");
        Ok(format!("Bio updated for user {id}").into_response())
    }

    // no return (204)
    async fn delete_user(&self, id: u64) -> mad_hatter::Result<()> {
        println!("[USER] Deleted user {id}");
        Ok(())
    }
}

// --- Main ---

#[tokio::main]
async fn main() {
    let app = App {
        next_id: AtomicU64::new(100),
    };

    let state = Arc::new(app);
    let router = DemoApi::router::<App>().with_state(state);

    let addr = "0.0.0.0:3000";
    println!("[SERVER] Listening on {addr}");
    println!("[SERVER] Try:");
    println!("  curl http://localhost:3000/status");
    println!("  curl http://localhost:3000/users?limit=3");
    println!("  curl -X POST http://localhost:3000/users -H 'Content-Type: application/json' -d '{{\"name\":\"Alice\"}}'");
    println!("  curl http://localhost:3000/users/42");
    println!("  curl http://localhost:3000/users/42/avatar");
    println!("  curl -X PUT http://localhost:3000/users/42/bio -H 'Content-Type: text/plain' -d 'Hello world'");
    println!("  curl -X DELETE http://localhost:3000/users/42");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, router).await.unwrap();
}

