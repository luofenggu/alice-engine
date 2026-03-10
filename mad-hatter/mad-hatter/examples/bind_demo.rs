//! Demo: bind_http! macro — separate trait from HTTP binding.
//!
//! Run with: `cargo run --example bind_demo`
//! Test with:
//!   curl http://localhost:3001/status
//!   curl http://localhost:3001/users?limit=2
//!   curl -X POST http://localhost:3001/users -H 'Content-Type: application/json' -d '{"name":"Alice"}'
//!   curl http://localhost:3001/users/42
//!   curl http://localhost:3001/users/42/avatar
//!   curl -X PUT http://localhost:3001/users/42/bio -H 'Content-Type: text/plain' -d 'Hello world'
//!   curl -X DELETE http://localhost:3001/users/42

use mad_hatter::bind_http;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// --- Domain types ---

#[derive(Serialize, Clone)]
struct User {
    id: u64,
    name: String,
}

#[derive(Serialize, Clone)]
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
    limit: Option<usize>,
}

// --- Pure business trait (zero framework dependency) ---

trait UserService: Send + Sync + 'static {
    fn get_status(&self) -> impl std::future::Future<Output = StatusInfo> + Send;
    fn list_users(&self, query: ListQuery) -> impl std::future::Future<Output = Vec<User>> + Send;
    fn create_user(&self, body: CreateUserReq) -> impl std::future::Future<Output = User> + Send;
    fn get_user(&self, id: u64) -> impl std::future::Future<Output = User> + Send;
    fn get_avatar(&self, id: u64) -> impl std::future::Future<Output = String> + Send;
    fn update_bio(&self, id: u64, bio: String) -> impl std::future::Future<Output = String> + Send;
    fn delete_user(&self, id: u64) -> impl std::future::Future<Output = ()> + Send;
}

// --- Application state ---

struct App {
    next_id: AtomicU64,
}

// --- Implement the hand-written trait ---

impl UserService for App {
    async fn get_status(&self) -> StatusInfo {
        StatusInfo {
            version: "1.0.0".into(),
            uptime: 42,
        }
    }

    async fn list_users(&self, query: ListQuery) -> Vec<User> {
        let limit = query.limit.unwrap_or(5);
        (0..limit as u64)
            .map(|i| User {
                id: i,
                name: format!("User#{i}"),
            })
            .collect()
    }

    async fn create_user(&self, body: CreateUserReq) -> User {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        User {
            id,
            name: body.name,
        }
    }

    async fn get_user(&self, id: u64) -> User {
        User {
            id,
            name: format!("User#{id}"),
        }
    }

    async fn get_avatar(&self, id: u64) -> String {
        format!("PNG-AVATAR-DATA-FOR-USER-{id}")
    }

    async fn update_bio(&self, id: u64, bio: String) -> String {
        println!("[USER] Updated bio for user {id}: {bio}");
        format!("Bio updated for user {id}")
    }

    async fn delete_user(&self, id: u64) -> () {
        println!("[USER] Deleted user {id}");
    }
}

// --- Bind the trait to HTTP routes (the only place route strings appear) ---

bind_http! {
    UserService for App {
        get_status()                              => GET    "status"            -> Json<StatusInfo>;
        list_users(query: ListQuery)              => GET    "users"             -> Json<Vec<User>>;
        create_user(body: CreateUserReq)           => POST   "users"             -> Json<User>;
        get_user(id: u64)                         => GET    "users/{id}"        -> Json<User>;
        get_avatar(id: u64)                       => GET    "users/{id}/avatar" -> Response;
        update_bio(id: u64, body: String)         => PUT    "users/{id}/bio"    -> Response;
        delete_user(id: u64)                      => DELETE "users/{id}";
    }
}

// --- Main ---

#[tokio::main]
async fn main() {
    let app = App {
        next_id: AtomicU64::new(100),
    };

    let state = Arc::new(app);
    let router = UserServiceBind::router().with_state(state);

    let addr = "0.0.0.0:3001";
    println!("[SERVER] Listening on {addr}");
    println!("[SERVER] Try:");
    println!("  curl http://localhost:3001/status");
    println!("  curl http://localhost:3001/users?limit=3");
    println!("  curl -X POST http://localhost:3001/users -H 'Content-Type: application/json' -d '{{\"name\":\"Alice\"}}'");
    println!("  curl http://localhost:3001/users/42");
    println!("  curl http://localhost:3001/users/42/avatar");
    println!("  curl -X PUT http://localhost:3001/users/42/bio -H 'Content-Type: text/plain' -d 'Hello world'");
    println!("  curl -X DELETE http://localhost:3001/users/42");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, router).await.unwrap();
}

