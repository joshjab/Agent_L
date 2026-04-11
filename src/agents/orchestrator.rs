use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::{Agent, schema::ParseError};

/// Remove a leading ` ```json ` / ` ``` ` code fence and trailing ` ``` ` if
/// present. Some models (e.g. gemma4) wrap their JSON output in markdown code
/// fences even when instructed to return raw JSON.
fn strip_code_fence(s: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = s.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start());
    match inner {
        Some(rest) => {
            let stripped = rest.strip_suffix("```").unwrap_or(rest).trim();
            std::borrow::Cow::Owned(stripped.to_string())
        }
        None => std::borrow::Cow::Borrowed(s),
    }
}

/// The high-level category of what the user wants.
///
/// Agent L classifies every incoming message into one of these buckets before
/// deciding which specialist(s) to invoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum IntentType {
    /// A question about real-world state (facts, current events, prices).
    /// Always routes to Search — never answered from model knowledge alone.
    Factual,
    /// Greetings, opinions, back-and-forth chat.
    Conversational,
    /// Writing, brainstorming, summarizing.
    Creative,
    /// Actions like scheduling, running code, sending email.
    Task,
}

/// The specialist role that can handle a pipeline step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AgentKind {
    Chat,
    Code,
    Search,
    Shell,
    Calendar,
    Memory,
    /// Returned when Agent L cannot determine a suitable specialist.
    Unknown,
}

/// A single step in Agent L's task plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Which specialist to invoke for this step.
    pub agent: AgentKind,
    /// Human-readable description of what this step should accomplish.
    /// Some models use "instruction" — accepted as an alias.
    #[serde(alias = "instruction", default)]
    pub task: String,
    /// Optional index into the parent `steps` array whose output feeds into
    /// this step as additional context. `None` means no dependency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<usize>,
}

/// The structured output produced by Agent L for every user message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPlan {
    pub intent_type: IntentType,
    /// Some models emit "plan" instead of "steps" — accepted as an alias.
    #[serde(alias = "plan")]
    pub steps: Vec<PlanStep>,
}

/// Maximum number of steps Agent L may include in one plan.
pub const MAX_PLAN_STEPS: usize = 5;

