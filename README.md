# Alice Engine

A self-evolving AI agent engine. Each agent gets its own workspace, memory, and tools — powered by any OpenAI-compatible LLM.

## Quick Start

### One-Line Install & Run

```bash
curl -fsSL https://raw.githubusercontent.com/luofenggu/alice-engine/main/start.sh | bash
```

This will download the binary to `~/.alice/`, start the engine, and open your browser. Run the same command again to launch — it won't re-download unless there's an update.

On **macOS**, you can also save `start.sh` as `Alice.command` and double-click it.

### Manual Download

Download the binary for your platform:

| Platform | Download |
|----------|----------|
| Linux x86_64 | [alice-engine-linux-x86_64](https://github.com/luofenggu/alice-engine/releases/latest/download/alice-engine-linux-x86_64) |
| macOS Apple Silicon | [alice-engine-macos-arm64](https://github.com/luofenggu/alice-engine/releases/latest/download/alice-engine-macos-arm64) |
| macOS Intel | [alice-engine-macos-x86_64](https://github.com/luofenggu/alice-engine/releases/latest/download/alice-engine-macos-x86_64) |

Then:

```bash
chmod +x alice-engine-*
./alice-engine-*
```

Open http://127.0.0.1:8081 — the setup wizard will guide you through configuration.

### Build from Source

```bash
git clone https://github.com/luofenggu/alice-engine.git
cd alice-engine
cargo build --release
./target/release/alice-engine
```

## First Launch

On first launch, the setup wizard asks for:
- **API Key** — from any OpenAI-compatible provider
- **Model** — in `provider@model_id` format (e.g. `openrouter@anthropic/claude-sonnet-4`)

Built-in providers: `openrouter`, `openai`. For custom endpoints, use a full URL:
```
https://your-api-server.com/v1/chat/completions@model-name
```

After setup, create your first agent instance and start chatting.

## Settings

Settings can be configured at two levels:

- **Global Settings** — click ⚙️ in the sidebar. Applies to all instances as defaults.
- **Instance Settings** — click ⚙️ on an instance. Overrides global settings for that instance.

Settings follow a three-layer inheritance: **Environment Variables → Global Settings → Instance Settings**. Each layer only overrides what it explicitly sets.

### Channel Rotation

Configure multiple LLM channels for automatic failover:

- **Primary Channel** — your main API key + model
- **Extra Channels** — backup channels (Extra 1, Extra 2, ...)

When a channel fails (e.g. rate limit, quota exceeded), the engine automatically rotates to the next channel with exponential backoff. This keeps your agents running even when individual API keys hit limits.

## Cloud Deployment

For running on a server, set a password to protect access:

```bash
AUTH_SECRET=your-password ./alice-engine
```

Then visit `http://your-server-ip:8081` and log in with your password.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AUTH_SECRET` | Login password | No password (open access) |
| `ALICE_DEFAULT_API_KEY` | Default LLM API key | — |
| `ALICE_DEFAULT_MODEL` | Default model | `openrouter@anthropic/claude-sonnet-4` |
| `ALICE_HTTP_PORT` | HTTP port | `8081` |
| `ALICE_BASE_DIR` | Data directory | `.` (current dir) |
| `ALICE_USER_ID` | Owner user ID | `default` |
| `ALICE_HOST` | Public hostname (for display) | — |

## How It Works

Each agent instance has:
- **Inbox/Outbox** — communicate via messages
- **Workspace** — read/write files, run scripts
- **Memory** — knowledge, history, session context (auto-managed)
- **Skills** — injectable prompt knowledge

The engine runs a beat loop: check messages → invoke LLM → execute actions → repeat.

Agents can:
- Read and write files in their workspace
- Execute shell scripts
- Send messages to users and other agents
- Create new agent instances (fission)
- Serve static files and run local services
- Manage their own knowledge and memory

## API Reference

All endpoints under `/api/`. Set `AUTH_SECRET` to enable authentication via session cookie.

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
| GET | `/api/instances/{id}/messages` | Get messages (query: `before_id`, `limit`) |
| POST | `/api/instances/{id}/messages` | Send message |
| GET | `/api/instances/{id}/replies` | Poll new messages (query: `after_id`) |

### Instance Management

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances/{id}/observe` | Observe instance state |
| POST | `/api/instances/{id}/interrupt` | Interrupt current inference |
| GET | `/api/instances/{id}/files/list` | List workspace files |
| GET | `/api/instances/{id}/files/read` | Read workspace file (query: `path`) |
| GET | `/api/instances/{id}/knowledge` | Get instance knowledge |
| GET | `/api/instances/{id}/skill` | Get skill |
| PUT | `/api/instances/{id}/skill` | Update skill |

### Settings

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/settings` | Get global settings |
| POST | `/api/settings` | Update global settings |
| GET | `/api/instances/{id}/settings` | Get instance settings |
| POST | `/api/instances/{id}/settings` | Update instance settings |

### Static Files & Proxy

| Path | Description |
|------|-------------|
| `/serve/{id}/{path}` | Serve workspace files (auth required) |
| `/public/{id}/apps/{path}` | Public files (no auth) |
| `/proxy/{port}/{path}` | Reverse proxy to localhost port |

## Development

### Project Structure

```
engine/              Core engine (Rust, axum HTTP server)
  src/api/           HTTP API layer
  src/core/          Agent lifecycle (beat/roll)
  src/persist/       Data persistence (SQLite)
  src/inference/     LLM integration
  src/action/        Action execution
  src/policy/        Configuration & defaults
  src/external/      External system adapters
  route-macro/       Proc-macro for route annotations
  templates/         Prompt templates
html-frontend/       Web UI (static HTML/JS)
integration/         E2E tests (Playwright + mock LLM)
defense/guardian/     Static analysis (literal placement rules)
```

### Testing

```bash
# Unit tests
cargo test

# Guardian (static analysis)
python3 defense/guardian/guardian.py engine/src

# E2E tests (requires Node.js + Playwright)
cd integration && npm test
```

## License

MIT — see [LICENSE](LICENSE).

