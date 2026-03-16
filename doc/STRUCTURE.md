# Repository Structure

## Overview

Agent L is a high-performance asynchronous terminal UI (TUI) for interacting with local LLMs via the Ollama API. Built in Rust using `ratatui` and `tokio`, it streams real-time token-by-token responses into a chat interface.

---

## Directory Layout

```
Agent_L/
├── Cargo.toml          # Rust project manifest and dependencies
├── Cargo.lock          # Locked dependency versions
├── .gitignore          # Ignores /target and .env
├── README.md           # Project overview and usage instructions
├── doc/
│   ├── ROADMAP.md      # Planned features and known fixes
│   ├── STRUCTURE.md    # This file
│   └── logo.png        # Project logo
└── src/
    ├── main.rs         # Entry point and main event loop
    ├── app.rs          # Application state and business logic
    ├── ui.rs           # Terminal UI rendering
    ├── config.rs       # Configuration via environment variables
    └── ollama.rs       # Ollama HTTP API and streaming
```

---

## Source Files

### `src/main.rs` — Entry Point

The async entry point. Initializes the ratatui terminal, creates the `App` instance, and runs the main event loop at ~60 FPS.

**Responsibilities:**
- Terminal setup and teardown
- Render loop (calls `ui.rs` rendering via ratatui)
- Keyboard event polling (`crossterm`)
- Dispatching key events to `App` methods
- Enforcing auto-scroll behavior

**Functions:**

| Function | Description |
|----------|-------------|
| `main() -> io::Result<()>` | Async entry point. Sets up terminal, runs event loop, restores terminal on exit. Handles key events: `Ctrl+Q` (quit), character input, `Backspace`, arrow keys, `Enter`. |

---

### `src/app.rs` — Application State

Defines all application state and the core logic for sending prompts, receiving streamed tokens, and managing scroll position.

**Enums:**

| Enum | Variants | Description |
|------|----------|-------------|
| `Role` | `User`, `Assistant` | Identifies the sender of a chat message |

**Structs:**

| Struct | Fields | Description |
|--------|--------|-------------|
| `ChatMessage` | `role: Role`, `content: String` | A single message in the chat history |
| `App` | See below | Full application state |

**`App` Fields:**

| Field | Type | Description |
|-------|------|-------------|
| `input` | `String` | Current text in the prompt input box |
| `history` | `Vec<ChatMessage>` | All messages in the conversation |
| `scroll_offset` | `u16` | Current vertical scroll position |
| `content_height` | `usize` | Estimated total content height in lines |
| `terminal_height` | `u16` | Current terminal viewport height |
| `auto_scroll` | `bool` | Whether the view should stick to the bottom |
| `is_loading` | `bool` | True while waiting for the first token from Ollama |
| `model_name` | `String` | Name of the currently active LLM model |
| `token_count` | `usize` | Total number of tokens received this session |
| `exit` | `bool` | Signals the main loop to terminate |
| `tx` | `mpsc::UnboundedSender<String>` | Sends streamed tokens from async task to UI |
| `rx` | `mpsc::UnboundedReceiver<String>` | Receives streamed tokens in the main loop |

**`App` Methods:**

| Method | Description |
|--------|-------------|
| `new() -> Self` | Constructor. Creates the MPSC channel, loads config, and initializes default state. |
| `ask_ollama(&mut self)` | Validates input, appends a `User` message and an empty `Assistant` placeholder to history, clears the input field, sets `is_loading`, then spawns an async task that calls `fetch_ollama_stream`. |
| `update(&mut self)` | Called each frame. Drains the token channel and appends received tokens to the last `Assistant` message. Clears `is_loading` on first token. Increments `token_count`. |
| `recalculate_scroll(&mut self)` | Estimates total wrapped line count across all messages (assumes 50-char width) and sets `content_height`. |
| `enforce_auto_scroll(&mut self, total_lines: usize, viewport_height: u16)` | Updates `content_height` and `terminal_height`. If `auto_scroll` is enabled, sets `scroll_offset` to the maximum to keep the view at the bottom. |
| `scroll_to_bottom(&mut self)` | Computes and sets `scroll_offset` so the last line of content is visible. |

