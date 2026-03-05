# Alice Engine

A self-evolving AI agent engine. Each agent gets its own workspace, memory, and tools — powered by any OpenAI-compatible LLM.

## Quick Start

### Option 1: Download & Run

Download the latest binary from [Releases](https://github.com/anthropic/alice-engine/releases), then:

```bash
chmod +x alice-engine
./alice-engine
```

Open http://localhost:8081 in your browser. The setup page will guide you through configuration.

### Option 2: Build from Source

```bash
git clone https://github.com/anthropic/alice-engine.git
cd alice-engine
cargo build --release
./target/release/alice-engine
```

Open http://localhost:8081 — the setup page will ask for your API key and model.

## Configuration

On first launch, Alice shows a setup page where you set:

- **API Key** — your LLM provider API key
- **Model** — choose a provider and model (e.g. `anthropic/claude-sonnet-4`)

That's it. You're ready to create your first agent.

### Environment Variables (Optional)

For production deployments, configure via environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `ALICE_AUTH_SECRET` | Login password (skip if unset) | No password |
| `ALICE_DEFAULT_API_KEY` | Default API key for new instances | — |
| `ALICE_DEFAULT_MODEL` | Default model (e.g. `openrouter@anthropic/claude-sonnet-4`) | `openrouter@anthropic/claude-opus-4.6` |
| `ALICE_PORT` | HTTP port | `8081` |
| `ALICE_BASE_DIR` | Data directory | `.` (current dir) |
| `ALICE_USER_ID` | Owner user ID | `default` |
| `ALICE_HOST` | Hostname for display | — |
| `ALICE_SKIP_AUTH` | Skip authentication (`true`/`false`) | `false` |

Model format: `provider@model_id`. Built-in providers: `openrouter`, `openai`, `zenmux`. Use a full URL as provider for custom endpoints.

## How It Works

Each agent instance has:
- **Inbox/Outbox** — communicate via messages
- **Workspace** — read/write files, run scripts
- **Memory** — knowledge, history, session context (auto-managed)
- **Skills** — injectable prompt knowledge

The engine runs a beat loop: check messages → invoke LLM → execute actions → repeat.

## API Reference

All endpoints under `/api/`. Authentication via session cookie (set `ALICE_AUTH_SECRET` to enable).

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
| GET | `/api/instances/{id}/messages` | Get messages (paginated) |
| POST | `/api/instances/{id}/messages` | Send message to instance |
| GET | `/api/instances/{id}/replies` | Poll for new replies |

### Instance Management

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/instances/{id}/observe` | Observe instance state |
| POST | `/api/instances/{id}/interrupt` | Interrupt current inference |
| GET | `/api/instances/{id}/files/list` | List workspace files |
| GET | `/api/instances/{id}/files/read` | Read a workspace file |
| GET | `/api/instances/{id}/knowledge` | Get instance knowledge |
| GET | `/api/instances/{id}/skill` | Get instance skill |
| PUT | `/api/instances/{id}/skill` | Update instance skill |

### Settings

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/settings` | Get global settings |
| POST | `/api/settings` | Update global settings |
| GET | `/api/instances/{id}/settings` | Get instance settings |
| POST | `/api/instances/{id}/settings` | Update instance settings |

### Static Files

| Path | Description |
|------|-------------|
| `/serve/{id}/{path}` | Serve files from instance workspace |
| `/public/{id}/{path}` | Public files (no auth required) |

## Development

### Project Structure

```
engine/          — Core engine (Rust, axum HTTP server)
html-frontend/   — Web UI (static HTML/JS)
route-macro/     — Proc-macro for route annotations
integration/     — End-to-end tests (Playwright + mock LLM)
defense/         — Code quality tools (guardian, leak-detector)
scripts/         — Build & deploy scripts
```

### Building

```bash
cargo build --release
```

### Testing

```bash
# Unit tests
cargo test

# End-to-end tests (requires Node.js + Playwright)
cd integration && npm test
```

### Guardian (Code Quality)

Static analysis tool that enforces literal placement rules:

```bash
python3 defense/guardian/guardian.py engine/src
```

## License

MIT — see [LICENSE](LICENSE).