# Agent-L: The Terminal LLM Client

<p align="center"><img src="doc/logo.png" width="200" alt="Agent-L Logo">

## Project Overview

**Agent-L** is a high-performance, asynchronous Terminal User Interface (TUI) for interacting with local Large Language Models via [Ollama](https://ollama.com/).

The long-term goal is to enable small local models running on consumer GPUs to perform agentic tasks — building a "Frontend, Intent, and Specialist" agent framework that breaks work into smaller chunks so lighter models can stay on track. Think of it as a local alternative to tools like [OpenClaw](https://github.com/openclaw/openclaw), built in Rust.

## Key Features

* **Real-Time Streaming:** Token-by-token response streaming with immediate UI updates as Ollama generates text.
* **Asynchronous Architecture:** Powered by the `tokio` runtime — network I/O runs in background tasks so the TUI stays responsive.
* **Startup Health Checks:** On launch, Agent-L connects to Ollama, verifies the configured model is pulled, and polls until it's loaded into memory before allowing input.
* **Robust TUI:** Built with `ratatui`, featuring:
  * **Smart Auto-Scrolling:** Sticks to the bottom while the AI is typing; pauses if you scroll up to review history.
  * **Visual Clarity:** Color-coded separators distinguish User and Assistant messages.
  * **Basic Markdown:** Bold (`**text**`) is highlighted for readability.

## Prerequisites

1. **Rust Toolchain:** Edition 2024 (Rust 1.85+).
2. **Ollama:** Running locally with your desired model pulled — e.g., `ollama pull gemma3`.

## Building & Running

```bash
git clone https://github.com/joshjab/agent_l.git
cd agent_l

cargo build --release
./target/release/agent_l
```

## Configuration

Create a `.env` file in the project root (it is `.gitignore`d). All three variables are optional and fall back to the defaults shown:

```env
OLLAMA_HOST=127.0.0.1   # default
OLLAMA_PORT=11434        # default
OLLAMA_MODEL=llama3      # default
```

Example — using a non-standard port with a specific model:

```env
OLLAMA_PORT=7869
OLLAMA_MODEL=gemma3
```

## Controls

| Key | Action |
| :--- | :--- |
| **Enter** | Send prompt to Ollama |
| **Up / Down Arrow** | Scroll through chat history |
| **Backspace** | Edit current prompt |
| **Ctrl + Q** | Exit |

## Testing

The test suite covers all modules with inline unit tests and wiremock-based integration tests. No running Ollama instance is required.

```bash
cargo test
```

## Project Structure

```
src/
  main.rs       — event loop, input handling
  app.rs        — application state and update logic
  config.rs     — configuration (env vars / .env file)
  ollama.rs     — streaming HTTP client for /api/chat
  startup.rs    — startup health checks (/api/tags, /api/ps)
  ui.rs         — ratatui rendering and markdown parsing
tests/
  ollama_integration.rs   — wiremock tests for the Ollama HTTP client
  startup_integration.rs  — wiremock tests for startup check sequences
```

## Roadmap

See [ROADMAP.md](doc/ROADMAP.md) for the full backlog. Upcoming work includes:

- Agent system prompts (Frontend / Intent / Specialist roles)
- Agent and sub-agent orchestration framework
- Tool call support