impl TaskPlan {
    /// Validate a `TaskPlan` that has already been deserialized.
    ///
    /// Returns `Err(ParseError)` if:
    /// - `steps` is empty
    /// - `steps` exceeds `MAX_PLAN_STEPS`
    /// - any `depends_on` index is out of bounds or is a self-reference
    pub fn validate(&self) -> Result<(), ParseError> {
        if self.steps.is_empty() {
            return Err(ParseError {
                message: "task plan must have at least one step".into(),
            });
        }
        if self.steps.len() > MAX_PLAN_STEPS {
            return Err(ParseError {
                message: format!(
                    "task plan has {} steps but the maximum is {}; please break your request into smaller parts",
                    self.steps.len(),
                    MAX_PLAN_STEPS
                ),
            });
        }
        for (i, step) in self.steps.iter().enumerate() {
            if let Some(dep) = step.depends_on
                && dep >= i
            {
                return Err(ParseError {
                    message: format!(
                        "step {i} has invalid depends_on={dep}; depends_on must refer to an earlier step"
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Number of recent conversation turns fed to Agent L for context.
const N_CONTEXT_TURNS: usize = 5;

/// JSON Schema sent in Ollama's `format` field to enforce the output shape.
fn task_plan_schema() -> Value {
    json!({
        "type": "object",
        "required": ["intent_type", "steps"],
        "properties": {
            "intent_type": {
                "type": "string",
                "enum": ["Factual", "Conversational", "Creative", "Task"]
            },
            "steps": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["agent", "task"],
                    "properties": {
                        "agent": {
                            "type": "string",
                            "enum": ["Chat", "Code", "Search", "Shell", "Calendar", "Memory", "Unknown"]
                        },
                        "task": { "type": "string" },
                        "depends_on": { "type": "integer", "minimum": 0 }
                    }
                }
            }
        }
    })
}

const SYSTEM_PROMPT: &str = "\
You are Agent L, an orchestrator. Given the last few turns of a conversation, \
classify the user's intent and output a task plan as JSON.

intent_type rules:
- Factual: any question about real-world facts, current events, prices, or live data \
  → route to Search (agent=Search); NEVER answer from your own knowledge. \
  Examples of Factual: 'Who is the president?', 'Who is the prime minister of X?', \
  'What is the stock price of Y?', 'What is the latest version of Z?', \
  'What happened in the news today?', 'What is the weather?'. \
  When in doubt, treat as Factual and search — do not guess. \
  This also includes searching, grepping, or finding content within local project files \
  (e.g. 'search my project files for X', 'find uses of function Y', 'grep for pattern Z') \
  → use agent=Search with local_search tool, NOT agent=Code.
- Conversational: greetings, opinions, casual back-and-forth, simple arithmetic → route to Chat
- Creative: prose writing, brainstorming, summarizing NON-CODE content → route to Chat. \
  IMPORTANT: writing or generating CODE is NOT Creative — it is Task with agent=Code
- Task: any request to write, generate, create, or modify code or scripts; \
  running commands; scheduling → route to the relevant specialist. \
  Use agent=Code for ALL code/script generation and modification requests. \
  IMPORTANT: searching/grepping for content in files is Factual (agent=Search), NOT Task.

IMPORTANT: simple arithmetic (2+2, 5*3, percentages) is ALWAYS Conversational, never Factual.

Output exactly one JSON object matching the schema. Max 5 steps. \
Use depends_on (0-indexed) only when a later step needs the output of an earlier step.";

/// The Agent L orchestrator. Feed it recent conversation turns; it returns a `TaskPlan`.
pub struct OrchestratorAgent {
    pub model: String,
}

impl OrchestratorAgent {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

impl Agent for OrchestratorAgent {
    type Output = TaskPlan;

    fn prompt(&self, context: &[Value], error_feedback: Option<&ParseError>) -> Value {
        // Take the last N_CONTEXT_TURNS messages as context
        let recent = if context.len() > N_CONTEXT_TURNS {
            &context[context.len() - N_CONTEXT_TURNS..]
        } else {
            context
        };

        let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];

        messages.extend_from_slice(recent);

        // On a retry, append the parse error so the model can self-correct
        if let Some(err) = error_feedback {
            messages.push(json!({
                "role": "user",
                "content": format!(
                    "Your previous response was invalid. Error: {}. \
                     Please output a valid JSON object matching the schema.",
                    err.message
                )
            }));
        }

        json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "think": false,
            "format": task_plan_schema()
        })
    }

    fn parse(&self, response: &str) -> Result<TaskPlan, ParseError> {
        // Ollama wraps the assistant reply in {"message": {"content": "..."}}
        let envelope: Value = serde_json::from_str(response).map_err(|e| ParseError {
            message: format!("Ollama response is not valid JSON: {e} (raw: {response:?})"),
        })?;

        let content = envelope["message"]["content"]
            .as_str()
            .ok_or_else(|| ParseError {
                message: format!(
                    "Ollama response missing message.content string (got: {envelope})"
                ),
            })?;

        // Some models wrap their JSON output in a ```json ... ``` code fence.
        // Strip it before parsing so the deserializer sees raw JSON.
        let content = strip_code_fence(content);

        let plan: TaskPlan = serde_json::from_str(&content).map_err(|e| ParseError {
            message: format!("task plan JSON is invalid: {e} (content: {content:?})"),
        })?;

        plan.validate()?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn intent_type_serializes_to_pascal_case() {
        assert_eq!(
            serde_json::to_string(&IntentType::Factual).unwrap(),
            r#""Factual""#
        );
        assert_eq!(
            serde_json::to_string(&IntentType::Conversational).unwrap(),
            r#""Conversational""#
        );
    }

    #[test]
    fn intent_type_deserializes_from_pascal_case() {
        let v: IntentType = serde_json::from_value(json!("Creative")).unwrap();
        assert_eq!(v, IntentType::Creative);
    }

    #[test]
    fn intent_type_rejects_unknown_string() {
        let result: Result<IntentType, _> = serde_json::from_value(json!("Bogus"));
        assert!(result.is_err());
    }

    #[test]
    fn agent_kind_serializes_to_pascal_case() {
        assert_eq!(
            serde_json::to_string(&AgentKind::Search).unwrap(),
            r#""Search""#
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Unknown).unwrap(),
            r#""Unknown""#
        );
    }

    #[test]
    fn agent_kind_deserializes_from_pascal_case() {
        let v: AgentKind = serde_json::from_value(json!("Shell")).unwrap();
        assert_eq!(v, AgentKind::Shell);
    }

    // --- TaskPlan / PlanStep ---

    fn single_step(agent: AgentKind) -> TaskPlan {
        TaskPlan {
            intent_type: IntentType::Conversational,
            steps: vec![PlanStep {
                agent,
                task: "do something".into(),
                depends_on: None,
            }],
        }
    }

    #[test]
    fn task_plan_roundtrips_through_json() {
        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![
                PlanStep {
                    agent: AgentKind::Search,
                    task: "find info".into(),
                    depends_on: None,
                },
                PlanStep {
                    agent: AgentKind::Chat,
                    task: "summarise".into(),
                    depends_on: Some(0),
                },
            ],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let restored: TaskPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, restored);
    }

    #[test]
    fn plan_step_omits_depends_on_when_none() {
        let step = PlanStep {
            agent: AgentKind::Chat,
            task: "hi".into(),
            depends_on: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(
            !json.contains("depends_on"),
            "depends_on should be omitted when None"
        );
    }

    #[test]
    fn validate_accepts_valid_single_step_plan() {
        assert!(single_step(AgentKind::Chat).validate().is_ok());
    }

    #[test]
    fn validate_accepts_max_steps_exactly() {
        let steps = (0..MAX_PLAN_STEPS)
            .map(|_| PlanStep {
                agent: AgentKind::Chat,
                task: "x".into(),
                depends_on: None,
            })
            .collect();
        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps,
        };
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_steps() {
        let plan = TaskPlan {
            intent_type: IntentType::Factual,
            steps: vec![],
        };
        let err = plan.validate().unwrap_err();
        assert!(err.message.contains("at least one step"));
    }

    #[test]
    fn validate_rejects_more_than_max_steps() {
        let steps = (0..=MAX_PLAN_STEPS)
            .map(|_| PlanStep {
                agent: AgentKind::Chat,
                task: "x".into(),
                depends_on: None,
            })
            .collect();
        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps,
        };
        let err = plan.validate().unwrap_err();
        assert!(
            err.message.contains("maximum"),
            "error should mention 'maximum'"
        );
        assert!(
            err.message.contains("break"),
            "error should ask user to break request up"
        );
    }

    #[test]
    fn validate_accepts_valid_depends_on() {
        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![
                PlanStep {
                    agent: AgentKind::Search,
                    task: "step 0".into(),
                    depends_on: None,
                },
                PlanStep {
                    agent: AgentKind::Chat,
                    task: "step 1".into(),
                    depends_on: Some(0),
                },
            ],
        };
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn validate_rejects_self_referencing_depends_on() {
        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![PlanStep {
                agent: AgentKind::Chat,
                task: "step 0".into(),
                depends_on: Some(0),
            }],
        };
        let err = plan.validate().unwrap_err();
        assert!(err.message.contains("invalid depends_on"));
    }

    #[test]
    fn validate_rejects_forward_depends_on() {
        let plan = TaskPlan {
            intent_type: IntentType::Task,
            steps: vec![
                PlanStep {
                    agent: AgentKind::Search,
                    task: "step 0".into(),
                    depends_on: Some(1),
                },
                PlanStep {
                    agent: AgentKind::Chat,
                    task: "step 1".into(),
                    depends_on: None,
                },
            ],
        };
        let err = plan.validate().unwrap_err();
        assert!(err.message.contains("invalid depends_on"));
    }

    // --- OrchestratorAgent::prompt() ---

    fn agent() -> OrchestratorAgent {
        OrchestratorAgent::new("test-model")
    }

    /// Wrap a TaskPlan as the JSON string Ollama would return.
    fn ollama_envelope(plan: &TaskPlan) -> String {
        let content = serde_json::to_string(plan).unwrap();
        json!({ "message": { "role": "assistant", "content": content } }).to_string()
    }

    #[test]
    fn prompt_contains_model_stream_false_and_format() {
        let req = agent().prompt(&[], None);
        assert_eq!(req["model"], "test-model");
        assert_eq!(req["stream"], false);
        assert_eq!(req["think"], false);
        assert!(
            req["format"].is_object(),
            "format field should be a JSON schema object"
        );
    }

    #[test]
    fn prompt_first_message_is_system_prompt() {
        let req = agent().prompt(&[], None);
        let first = &req["messages"][0];
        assert_eq!(first["role"], "system");
        let content = first["content"].as_str().unwrap();
        assert!(
            content.contains("Agent L"),
            "system prompt should mention Agent L"
        );
    }

    #[test]
    fn prompt_trims_context_to_last_n_turns() {
        // Build 10 turns; only the last N_CONTEXT_TURNS should appear in messages
        let turns: Vec<Value> = (0..10u32)
            .map(|i| json!({ "role": "user", "content": format!("turn {i}") }))
            .collect();
        let req = agent().prompt(&turns, None);
        let messages = req["messages"].as_array().unwrap();
        // 1 system message + N_CONTEXT_TURNS context turns (no error feedback)
        assert_eq!(messages.len(), 1 + N_CONTEXT_TURNS);
        let last_content = messages.last().unwrap()["content"].as_str().unwrap();
        assert_eq!(last_content, "turn 9");
    }

    #[test]
    fn prompt_includes_all_turns_when_fewer_than_limit() {
        let turns: Vec<Value> = (0..3u32)
            .map(|i| json!({ "role": "user", "content": format!("turn {i}") }))
            .collect();
        let req = agent().prompt(&turns, None);
        let messages = req["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1 + 3); // system + 3 turns
    }

    #[test]
    fn prompt_appends_error_feedback_on_retry() {
        let err = ParseError {
            message: "missing steps field".into(),
        };
        let req = agent().prompt(&[], Some(&err));
        let messages = req["messages"].as_array().unwrap();
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "user");
        let content = last["content"].as_str().unwrap();
        assert!(
            content.contains("missing steps field"),
            "error text should be embedded in retry message"
        );
        assert!(
            content.contains("invalid"),
            "retry message should say the response was invalid"
        );
    }

    #[test]
    fn prompt_no_error_feedback_on_first_attempt() {
        let req = agent().prompt(&[], None);
        let messages = req["messages"].as_array().unwrap();
        // Only the system message — no extra user turn injected
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "system");
    }

