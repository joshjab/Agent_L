use serde_json::{Value, json};

use crate::prompts;

pub const DEFAULT_PERSONA_PROMPT: &str = "You are Agent-L, a local personal assistant \
running entirely on your device. You are helpful, concise, and direct. \
IMPORTANT: You have a knowledge cutoff and cannot reliably answer questions about \
current real-world facts — current leaders, today's news, recent events, live prices, \
or anything that changes over time. If asked such a question directly, say: \
\"I don't have current information on that — ask me to search for it.\" \
Do NOT guess or state facts about current events from your own training data.";

pub const GOAL_REMINDER_INTERVAL: usize = 10;

pub const GOAL_REMINDER_TEXT: &str = "Remember: You are Agent-L, a local personal \
assistant. Stay helpful, concise, and grounded — never fabricate facts.";

/// Wraps every Ollama request with a consistent system prompt and injects a
/// periodic goal reminder to prevent personality drift over long sessions.
pub struct Persona {
    pub system_prompt: String,
}

impl Default for Persona {
    fn default() -> Self {
        Self::new()
    }
}

impl Persona {
    /// Create a Persona.
    ///
    /// If the `PERSONA_SYSTEM_PROMPT` environment variable is set, its value is
    /// used as the system prompt. Otherwise falls back to [`DEFAULT_PERSONA_PROMPT`].
    pub fn new() -> Self {
        let system_prompt = std::env::var("PERSONA_SYSTEM_PROMPT")
            .unwrap_or_else(|_| prompts::load("persona", DEFAULT_PERSONA_PROMPT));
        Self { system_prompt }
    }

