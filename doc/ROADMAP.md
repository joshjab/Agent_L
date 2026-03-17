# Agent-L Roadmap

## Vision

Enable small local models (7B–14B, running on consumer GPUs) to reliably perform agentic tasks by breaking work into small, focused stages with strict guardrails at every boundary. The thesis: a well-orchestrated chain of 7B models that never drift beats a single 70B model that does.

---

## Architecture: Frontend → Intent → Specialist

The core design is a three-layer pipeline. No agent is allowed to call another agent directly; all communication flows through structured data, validated at every boundary.

```
┌──────────────────────────────────────────────────────┐
│  USER INPUT                                          │
└──────────────────┬───────────────────────────────────┘
                   │
                   ▼
┌──────────────────────────────────────────────────────┐
│  PERSONA LAYER  (Frontend Agent)                     │
│  • Maintains personality / tone system prompt        │
│  • Compresses old turns to prevent context drift     │
│  • Injects relevant episodic memory via retrieval    │
│  • Final response formatting and streaming to UI     │
│  Model: conversational (gemma3, llama3)              │
└──────────────────┬───────────────────────────────────┘
                   │ compressed context + enriched prompt
                   ▼
┌──────────────────────────────────────────────────────┐
│  INTENT ROUTER  (Intent Agent)                       │
│  • Classifies request → structured JSON only         │
│  • Output: { intent: Enum, params: {…}, agent: Enum }│
│  • Uses last 3–5 turns only (small context)          │
│  • Re-prompts with error on invalid output (max 3x)  │
│  Model: fast/small (mistral, qwen)                   │
└──────────────────┬───────────────────────────────────┘
                   │ typed routing decision
                   ▼
┌──────────────────────────────────────────────────────┐
│  SPECIALISTS  (one per task domain)                  │
│  Each specialist:                                    │
│  • Gets task-specific context only (no full history) │
│  • Has a fixed, declared tool list — no exceptions   │
│  • Uses ReAct loop: Thought → Action → Observation   │
│  • Max N steps; hard stop + fallback at limit        │
│  • Outputs structured result, validated before use   │
│                                                      │
│  chat     — direct answer, no tools                  │
│  code      — code generation, explanation, review    │
│  search    — web or local file retrieval             │
│  shell     — sandboxed command execution             │
│  calendar  — date/time and scheduling tasks          │
│  memory    — explicit memory read/write operations   │
└──────────────────┬───────────────────────────────────┘
                   │ validated structured result
                   ▼
         back to Persona Layer for synthesis
```

### Why this prevents drift

- **Constrained outputs at every boundary** — agents communicate via JSON schemas, not prose. Invalid tokens are masked via GBNF grammar sampling (Ollama supports this natively). A model cannot hallucinate a field that isn't in the schema.
- **Small context per agent** — each agent sees only what it needs. The intent router does not see the full conversation. Specialists do not see other specialists' outputs. This prevents compounding errors.
- **Retry with error feedback** — if an agent produces invalid output, the error is fed back into its next prompt (max 3 retries, then fallback). Small models respond well to explicit error correction.
- **Hard step limits** — specialists using the ReAct loop have a maximum step count (configurable, default 10). Beyond this a circuit breaker fires, returning a structured error to the persona layer.
- **Conversation compression** — the persona layer periodically summarizes old turns (every N tokens) so the context window never fills with stale content that causes behavioral drift.

### Guardrails summary (from 2024–2025 research)

| Problem | Mitigation |
|---|---|
| Hallucinated tool calls | GBNF grammar sampling; validate args before execution |
| Inter-agent misalignment | Typed boundaries; no free-text between agents |
| Context drift over time | Conversation compression; episodic memory; goal reminders |
| Compounding errors | Validate output at every boundary; don't pass prose between agents |
| Prompt injection | Strict input delimiters; semantic validation of tool args |
| Runaway loops | Hard step limit + circuit breaker per specialist |

---

## Memory Architecture

Three layers, modeled on cognitive memory research:

```
Working Memory       — current context window (what the model sees right now)
Episodic Memory      — SQLite log: turns, tool calls, corrections, timestamps
Semantic Memory      — consolidated facts: "user prefers X", "project Y uses Z"
```

On each turn, the persona layer:
1. Queries semantic memory for relevant facts (BM25 + embedding search)
2. Pages those facts into the working memory prompt
3. Appends the current turn to episodic memory
4. Runs a consolidation pass: if 3+ episodes agree on a preference, promote to semantic

This gives the assistant persistent knowledge of the user across sessions without blowing the context window.

---

## Milestones

### M1 — Structured Output & Agent Skeleton *(foundation)*
- [ ] Add `agents/` module with a common `Agent` trait (`prompt`, `parse`, `retry`)
- [ ] Implement GBNF / JSON schema enforcement for all non-streaming calls (Ollama `format` field)
- [ ] Add serde-based schema validation at agent output boundaries
- [ ] Retry logic: re-prompt with structured error on parse failure (max 3 attempts)
- [ ] Unit tests for schema validation and retry logic (no Ollama needed)

