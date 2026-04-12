# Agent-L: The Terminal LLM Client

<p align="center"><img src="doc/logo.png" width="200" alt="Agent-L Logo">

## Project Overview

**Agent-L** is a high-performance, asynchronous Terminal User Interface (TUI) for interacting with local Large Language Models via [Ollama](https://ollama.com/).

The long-term goal is to enable small local models running on consumer GPUs to perform agentic tasks — building a "Frontend, Intent, and Specialist" agent framework that breaks work into smaller chunks so lighter models can stay on track. Think of it as a local alternative to tools like [OpenClaw](https://github.com/openclaw/openclaw), built in Rust.

## Key Features

* **Real-Time Streaming:** Token-by-token response streaming with immediate UI updates as Ollama generates text.
* **Asynchronous Architecture:** Powered by the `tokio` runtime — network I/O runs in background tasks so the TUI stays responsive.
* **Startup Health Checks:** On launch, Agent-L connects to Ollama, verifies the configured model is pulled, and polls until it's loaded into memory before allowing input.
* **Three-Layer Agent Pipeline:** Every request flows through Persona → Agent L (orchestrator) → Specialist. Each layer communicates via validated JSON — no free-text between agents.
* **Intent Routing:** Agent L classifies each message (`Conversational`, `Factual`, `Creative`, `Task`) and routes to the right specialist automatically.
* **Search Grounding:** Factual queries always go to the Search specialist (DuckDuckGo + local ripgrep), never to the model's internal knowledge. Responses include source citations.
* **Code Specialist:** Code requests route to a sandboxed `claude` CLI subprocess for one-off code tasks; project-scope tasks show a clear limitation message.
* **Robust TUI:** Built with `ratatui`, featuring:
  * **Smart Auto-Scrolling:** Sticks to the bottom while the AI is typing; pauses if you scroll up to review history.
  * **Visual Clarity:** Color-coded separators distinguish User and Assistant messages.
  * **Markdown & Hyperlinks:** Bold (`**text**`) highlighting; bare `https://` URLs are rendered as OSC 8 clickable links in supported terminals.

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

The test suite covers all modules with inline unit tests and wiremock-based integration tests. No running Ollama instance is required for the fast suite.

```bash
# Fast suite — no Ollama needed
cargo test

# Live suite — requires Ollama running with the configured model
cargo test --test live_pipeline -- --ignored --nocapture
```

## Project Structure

```
src/
  main.rs                     — event loop, keyboard input, terminal I/O
  lib.rs                      — re-exports all modules for integration tests
  app.rs                      — App state, AppEvent enum, StartupState enum
  config.rs                   — configuration (env vars / .env file)
  prompts.rs                  — load prompts from prompts/*.md with fallback to compiled-in defaults
  ollama.rs                   — streaming HTTP client for /api/chat, post_json helper
  startup.rs                  — startup health checks (/api/tags, /api/ps)
  ui.rs                       — ratatui rendering, markdown parsing, OSC 8 hyperlinks

  agents/
    mod.rs                    — Agent trait, AgentErrorKind, call_with_retry
    orchestrator.rs           — Agent L: intent classification → TaskPlan
    persona.rs                — Persona layer: system prompt + context compression
    compression.rs            — conversation summarisation (triggered by token budget)
    schema.rs                 — JSON schema validation helpers
    specialists/
      mod.rs                  — run_plan(): executes ordered task plan step by step
      chat.rs                 — ChatSpecialist: conversational/creative, no tools
      code.rs                 — CodeSpecialist: delegates to claude CLI subprocess
      search.rs               — SearchSpecialist: DuckDuckGo + local ripgrep

  tools/
    mod.rs                    — Tool trait, ToolRegistry
    executor.rs               — ReAct loop: Thought → ToolCall → Observation; circuit breaker
    search_tools.rs           — web_search (DuckDuckGo) + local_search (ripgrep)
    claude_code.rs            — claude CLI subprocess runner (used by CodeSpecialist)

tests/
  ollama_integration.rs       — wiremock tests for the Ollama HTTP client
  startup_integration.rs      — wiremock tests for startup check sequences
  orchestrator_integration.rs — wiremock tests for intent classification + plan validation
  pipeline_integration.rs     — end-to-end: Persona → Agent L → Specialist
  search_integration.rs       — wiremock tests for DuckDuckGo responses and citation format
  live/
    live_pipeline.rs          — live tests against a real Ollama instance (#[ignore] by default)
```

## Prompt Customization

Every LLM system prompt lives in the `prompts/` directory as a plain Markdown file. You can edit these without recompiling.

| File | Controls |
| :--- | :--- |
| `prompts/orchestrator.md` | Intent classification and agent routing rules |
| `prompts/persona.md` | Assistant personality and tone |
| `prompts/persona_goal_reminder.md` | Short reminder injected every 10 turns |
| `prompts/search.md` | Search specialist behavior and ReAct format (`{now}` is replaced with the current UTC time at runtime) |
| `prompts/code_scope.md` | One-off vs. project scope classification |

To override the directory (e.g., store prompts in your config folder or an Obsidian vault):

```env
AGENT_L_PROMPTS_DIR=/path/to/my/prompts
```

If a file is missing or unreadable, Agent-L falls back to the compiled-in default — the binary always works standalone.

## Roadmap

See [ROADMAP.md](doc/ROADMAP.md) for the full backlog. Upcoming work includes:

- Shell specialist with sandboxed command execution and confirmation gate (M8)
- Persistent memory across sessions — episodic + semantic stores (M9)
- Per-agent model configuration via TOML (M10)
