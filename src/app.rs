use tokio::sync::mpsc;

use crate::agents::orchestrator::TaskPlan;
use crate::startup::StartupTimings;

pub enum Role { User, Assistant }

pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, PartialEq, Debug)]
pub enum StartupState {
    Connecting,
    CheckingModel,
    LoadingModel,
    Ready,
    Failed(String),
}

pub enum AppEvent {
    Token(String),
    StreamDone,
    StartupUpdate(StartupState),
    /// Agent L has decided how to route the user's message.
    RouteDecision(TaskPlan),
}

pub struct App {
    pub input: String,
    pub history: Vec<ChatMessage>,
    pub scroll_offset: u16,
    pub content_height: usize,
    pub terminal_height: u16,
    pub auto_scroll: bool,
    pub is_loading: bool,
    pub model_name: String,
    pub base_url: String,
    pub token_count: usize,
    pub exit: bool,
    pub startup_state: StartupState,
    pub tick: usize,
    /// The most recent routing decision made by Agent L. `None` until the
    /// first message is processed.
    pub route_decision: Option<TaskPlan>,

    // Channel for the background worker to talk to the UI
    tx: mpsc::UnboundedSender<AppEvent>,
    rx: mpsc::UnboundedReceiver<AppEvent>,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let config = crate::config::Config::from_env();

        let startup_tx = tx.clone();
        let startup_config = crate::config::Config::from_env();
        tokio::spawn(async move {
            crate::startup::run_startup_checks(startup_config, startup_tx, StartupTimings::default()).await;
        });