---

### `src/ui.rs` — Terminal UI Rendering

Implements ratatui's `Widget` trait for `App` to render the full TUI.

**Trait Implementations:**

| Trait | For | Description |
|-------|-----|-------------|
| `Widget` | `&App` | Enables ratatui to call `render()` on the app directly |

**`render()` Layout:**

The terminal is split into three vertical sections:

1. **Chat area** (flexible height) — Scrollable message history
2. **Prompt input** (3 lines fixed) — Current user input with `> ` prefix, cyan border
3. **Status bar** (1 line fixed) — Model name, token count, and `[Ctrl+Q] Quit` hint

**Chat rendering details:**
- Each message is preceded by a horizontal separator
- User messages are colored **blue** with a `"You:"` prefix
- Assistant messages are colored **magenta** with an `"Ollama:"` prefix
- Assistant message content is passed through `parse_simple_markdown()`
- The outer block has a rounded border and a centered `"🦙 Agent L"` title in yellow/bold

**Functions:**

| Function | Description |
|----------|-------------|
| `render(self, area: Rect, buf: &mut Buffer)` | Core rendering method. Builds the layout, styles all widgets, and draws them into the ratatui buffer. |
| `parse_simple_markdown(text: &str) -> Vec<Line<'_>>` | Minimal Markdown parser. Splits text on `**` delimiters and applies yellow bold styling to enclosed sections. Returns a `Vec<Line>` of styled spans for ratatui. |

---

### `src/config.rs` — Configuration

Loads runtime configuration from environment variables or a `.env` file.

**Structs:**

| Struct | Fields | Description |
|--------|--------|-------------|
| `Config` | `ollama_url: String`, `model_name: String` | Holds the resolved Ollama API endpoint and model name |

**`Config` Methods:**

| Method | Description |
|--------|-------------|
| `from_env() -> Self` | Loads `.env` via `dotenvy`. Reads `OLLAMA_HOST` (default `127.0.0.1`), `OLLAMA_PORT` (default `11434`), and `OLLAMA_MODEL` (default `llama3`). Constructs `ollama_url` as `http://{host}:{port}/api/generate`. |

---

### `src/ollama.rs` — Ollama API Client

Handles HTTP communication with the Ollama server, including streaming response processing.

**Functions:**

| Function | Description |
|----------|-------------|
| `fetch_ollama_stream(prompt: &str, tx: UnboundedSender<String>) -> Result<(), Box<dyn Error>>` | Sends a POST request to the Ollama `/api/generate` endpoint with `stream: true`. Reads the response as a byte stream, deserializes each newline-delimited JSON chunk, extracts the `response` token string, and sends it through the `tx` channel. Sends error strings through the channel on HTTP or parse failures. |

---

## Data Flow

```
User keystroke (Enter)
        │
        ▼
App::ask_ollama()
  ├── Appends User message to history
  ├── Appends empty Assistant placeholder
  └── Spawns tokio task ──► fetch_ollama_stream()
                                    │
                              Streams tokens via
                              MPSC channel (tx)
                                    │
Main loop ◄─────────────── App::update() drains rx
        │
        ▼
ratatui render (60 FPS)
  └── ui::render() draws chat history + input + status bar
```

---

## Dependencies (`Cargo.toml`)

| Crate | Version | Purpose |
|-------|---------|---------|
| `ratatui` | 0.30.0 | Terminal UI framework |
| `crossterm` | 0.29.0 | Cross-platform terminal input/output |
| `tokio` | 1.x | Async runtime (full features) |
| `reqwest` | 0.13.2 | HTTP client with JSON and streaming support |
| `serde` / `serde_json` | 1.x | JSON serialization/deserialization |
| `futures-util` | 0.3.32 | Async stream utilities |
| `dotenvy` | 0.15.7 | `.env` file loading |

---

## Configuration

Copy or create a `.env` file in the project root (it is gitignored):

```env
OLLAMA_HOST=127.0.0.1
OLLAMA_PORT=11434
OLLAMA_MODEL=llama3
```

All three variables have the defaults shown above, so the file is optional if running Ollama locally with `llama3`.
