//! Standalone Mock LLM Server binary.
//!
//! Usage: mock-llm-server <script.json> [port]
//!
//! Reads a JSON script file and starts an HTTP server impersonating
//! an OpenAI-compatible LLM provider.
//!
//! Script JSON format:
//! ```json
//! [
//!   {"response": "...", "expected_user_contains": "keyword"},
//!   {"response": "..."}
//! ]
//! ```
//!
//! Prints the listening port to stdout on startup.

use alice_integration::mock_llm::{MockLlmServer, MockScript};
use serde::Deserialize;

#[derive(Deserialize)]
struct ScriptEntry {
    response: String,
    expected_user_contains: Option<String>,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: mock-llm-server <script.json> [port]");
        std::process::exit(1);
    }

    let script_path = &args[1];
    let port: u16 = args.get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let content = std::fs::read_to_string(script_path)
        .unwrap_or_else(|e| {
            eprintln!("Failed to read script file '{}': {}", script_path, e);
            std::process::exit(1);
        });

    let entries: Vec<ScriptEntry> = serde_json::from_str(&content)
        .unwrap_or_else(|e| {
            eprintln!("Failed to parse script JSON: {}", e);
            std::process::exit(1);
        });

    let scripts: Vec<MockScript> = entries.into_iter().map(|e| {
        match e.expected_user_contains {
            Some(expected) => MockScript::with_user_assert(e.response, expected),
            None => MockScript::new(e.response),
        }
    }).collect();

    let count = scripts.len();
    let mock = if port > 0 {
        MockLlmServer::start_on_port(scripts, port).await
    } else {
        MockLlmServer::start(scripts).await
    };

    // Print port to stdout for the orchestration script to capture
    println!("{}", mock.port);
    eprintln!("[MOCK-LLM] Started with {} scripts on port {}", count, mock.port);

    // Keep running until killed
    tokio::signal::ctrl_c().await.ok();
}
