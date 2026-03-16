use std::env;


pub struct Config {
    pub base_url: String,
    pub ollama_url: String,
    pub model_name: String,
}

impl Config {
    pub fn from_env() -> Self {
        // Load .env file if it exists
        let _ = dotenvy::dotenv();

        let host = env::var("OLLAMA_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = env::var("OLLAMA_PORT").unwrap_or_else(|_| "11434".to_string());
        let model = env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3".to_string());

        let base_url = format!("http://{}:{}", host, port);
        Self {
            ollama_url: format!("{}/api/chat", base_url),
            base_url,
            model_name: model,
        }
    }
}