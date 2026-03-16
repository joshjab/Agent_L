use tokio::sync::mpsc;

pub enum Role { User, Assistant }

pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, PartialEq)]
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
            crate::startup::run_startup_checks(startup_config, startup_tx).await;
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
            tx,
            rx,
        }
    }

    pub fn ask_ollama(&mut self) {
        if self.input.is_empty() || self.is_loading || self.startup_state != StartupState::Ready {
            return;
        }

        let user_text = self.input.clone();
        // 1. Push user message
        self.history.push(ChatMessage { role: Role::User, content: user_text });

        // 2. Serialize NOW — placeholder not yet added
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

        tokio::spawn(async move {
            let _ = crate::ollama::fetch_ollama_stream(messages, tx).await;
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
