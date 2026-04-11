# Test Case Catalogue

A human-readable record of the live integration tests in `tests/live/live_pipeline.rs`.
Each entry describes the input, what the test asserts, and why.

Add a new entry here whenever a prompt regression is discovered and a new test
is added to prevent it from regressing again.

---

## Conversational — basic Chat routing

**Test:** `live_conversational_produces_response`

**Input:** `"say the word 'hello' and nothing else"`

**Asserts:** response is non-empty and contains `"hello"` (case-insensitive)

**Why:** Verifies the Chat specialist returns at all and that the model follows
a simple instruction. If this fails, the Ollama connection or Chat streaming
is broken.

---

## Factual — Search routing and citation

**Test:** `live_factual_returns_cited_answer`

**Input:** `"what country is Paris the capital of? reply in one word"`

**Asserts:** response contains `"france"` (case-insensitive)

**Why:** Verifies that Factual queries reach the Search specialist, that the
ReAct loop calls web_search, and that the model returns a sensible answer.
Uses a question with a known, stable answer to avoid flakiness.

---

## Regression — no duplicate sentences in Search answers

**Test:** `live_search_does_not_duplicate_sentences`

**Input:** `"in one sentence, what is the capital of France?"`

**Asserts:** no sentence appears more than once in the response (split on `". "`)

**Why:** Caught during M7 manual validation — the model was echoing the DDG
snippet verbatim and then restating the same fact, producing responses like
*"Paris is the capital of France. Paris is the capital of France."*.
Fixed in M7.5 by changing the observation format and adding `deduplicate_sentences`.

---

## Agent L routing — math is Conversational, not Factual

**Test:** `live_agent_l_routes_math_to_conversational`

**Input (to orchestrator):** `"what is 2+2?"`

**Asserts:** `intent_type` is `Conversational` or `Creative` (not `Factual` or `Task`)

**Why:** Math questions are answerable from model knowledge — they must not
route to Search (which calls DDG and adds latency for no benefit). If this
fails, the orchestrator system prompt may be over-classifying as Factual.

---

## Regression — file search routes to Search, not Code

**Test:** `live_file_search_routes_to_search_not_code`

**Input (to orchestrator):** `"search my project files for 'retry'"`

**Asserts:** first plan step has `agent == Search`

**Why:** Caught during M7 manual validation — the query triggered the Code
specialist (keyword `"project"`) instead of the Search specialist with
`local_search`. Fixed in M7.5 by updating the orchestrator system prompt to
explicitly map file-search queries to Search.
