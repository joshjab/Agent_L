# Live Integration Tests

These tests run the full Agent-L pipeline against a real Ollama instance. They
are `#[ignore]` by default so `cargo test` stays fast and CI does not require
Ollama.

## Prerequisites

1. Ollama installed and running: <https://ollama.ai>
2. A model pulled locally (default: `llama3.2`):
   ```bash
   ollama pull llama3.2
   ```

## Running

```bash
# Run all live tests with output visible
cargo test --test live_pipeline -- --ignored --nocapture

# Run a single test
cargo test --test live_pipeline live_conversational_produces_response -- --ignored --nocapture
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_HOST` | `localhost` | Ollama server hostname |
| `OLLAMA_PORT` | `11434` | Ollama server port |
| `OLLAMA_MODEL` | `llama3.2` | Model name to use |

## Test catalogue

See `doc/test-cases.md` for a full description of what each test asserts and
why. When a new prompt regression is found, add a test here and document it
there.

## Debugging failures

Run with `--nocapture` to see the actual model response:

```bash
cargo test --test live_pipeline -- --ignored --nocapture 2>&1 | grep -A5 "FAILED"
```

If a test fails, read the output to see the actual response before editing the
prompt. Changes to prompts should be driven by observed model behaviour, not
guesswork.
