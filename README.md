# Agent-L: The Terminal LLM Client

<p align="center"><img src="doc/logo.png" width="200" alt="Agent-L Logo">

## Project Overview

**Agent-L** is a high-performance, asynchronous Terminal User Interface (TUI) designed for interacting with local Large Language Models (LLMs) via [Ollama](https://ollama.com/).

Built with Rust, it prioritizes low latency and resource efficiency. Unlike heavy web-based interfaces, this tool is designed for developers and systems engineers who want to interact with models directly from their terminal without sacrificing responsiveness or features.

## Key Features

* **Real-Time Streaming:** Experience instant gratification with token-by-token response streaming. The UI updates immediately as Ollama generates text.
* **Asynchronous Architecture:** Powered by the `tokio` runtime, network I/O is handled in background threads. This ensures the TUI remains buttery smooth at 60 FPS.
* **Robust TUI Experience:** Built using the `ratatui` library, featuring:
    * **Smart Auto-Scrolling:** The view "sticks" to the bottom while the AI is typing but intelligently pauses if you manually scroll up to review history.
    * **Visual Clarity:** Clear visual separators and color-coding distinguish between User prompts and Assistant responses.
    * **Basic Markdown:** Highlights bold text and code snippets for better readability.

## Prerequisites & Installation

1.  **Rust Toolchain:** Version 1.75+ is recommended.
2.  **Ollama:** Ensure the Ollama server is running and you have pulled your desired model (e.g., `ollama pull gemma3:12b`).

### Building from Source

```bash
# Clone the repository
git clone [https://github.com/yourusername/agent-l.git](https://github.com/yourusername/agent-l.git)
cd agent-l

# Build in release mode for best performance
cargo build --release

# Run the binary
./target/release/agent_l
```

## Configuration

Configuration is currently managed in the source:
* **Model Selection:** Update the model string in `src/ollama.rs`.
* **Host Address:** If running Ollama on a remote local IP (e.g., `192.168.86.11`), update the endpoint in `src/ollama.rs`.

## Controls & Usage

| Key / Action | Description |
| :--- | :--- |
| **Enter** | Send your current prompt to Ollama. |
| **Up / Down Arrow** | Manually scroll through the chat history. |
| **Backspace** | Edit your current prompt. |
| **Ctrl + Q** | Safely exit the application. |

## Roadmap

Future developments include local config file support, multi-model selection menus, and scrollback buffering.

Please see [ROADMAP.md](doc/ROADMAP.md) for the full feature backlog.