    /// Create a Persona with a specific prompt. Used in unit tests only.
    #[cfg(test)]
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: prompt.into(),
        }
    }

    /// Returns a `{"role": "system", "content": "..."}` message for Ollama.
    pub fn system_message(&self) -> Value {
        json!({"role": "system", "content": self.system_prompt})
    }

    /// Returns a goal-reminder message when `turn_count > 0` and
    /// `turn_count % GOAL_REMINDER_INTERVAL == 0`, otherwise `None`.
    ///
    /// The reminder is a `{"role": "system", "content": GOAL_REMINDER_TEXT}` value.
    pub fn goal_reminder_if_needed(&self, turn_count: usize) -> Option<Value> {
        if turn_count > 0 && turn_count.is_multiple_of(GOAL_REMINDER_INTERVAL) {
            let reminder = prompts::load("persona_goal_reminder", GOAL_REMINDER_TEXT);
            Some(json!({"role": "system", "content": reminder}))
        } else {
            None
        }
    }

    /// Build the full messages array for an Ollama request.
    ///
    /// - Prepends the system message.
    /// - Appends all of `history`.
    /// - If a goal reminder is due at `turn_count`, inserts it just before the
    ///   last message so the model sees it as recent context.
    pub fn build_messages(&self, history: &[Value], turn_count: usize) -> Vec<Value> {
        let mut result = Vec::with_capacity(history.len() + 2);
        result.push(self.system_message());

        if let Some(reminder) = self.goal_reminder_if_needed(turn_count) {
            if history.is_empty() {
                result.push(reminder);
            } else {
                // Insert reminder just before the last message.
                let (rest, last) = history.split_at(history.len() - 1);
                result.extend_from_slice(rest);
                result.push(reminder);
                result.push(last[0].clone());
            }
        } else {
            result.extend_from_slice(history);
        }

        result
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Serialised, to prevent parallel env-var tests from racing.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // ── Persona::from_prompt / system_message ────────────────────────────────

    #[test]
    fn from_prompt_stores_given_prompt() {
        let p = Persona::from_prompt("my prompt");
        assert_eq!(p.system_prompt, "my prompt");
    }

    #[test]
    fn system_message_has_system_role() {
        let p = Persona::from_prompt("x");
        let msg = p.system_message();
        assert_eq!(msg["role"], "system");
    }

    #[test]
    fn system_message_content_matches_prompt() {
        let p = Persona::from_prompt("hello world");
        let msg = p.system_message();
        assert_eq!(msg["content"], "hello world");
    }

    // ── Persona::new (env var) ───────────────────────────────────────────────

    #[test]
    fn new_uses_default_prompt_when_env_var_absent() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe { std::env::remove_var("PERSONA_SYSTEM_PROMPT") };
        let p = Persona::new();
        assert_eq!(p.system_prompt, DEFAULT_PERSONA_PROMPT);
    }

    #[test]
    fn new_uses_env_var_when_set() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe { std::env::set_var("PERSONA_SYSTEM_PROMPT", "custom prompt") };
        let p = Persona::new();
        unsafe { std::env::remove_var("PERSONA_SYSTEM_PROMPT") };
        assert_eq!(p.system_prompt, "custom prompt");
    }

    // ── goal_reminder_if_needed ──────────────────────────────────────────────

    #[test]
    fn goal_reminder_absent_at_turn_zero() {
        let p = Persona::from_prompt("x");
        assert!(p.goal_reminder_if_needed(0).is_none());
    }

    #[test]
    fn goal_reminder_absent_between_intervals() {
        let p = Persona::from_prompt("x");
        for t in 1..GOAL_REMINDER_INTERVAL {
            assert!(
                p.goal_reminder_if_needed(t).is_none(),
                "expected no reminder at turn {t}"
            );
        }
    }

    #[test]
    fn goal_reminder_present_at_interval() {
        let p = Persona::from_prompt("x");
        let msg = p
            .goal_reminder_if_needed(GOAL_REMINDER_INTERVAL)
            .expect("expected a reminder at turn GOAL_REMINDER_INTERVAL");
        assert_eq!(msg["role"], "system");
        assert_eq!(msg["content"], GOAL_REMINDER_TEXT);
    }

    #[test]
    fn goal_reminder_present_at_multiples_of_interval() {
        let p = Persona::from_prompt("x");
        assert!(
            p.goal_reminder_if_needed(GOAL_REMINDER_INTERVAL * 2)
                .is_some()
        );
        assert!(
            p.goal_reminder_if_needed(GOAL_REMINDER_INTERVAL * 3)
                .is_some()
        );
    }

    // ── build_messages ───────────────────────────────────────────────────────

    fn user_msg(content: &str) -> Value {
        json!({"role": "user", "content": content})
    }

    fn assistant_msg(content: &str) -> Value {
        json!({"role": "assistant", "content": content})
    }

    #[test]
    fn build_messages_prepends_system_prompt() {
        let p = Persona::from_prompt("sys");
        let history = vec![user_msg("hello")];
        let msgs = p.build_messages(&history, 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "sys");
    }

    #[test]
    fn build_messages_includes_all_history() {
        let p = Persona::from_prompt("sys");
        let history = vec![user_msg("a"), assistant_msg("b"), user_msg("c")];
        let msgs = p.build_messages(&history, 1);
        // system + 3 history messages
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1], user_msg("a"));
        assert_eq!(msgs[2], assistant_msg("b"));
        assert_eq!(msgs[3], user_msg("c"));
    }

    #[test]
    fn build_messages_no_reminder_when_not_due() {
        let p = Persona::from_prompt("sys");
        let history = vec![user_msg("hi"), assistant_msg("hey"), user_msg("again")];
        // turn 3 — not at a reminder interval
        let msgs = p.build_messages(&history, 3);
        assert_eq!(msgs.len(), 4); // just system + 3 history
        assert!(msgs.iter().all(|m| { m["content"] != GOAL_REMINDER_TEXT }));
    }

    #[test]
    fn build_messages_injects_reminder_before_last_when_due() {
        let p = Persona::from_prompt("sys");
        let history = vec![
            user_msg("first"),
            assistant_msg("reply"),
            user_msg("latest"),
        ];
        // turn GOAL_REMINDER_INTERVAL — reminder should appear
        let msgs = p.build_messages(&history, GOAL_REMINDER_INTERVAL);

        // Expected layout:
        //   [0] system prompt
        //   [1] user "first"
        //   [2] assistant "reply"
        //   [3] goal reminder (system)
        //   [4] user "latest"
        assert_eq!(msgs.len(), 5);
        let reminder_pos = msgs.len() - 2;
        assert_eq!(msgs[reminder_pos]["role"], "system");
        assert_eq!(msgs[reminder_pos]["content"], GOAL_REMINDER_TEXT);
        assert_eq!(msgs[msgs.len() - 1], user_msg("latest"));
    }

    #[test]
    fn build_messages_empty_history_returns_just_system() {
        let p = Persona::from_prompt("sys");
        let msgs = p.build_messages(&[], 0);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "system");
    }
}
