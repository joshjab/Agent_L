//! Live integration tests for Agent-L.
//!
//! All tests require Ollama running locally and are `#[ignore]` by default
//! so that `cargo test` stays fast and CI does not need Ollama.
//!
//! ## Run all live tests
//!
//! ```bash
//! cargo test --test live -- --ignored --nocapture
//! ```
//!
//! ## Run a specific category
//!
//! ```bash
//! cargo test --test live live_pipeline:: -- --ignored --nocapture
//! cargo test --test live live_factual_review:: -- --ignored --nocapture
//! cargo test --test live live_synthesis_review:: -- --ignored --nocapture
//! ```
//!
//! ## Run a single test
//!
//! ```bash
//! cargo test --test live live_pipeline::live_conversational_produces_response -- --ignored --nocapture
//! ```
//!
//! ## Prerequisites
//! - Ollama running at `OLLAMA_HOST:OLLAMA_PORT` (defaults: `localhost:11434`)
//! - The configured model pulled locally (`OLLAMA_MODEL`, default: `llama3.2`)
//! - `TAVILY_API_KEY` set (for factual and synthesis tests)

#[path = "live/live_factual_review.rs"]
mod live_factual_review;

#[path = "live/live_pipeline.rs"]
mod live_pipeline;

#[path = "live/live_synthesis_review.rs"]
mod live_synthesis_review;
