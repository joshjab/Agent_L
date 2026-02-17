use tokio::sync::mpsc;

pub enum Role { User, Assistant }

pub struct ChatMessage {
    pub role: Role,
    pub content: String,
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
    pub token_count: usize,
    pub exit: bool,

    // Channel for the background worker to talk to the UI
    tx: mpsc::UnboundedSender<String>,
    rx: mpsc::UnboundedReceiver<String>,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let config = crate::config::Config::from_env();
        Self {
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            is_loading: false,
            exit: false,
            model_name: config.model_name,
            token_count: 0,
            content_height: 0,
            terminal_height: 10, 
            auto_scroll: true, 
            tx,
            rx,
        }
    }

    pub fn ask_ollama(&mut self) {
        if self.input.is_empty() || self.is_loading { return; }

        let user_text = self.input.clone();
        // User message to history immediately
        self.history.push(ChatMessage { role: Role::User, content: user_text.clone() });
        // Empty Assistant entry that we will stream into
        self.history.push(ChatMessage { role: Role::Assistant, content: String::new() });
        
        self.input.clear();
        self.is_loading = true;
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let _ = crate::ollama::fetch_ollama_stream(&user_text, tx).await;
        });
    }
    
    pub fn update(&mut self) {
        let new_tokens = false;
        while let Ok(token) = self.rx.try_recv() {
            self.is_loading = false;
            if let Some(last_msg) = self.history.last_mut() {
                last_msg.content.push_str(&token);
                self.token_count += 1; // Increment tokens as they stream in
            }
        }

        // We only trigger the re-calculation if new text arrived
        if new_tokens && self.auto_scroll {
            self.recalculate_scroll();
        }
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