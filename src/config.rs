use std::env;


pub struct Config {
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

        Self {
            ollama_url: format!("http://{}:{}/api/generate", host, port),
            model_name: model,
        }
    }
}