    // --- OrchestratorAgent::parse() ---

    #[test]
    fn parse_succeeds_on_valid_envelope() {
        let plan = TaskPlan {
            intent_type: IntentType::Conversational,
            steps: vec![PlanStep {
                agent: AgentKind::Chat,
                task: "reply".into(),
                depends_on: None,
            }],
        };
        let raw = ollama_envelope(&plan);
        assert_eq!(agent().parse(&raw).unwrap(), plan);
    }

    #[test]
    fn parse_fails_when_response_is_not_json() {
        let err = agent().parse("not json at all").unwrap_err();
        assert!(
            err.message.contains("not valid JSON"),
            "got: {}",
            err.message
        );
        assert!(
            err.message.contains("not json at all"),
            "raw input should appear in error"
        );
    }

    #[test]
    fn parse_fails_when_message_content_is_missing() {
        let raw = json!({ "message": { "role": "assistant" } }).to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(
            err.message.contains("missing message.content"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_fails_when_envelope_has_no_message_key() {
        let raw = json!({ "done": true }).to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(
            err.message.contains("missing message.content"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_fails_when_content_has_unknown_intent_type() {
        let raw = json!({
            "message": {
                "content": r#"{"intent_type":"Bogus","steps":[{"agent":"Chat","task":"hi"}]}"#
            }
        })
        .to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(
            err.message.contains("task plan JSON is invalid"),
            "got: {}",
            err.message
        );
        // The bad content should be echoed so the caller knows what the model returned
        assert!(
            err.message.contains("Bogus"),
            "raw content should appear in error: {}",
            err.message
        );
    }

    #[test]
    fn parse_fails_when_content_is_missing_required_field() {
        // No "steps" field
        let raw = json!({
            "message": { "content": r#"{"intent_type":"Chat"}"# }
        })
        .to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(
            err.message.contains("task plan JSON is invalid"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn parse_fails_when_plan_fails_validation() {
        // Valid JSON structure but empty steps — should fail TaskPlan::validate()
        let raw = json!({
            "message": { "content": r#"{"intent_type":"Factual","steps":[]}"# }
        })
        .to_string();
        let err = agent().parse(&raw).unwrap_err();
        assert!(
            err.message.contains("at least one step"),
            "got: {}",
            err.message
        );
    }

    // ── gemma4 / code-fence robustness ───────────────────────────────────────

    #[test]
    fn parse_accepts_code_fenced_json() {
        // gemma4 wraps its JSON output in ```json ... ``` code fences
        let fenced = "```json\n{\"intent_type\":\"Factual\",\"steps\":[{\"agent\":\"Search\",\"task\":\"find it\"}]}\n```";
        let raw = json!({ "message": { "content": fenced } }).to_string();
        let plan = agent().parse(&raw).unwrap();
        assert_eq!(plan.intent_type, IntentType::Factual);
        assert_eq!(plan.steps[0].agent, AgentKind::Search);
    }

    #[test]
    fn parse_accepts_plan_field_alias() {
        // gemma4 sometimes uses "plan" instead of "steps"
        let content =
            r#"{"intent_type":"Conversational","plan":[{"agent":"Chat","task":"reply"}]}"#;
        let raw = json!({ "message": { "content": content } }).to_string();
        let plan = agent().parse(&raw).unwrap();
        assert_eq!(plan.intent_type, IntentType::Conversational);
        assert_eq!(plan.steps[0].agent, AgentKind::Chat);
    }

    #[test]
    fn parse_accepts_instruction_field_alias() {
        // gemma4 sometimes uses "instruction" instead of "task"
        let content =
            r#"{"intent_type":"Task","steps":[{"agent":"Code","instruction":"write a script"}]}"#;
        let raw = json!({ "message": { "content": content } }).to_string();
        let plan = agent().parse(&raw).unwrap();
        assert_eq!(plan.steps[0].task, "write a script");
    }

    #[test]
    fn strip_code_fence_removes_json_fence() {
        let fenced = "```json\n{\"key\":\"val\"}\n```";
        assert_eq!(strip_code_fence(fenced), "{\"key\":\"val\"}");
    }

    #[test]
    fn strip_code_fence_removes_plain_fence() {
        let fenced = "```\n{\"key\":\"val\"}\n```";
        assert_eq!(strip_code_fence(fenced), "{\"key\":\"val\"}");
    }

    #[test]
    fn strip_code_fence_no_fence_unchanged() {
        let s = "{\"key\":\"val\"}";
        assert_eq!(strip_code_fence(s), s);
    }
}
