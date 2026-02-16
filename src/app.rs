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
    pub exit: bool,
    // Channel for the background worker to talk to the UI
    tx: mpsc::UnboundedSender<String>,
    rx: mpsc::UnboundedReceiver<String>,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            input: String::new(),
            history: Vec::new(),
            scroll_offset: 0,
            is_loading: false,
            exit: false,
            
            // Defaulting terminal_height to something safe. 
            // The UI will overwrite this on the first draw call.
            content_height: 0,
            terminal_height: 10, 
            auto_scroll: true, // Default to following the AI's "typing"
            
            tx,
            rx,
        }
    }

    pub fn ask_ollama(&mut self) {
        if self.input.is_empty() || self.is_loading { return; }

        let user_text = self.input.clone();
        // Add User message to history immediately
        self.history.push(ChatMessage { role: Role::User, content: user_text.clone() });
        // Add an empty Assistant entry that we will stream into
        self.history.push(ChatMessage { role: Role::Assistant, content: String::new() });
        
        self.input.clear();
        self.is_loading = true;
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let _ = crate::ollama::fetch_ollama_stream(&user_text, tx).await;
        });
    }
    
    pub fn update(&mut self) {
        let mut new_tokens = false;
        while let Ok(token) = self.rx.try_recv() {
            self.is_loading = false;
            if let Some(last_msg) = self.history.last_mut() {
                last_msg.content.push_str(&token);
                new_tokens = true;
            }
        }
    
        // Auto-scroll logic: If we got new text, move the offset down
        // This is a naive implementation; 
        // a perfect one requires knowing the terminal width to calculate line wraps.
        if new_tokens && self.history.len() > 5 {
            // Just incrementing the offset based on message count for now
            self.scroll_offset = (self.history.len() as u16).saturating_sub(5); 
        }
    }
    pub fn scroll_to_bottom(&mut self) {
        // If content is taller than the window, set offset to the difference
        if self.content_height > self.terminal_height as usize {
            self.scroll_offset = (self.content_height - self.terminal_height as usize) as u16;
        }
    }
}