### M2 — Intent Router
- [ ] Define `Intent` enum: `Chat`, `Code`, `Search`, `Shell`, `Calendar`, `Memory`, `Unknown`
- [ ] Implement intent classifier agent with constrained JSON output
- [ ] Wire intent result into `App` state; update `AppEvent` to carry routing decision
- [ ] Add routing display in UI (show which specialist was invoked)
- [ ] Integration tests with wiremock for classification scenarios

### M3 — Persona Layer
- [ ] Extract personality / system prompt into `agents/persona.rs`
- [ ] Implement conversation compression: summarize turns beyond a token threshold
- [ ] Goal-reminder injection: prepend abbreviated goal every N turns
- [ ] Persona layer wraps all outbound prompts and all inbound specialist results
- [ ] Config: `PERSONA_SYSTEM_PROMPT` env var (falls back to built-in default)

### M4 — First Specialist: Chat (direct answer)
- [ ] Chat specialist is the simplest: no tools, streams tokens directly to UI
- [ ] Establish the full pipeline end-to-end: Persona → Intent → Specialist → Persona
- [ ] Verify the routing + response cycle works in integration tests

### M5 — Tool Call Infrastructure
- [ ] Define `Tool` trait: `name`, `description`, `schema` (JSON schema for args), `execute`
- [ ] Implement tool call parsing from model output (structured + XML fallback)
- [ ] Arg schema validation before execution
- [ ] ReAct loop: `Thought → ToolCall → Observation → …` with hard step limit
- [ ] Add `AppEvent::ToolCall` and `AppEvent::ToolResult` for UI visibility

### M6 — Specialist: Code
- [ ] Code generation, explanation, and review
- [ ] Tools: syntax highlight, run snippet in sandbox, write to file
- [ ] Output formatting: fenced code blocks in ratatui with language label
- [ ] Tests: mock tool executor, verify ReAct loop terminates correctly

### M7 — Specialist: Search
- [ ] Web search tool (DuckDuckGo API or similar, no API key required)
- [ ] Local file search tool (grep / ripgrep wrapper)
- [ ] Citation output: specialist returns structured results, persona formats prose
- [ ] Tests: wiremock for search HTTP responses

### M8 — Specialist: Shell
- [ ] Sandboxed command execution (no network, no writes outside working dir by default)
- [ ] Explicit confirmation required before execution (safety gate in UI)
- [ ] Output capture and streaming back to persona layer
- [ ] Configurable allow/deny lists for commands

### M9 — Memory System
- [ ] Episodic memory: SQLite log of all turns + tool calls
- [ ] Semantic memory: key-value store for consolidated user facts/preferences
- [ ] Retrieval: BM25 keyword search + optional embedding-based semantic search
- [ ] Memory specialist: explicit read/write/forget operations the user can invoke
- [ ] Consolidation job: promote episodic patterns → semantic memory

### M10 — Polish & Observability
- [ ] Agent trace view in TUI: show routing decisions, tool calls, step count
- [ ] Token budget display per agent call
- [ ] Configurable model per agent role (e.g., fast model for intent, larger for code)
- [ ] Graceful degradation: if specialist fails after retries, fall back to plain chat

---

## Minor Fixes / Features

- [ ] Fix generated logo transparency
- [ ] Fix auto-scrolling (currently broken during streaming)
- [ ] Paste support in input box (bracketed paste mode)
- [ ] Scrollable input for multi-line prompts
- [ ] Config file support (TOML) as alternative to env vars

---

## Model Recommendations (local, Ollama-compatible)

| Role | Recommended | Why |
|---|---|---|
| Persona / Chat | `gemma3:12b`, `llama3.1:8b` | Good instruction following, conversational |
| Intent Router | `qwen2.5:7b`, `mistral:7b` | Fast, strong structured output |
| Code Specialist | `qwen2.5-coder:7b`, `deepseek-coder-v2` | Fine-tuned for code |
| General Specialist | `qwen3:14b` | Best tool calling at this size (0.971 F1) |

---

## References

Architecture patterns drawn from:
- ReAct (Yao et al., 2022) — interleaved reasoning and acting
- Reflexion (Shinn et al., 2023) — self-critique via linguistic reflection
- MAST failure taxonomy (2025) — 14 identified failure modes in multi-agent systems
- Agent Drift study (arxiv 2601.04170) — behavioral degradation quantified; compression mitigates it
- Constrained decoding for agents (GBNF, Ollama structured outputs, SGLang NeurIPS 2024)
- LangGraph hierarchical agent teams pattern — ported conceptually to Rust state machine
