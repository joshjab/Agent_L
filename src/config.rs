use std::env;

pub struct Config {
    pub base_url: String,
    pub model_name: String,
}

impl Config {
    pub fn new(host: &str, port: u16, model: &str) -> Self {
        Self {
            base_url: format!("http://{}:{}", host, port),
            model_name: model.to_string(),
        }
    }

    pub fn from_env() -> Self {
        // Load .env file if it exists (skip in tests so env-var tests are deterministic)
        #[cfg(not(test))]
        let _ = dotenvy::dotenv();

        let host = env::var("OLLAMA_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port: u16 = env::var("OLLAMA_PORT")
            .unwrap_or_else(|_| "11434".to_string())
            .parse()
            .unwrap_or(11434);
        let model = env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3".to_string());

        Self::new(&host, port, &model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env-var tests to prevent parallel test races
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_defaults() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
            std::env::remove_var("OLLAMA_PORT");
            std::env::remove_var("OLLAMA_MODEL");
        }

        let config = Config::from_env();
        assert_eq!(config.base_url, "http://127.0.0.1:11434");
        assert_eq!(config.model_name, "llama3");
    }

    #[test]
    fn test_custom_env_vars() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var("OLLAMA_HOST", "10.0.0.1");
            std::env::set_var("OLLAMA_PORT", "9999");
            std::env::set_var("OLLAMA_MODEL", "mistral");
        }

        let config = Config::from_env();
        assert_eq!(config.base_url, "http://10.0.0.1:9999");
        assert_eq!(config.model_name, "mistral");

        // Teardown
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
            std::env::remove_var("OLLAMA_PORT");
            std::env::remove_var("OLLAMA_MODEL");
        }
    }

    #[test]
    fn test_new_constructor() {
        let config = Config::new("192.168.1.5", 8080, "codellama");
        assert_eq!(config.base_url, "http://192.168.1.5:8080");
        assert_eq!(config.model_name, "codellama");
    }
}