        Self {
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            is_loading: false,
            exit: false,
            model_name: config.model_name,
            base_url: config.base_url,
            token_count: 0,
            content_height: 0,
            terminal_height: 10,
            auto_scroll: true,
            startup_state: StartupState::Connecting,
            tick: 0,
            route_decision: None,
            tx,
            rx,
        }
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let config = crate::config::Config::from_env();
        Self {
            startup_state: StartupState::Ready,
            tick: 0,
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            content_height: 0,
            terminal_height: 10,
            auto_scroll: true,
            is_loading: false,
            exit: false,
            model_name: config.model_name,
            base_url: config.base_url,
            token_count: 0,
            route_decision: None,
            tx,
            rx,
        }
    }

    #[cfg(test)]
    pub fn sender_for_test(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }

    pub fn ask_ollama(&mut self) {
        if self.input.is_empty() || self.is_loading || self.startup_state != StartupState::Ready {
            return;
        }

        let user_text = self.input.clone();
        // 1. Push user message
        self.history.push(ChatMessage { role: Role::User, content: user_text });

        // 2. Serialize NOW — placeholder not yet added.
        //    This slice is also the context Agent L receives for classification.
        let messages: Vec<serde_json::Value> = self.history.iter().map(|m| {
            serde_json::json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content
            })
        }).collect();

        // 3. Push empty assistant placeholder for streaming tokens to fill
        self.history.push(ChatMessage { role: Role::Assistant, content: String::new() });

        self.input.clear();
        self.is_loading = true;
        let tx = self.tx.clone();
        let chat_url = format!("{}/api/chat", self.base_url);
        let model = self.model_name.clone();

        tokio::spawn(async move {
            // Step A: run Agent L to classify intent and emit a RouteDecision.
            let agent = crate::agents::orchestrator::OrchestratorAgent::new(&model);
            let agent_url = chat_url.clone();
            let context = messages.clone();
            if let Ok(plan) = crate::agents::call_with_retry(
                &agent,
                &context,
                |req| {
                    let url = agent_url.clone();
                    async move { crate::ollama::post_json(&url, req).await }
                },
                3,
            )
            .await
            {
                let _ = tx.send(AppEvent::RouteDecision(plan));
            }

            // Step B: stream the actual chat response.
            let _ = crate::ollama::fetch_ollama_stream(&chat_url, &model, messages, tx).await;
        });
    }

    pub fn update(&mut self) {
        let new_tokens = false;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::Token(t) => {
                    if let Some(last) = self.history.last_mut() {
                        last.content.push_str(&t);
                        self.token_count += 1;
                    }
                }
                AppEvent::StreamDone => { self.is_loading = false; }
                AppEvent::StartupUpdate(state) => { self.startup_state = state; }
                AppEvent::RouteDecision(plan) => { self.route_decision = Some(plan); }
            }
        }

        // We only trigger the re-calculation if new text arrived
        if new_tokens && self.auto_scroll {
            self.recalculate_scroll();
        }

        self.tick = self.tick.wrapping_add(1);
    }

    fn recalculate_scroll(&mut self) {
        // We approximate the wrapped height.
        // Let's assume an 80-character width for the approximation.
        let mut estimated_lines = 0;
        for msg in &self.history {
            estimated_lines += 2; // Header + Spacer
            for line in msg.content.lines() {
                // Approximate wrapping: (length / width) + 1
                estimated_lines += (line.len() / 50).max(1);
            }
        }

        self.content_height = estimated_lines;
        let max_scroll = self.content_height.saturating_sub(self.terminal_height as usize);
        self.scroll_offset = max_scroll as u16;
    }

    // Speed Governor"
    pub fn enforce_auto_scroll(&mut self, total_lines: usize, viewport_height: u16) {
        self.content_height = total_lines;
        self.terminal_height = viewport_height;

        if self.auto_scroll {
            let max_scroll = total_lines.saturating_sub(viewport_height as usize);
            self.scroll_offset = max_scroll as u16;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        // If content is taller than the window, set offset to the difference
        if self.content_height > self.terminal_height as usize {
            self.scroll_offset = (self.content_height - self.terminal_height as usize) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_initial_state() {
        let app = App::new_for_test();
        assert_eq!(app.input, "");
        assert!(app.history.is_empty());
        assert!(!app.is_loading);
        assert_eq!(app.startup_state, StartupState::Ready);
        assert_eq!(app.tick, 0);
    }

    #[tokio::test]
    async fn test_tick_increments_each_update() {
        let mut app = App::new_for_test();
        app.update();
        app.update();
        app.update();
        assert_eq!(app.tick, 3);
    }

    #[tokio::test]
    async fn test_tick_wraps_at_max() {
        let mut app = App::new_for_test();
        app.tick = usize::MAX;
        app.update();
        assert_eq!(app.tick, 0);
    }

    #[tokio::test]
    async fn test_token_appends_to_last_message() {
        let mut app = App::new_for_test();
        app.history.push(ChatMessage { role: Role::Assistant, content: String::new() });
        let tx = app.sender_for_test();
        tx.send(AppEvent::Token("hello".to_string())).unwrap();
        app.update();
        assert_eq!(app.history.last().unwrap().content, "hello");
        assert_eq!(app.token_count, 1);
    }

    #[tokio::test]
    async fn test_multiple_tokens_concatenate() {
        let mut app = App::new_for_test();
        app.history.push(ChatMessage { role: Role::Assistant, content: String::new() });
        let tx = app.sender_for_test();
        tx.send(AppEvent::Token("foo".to_string())).unwrap();
        tx.send(AppEvent::Token(" ".to_string())).unwrap();
        tx.send(AppEvent::Token("bar".to_string())).unwrap();
        app.update();
        assert_eq!(app.history.last().unwrap().content, "foo bar");
        assert_eq!(app.token_count, 3);
    }

    #[tokio::test]
    async fn test_token_discarded_with_empty_history() {
        let mut app = App::new_for_test();
        let tx = app.sender_for_test();
        tx.send(AppEvent::Token("x".to_string())).unwrap();
        app.update();
        assert!(app.history.is_empty());
        assert_eq!(app.token_count, 0);
    }

    #[tokio::test]
    async fn test_stream_done_clears_loading() {
        let mut app = App::new_for_test();
        app.is_loading = true;
        let tx = app.sender_for_test();
        tx.send(AppEvent::StreamDone).unwrap();
        app.update();
        assert!(!app.is_loading);
    }

    #[tokio::test]
    async fn test_startup_update_transitions_state() {
        let mut app = App::new_for_test();
        let tx = app.sender_for_test();
        tx.send(AppEvent::StartupUpdate(StartupState::CheckingModel)).unwrap();
        app.update();
        assert_eq!(app.startup_state, StartupState::CheckingModel);

        tx.send(AppEvent::StartupUpdate(StartupState::Failed("err".to_string()))).unwrap();
        app.update();
        assert_eq!(app.startup_state, StartupState::Failed("err".to_string()));
    }

    #[tokio::test]
    async fn test_ask_ollama_blocked_when_not_ready() {
        let mut app = App::new_for_test();
        app.startup_state = StartupState::Connecting;
        app.input = "hi".to_string();
        app.ask_ollama();
        assert!(app.history.is_empty());
        assert!(!app.is_loading);
    }

    #[tokio::test]
    async fn test_ask_ollama_blocked_when_empty_input() {
        let mut app = App::new_for_test();
        app.input = String::new();
        app.ask_ollama();
        assert!(app.history.is_empty());
    }

    #[tokio::test]
    async fn test_ask_ollama_blocked_when_loading() {
        let mut app = App::new_for_test();
        app.is_loading = true;
        app.input = "hi".to_string();
        app.ask_ollama();
        assert!(app.history.is_empty());
    }

    #[tokio::test]
    async fn test_ask_ollama_pushes_messages() {
        let mut app = App::new_for_test();
        app.input = "What is Rust?".to_string();
        app.ask_ollama();
        assert_eq!(app.history.len(), 2);
        assert_eq!(app.history[0].content, "What is Rust?");
        assert_eq!(app.history[1].content, "");
        assert_eq!(app.input, "");
        assert!(app.is_loading);
    }

    #[tokio::test]
    async fn test_ask_ollama_double_call_blocked() {
        let mut app = App::new_for_test();
        app.input = "first".to_string();
        app.ask_ollama();
        // is_loading is now true; second call should be blocked
        app.input = "second".to_string();
        app.ask_ollama();
        // Only 2 messages from the first call, not 4
        assert_eq!(app.history.len(), 2);
    }

    #[tokio::test]
    async fn test_enforce_auto_scroll_on() {
        let mut app = App::new_for_test();
        app.auto_scroll = true;
        app.enforce_auto_scroll(100, 20);
        assert_eq!(app.scroll_offset, 80);
        assert_eq!(app.content_height, 100);
    }

    #[tokio::test]
    async fn test_enforce_auto_scroll_off() {
        let mut app = App::new_for_test();
        app.auto_scroll = false;
        app.scroll_offset = 5;
        app.enforce_auto_scroll(100, 20);
        assert_eq!(app.scroll_offset, 5);
    }

    #[tokio::test]
    async fn test_enforce_auto_scroll_content_fits() {
        let mut app = App::new_for_test();
        app.auto_scroll = true;
        app.enforce_auto_scroll(10, 20);
        assert_eq!(app.scroll_offset, 0);
    }

    #[tokio::test]
    async fn test_enforce_content_equals_viewport() {
        let mut app = App::new_for_test();
        app.auto_scroll = true;
        app.enforce_auto_scroll(20, 20);
        assert_eq!(app.scroll_offset, 0);
    }

    #[tokio::test]
    async fn test_scroll_to_bottom_when_taller() {
        let mut app = App::new_for_test();
        app.content_height = 50;
        app.terminal_height = 20;
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 30);
    }

    #[tokio::test]
    async fn test_scroll_to_bottom_when_fits() {
        let mut app = App::new_for_test();
        app.content_height = 10;
        app.terminal_height = 20;
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);
    }

    #[tokio::test]
    async fn test_scroll_to_bottom_equal() {
        let mut app = App::new_for_test();
        app.content_height = 20;
        app.terminal_height = 20;
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset, 0);
    }

    // --- RouteDecision ---

    fn chat_plan() -> TaskPlan {
        use crate::agents::orchestrator::{AgentKind, IntentType, PlanStep};
        TaskPlan {
            intent_type: IntentType::Conversational,
            steps: vec![PlanStep { agent: AgentKind::Chat, task: "reply".into(), depends_on: None }],
        }
    }

    #[tokio::test]
    async fn route_decision_starts_as_none() {
        let app = App::new_for_test();
        assert!(app.route_decision.is_none());
    }

    #[tokio::test]
    async fn route_decision_event_stores_plan() {
        let mut app = App::new_for_test();
        let plan = chat_plan();
        app.sender_for_test().send(AppEvent::RouteDecision(plan.clone())).unwrap();
        app.update();
        assert_eq!(app.route_decision.as_ref().unwrap(), &plan);
    }

    #[tokio::test]
    async fn route_decision_event_replaces_previous_plan() {
        use crate::agents::orchestrator::{AgentKind, IntentType, PlanStep};
        let mut app = App::new_for_test();
        let tx = app.sender_for_test();

        tx.send(AppEvent::RouteDecision(chat_plan())).unwrap();
        app.update();

        let search_plan = TaskPlan {
            intent_type: IntentType::Factual,
            steps: vec![PlanStep { agent: AgentKind::Search, task: "look it up".into(), depends_on: None }],
        };
        tx.send(AppEvent::RouteDecision(search_plan.clone())).unwrap();
        app.update();

        assert_eq!(app.route_decision.as_ref().unwrap(), &search_plan);
    }
}
