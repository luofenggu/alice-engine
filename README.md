# Alice Engine

A self-hosted AI agent engine written in Rust. One binary, zero external dependencies beyond an LLM provider.

Alice Engine manages autonomous agent instances — each with its own memory, knowledge, skills, and a persistent workspace. Agents communicate through messages, execute shell scripts, read and write files, and evolve their knowledge over time.

## Features

- **Single binary** — HTTP API server + static file serving, no separate frontend deployment
- **Multi-instance** — Run multiple agent instances, each with isolated memory and workspace
- **Persistent memory** — Four-layer memory system: knowledge, history, session blocks, and current context
- **Action system** — Agents execute actions (shell scripts, file I/O, messaging, self-management) through a structured protocol
- **Streaming inference** — Real-time streaming from LLM providers with action parsing
- **Three-layer settings** — Environment variables → global settings → per-instance settings, with runtime merge
- **Provider agnostic** — Any OpenAI-compatible API endpoint (configure via URL)
- **Built-in auth** — Cookie-based authentication with configurable secret
- **Static file serving** — Serve agent workspace files and public apps directory

## Quick Start

### Prerequisites

- Rust toolchain (1.75+)
- An LLM API key (OpenAI, Anthropic via OpenRouter/ZenMux, or any OpenAI-compatible provider)

### Build

```bash
cargo build --release
```

### Run

```bash
# Minimal startup
ALICE_AUTH_SECRET=your-secret \
ALICE_DEFAULT_API_KEY=sk-your-api-key \
ALICE_DEFAULT_MODEL="openrouter@anthropic/claude-sonnet-4" \
./target/release/alice-engine
```

The engine starts on port `8081` by default. Open `http://localhost:8081` in your browser, log in with your auth secret, and create your first agent instance.

### Production Example

```bash
ALICE_HTTP_PORT=9527 \
ALICE_HOST=your-server.com:9527 \
ALICE_HTML_DIR=./html-frontend \
ALICE_BASE_DIR=/var/lib/alice \
ALICE_INSTANCES_DIR=/var/lib/alice/instances \
ALICE_LOGS_DIR=/var/lib/alice/logs \
ALICE_AUTH_SECRET=your-secret \
ALICE_USER_ID=admin \
ALICE_DEFAULT_MODEL="openrouter@anthropic/claude-sonnet-4" \
ALICE_DEFAULT_API_KEY=sk-your-api-key \
ALICE_INFER_LOG_IN=true \
./target/release/alice-engine
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ALICE_HTTP_PORT` | `8081` | HTTP listen port |
| `ALICE_HOST` | *(none)* | Public host address (shown to agents for URL generation) |
| `ALICE_AUTH_SECRET` | `alice-local-default` | Authentication secret (used as login password) |
| `ALICE_SKIP_AUTH` | `false` | Skip authentication (development only) |
| `ALICE_USER_ID` | `user` | User identifier |
| `ALICE_BASE_DIR` | `.` | Base directory for relative paths |
| `ALICE_INSTANCES_DIR` | `{base}/instances` | Instance data storage |
| `ALICE_LOGS_DIR` | `{base}/logs` | Log file storage |
| `ALICE_HTML_DIR` | `{base}/html` | HTML frontend directory |
| `ALICE_DEFAULT_API_KEY` | *(empty)* | Default LLM API key for new instances |
| `ALICE_DEFAULT_MODEL` | *(from engine.toml)* | Default model in `provider@model` format |
| `ALICE_PID_FILE` | `{base}/alice-engine.pid` | PID file path |
| `ALICE_INFER_LOG_IN` | `false` | Enable inference input logging |
| `ALICE_INFER_LOG_RETENTION_DAYS` | `7` | Days to retain inference logs |
| `ALICE_SHELL_ENV` | `Linux系统，请生成bash脚本` | Shell environment description (included in agent prompts) |
| `ALICE_SHUTDOWN_SIGNAL_FILE` | `/var/run/alice-engine-shutdown.signal` | Graceful shutdown signal file |

### Model Format

Models are specified as `provider@model-name`, where the provider maps to an API endpoint defined in `engine.toml`:

```
openrouter@anthropic/claude-sonnet-4
openai@gpt-4o
zenmux@anthropic/claude-opus-4
```

Custom providers can be added to `engine.toml` under `[llm.providers]`:

```toml
[llm.providers]
openrouter = "https://openrouter.ai/api/v1/chat/completions"
openai = "https://api.openai.com/v1/chat/completions"
my-provider = "https://my-llm-proxy.com/v1/chat/completions"
```

## API Reference

All API endpoints require authentication via cookie (obtain by `POST /login`).

### Authentication

| Method | Path | Description |
|--------|------|-------------|
| GET | `/login` | Login page |
| POST | `/login` | Authenticate (form field: `password`) |
| GET | `/api/auth/check` | Check authentication status |
| GET | `/api/logout` | Log out |

### Instances

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances` | List all instances |
| POST | `/api/instances` | Create instance |
| GET | `/api/instances/{id}` | Get instance details |
| DELETE | `/api/instances/{id}` | Delete instance |

### Messaging

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances/{id}/messages` | Get message history |
| POST | `/api/instances/{id}/messages` | Send message to instance |
| GET | `/api/instances/{id}/replies` | Poll for new replies (long-polling) |

### Instance Management

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances/{id}/observe` | Observe instance state |
| POST | `/api/instances/{id}/interrupt` | Interrupt ongoing inference |

### Settings

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/settings` | Get global settings |
| POST | `/api/settings` | Update global settings |
| GET | `/api/instances/{id}/settings` | Get instance settings (merged) |
| POST | `/api/instances/{id}/settings` | Update instance settings |

### Knowledge & Skills

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances/{id}/knowledge` | Get instance knowledge |
| GET | `/api/instances/{id}/skill` | Get instance skill |
| PUT | `/api/instances/{id}/skill` | Update instance skill |

### Files

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances/{id}/files/list` | List workspace files |
| GET | `/api/instances/{id}/files/read` | Read workspace file |

### Static Files

| Method | Path | Description |
|--------|------|-------------|
| GET | `/serve/{id}/{path}` | Serve file from instance workspace (authenticated) |
| GET | `/public/{id}/apps/{path}` | Serve public file (no auth, `apps/` directory only) |
| ANY | `/proxy/{port}/{path}` | Reverse proxy to localhost port (authenticated) |

## Project Structure

```
alice-engine/
├── engine/              # Core engine crate
│   ├── src/
│   │   ├── core/        # Alice struct, transaction, beat/roll cycle
│   │   ├── action/      # Action execution (script, file I/O, messaging, etc.)
│   │   ├── api/         # HTTP API (routes, auth, state management)
│   │   ├── inference/   # LLM inference protocol (request/response/streaming)
│   │   ├── prompt/      # Prompt assembly and data extraction
│   │   ├── persist/     # Persistence layer (settings, file I/O)
│   │   ├── policy/      # Policy configuration (engine.toml, env config)
│   │   ├── external/    # External system adapters
│   │   └── util/        # Pure utility functions
│   └── templates/       # Prompt templates
├── html-frontend/       # Static HTML frontend
├── route-macro/         # Proc-macro for route path extraction
├── integration/         # End-to-end integration tests
├── defense/guardian/     # Static analysis tooling
└── scripts/             # Deployment scripts
```

## Configuration

Engine behavior is configured through `engine/src/policy/engine.toml`:

- **`[engine]`** — Beat interval, error backoff, disk checks, sandbox settings
- **`[memory]`** — Session block limits, history size, knowledge capacity
- **`[llm]`** — Default model, temperature, max tokens
- **`[llm.providers]`** — Provider name → API endpoint URL mapping
- **`[streaming]`** — Stream polling interval
- **`[file_browse]`** — Binary extensions, hidden directories, max file size
- **`[rpc]`** — Pagination settings, heartbeat timeout

## License

[MIT](LICENSE)

