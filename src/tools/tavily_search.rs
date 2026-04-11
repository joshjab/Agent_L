use serde::Deserialize;
use serde_json::{Value, json};

use super::Tool;

// ─── Tavily response shapes ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
    #[serde(default)]
    answer: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    /// Main textual content for this result (Tavily's field name is `content`).
    content: String,
}

// ─── TavilySearchTool ────────────────────────────────────────────────────────

/// Calls the Tavily Search API.
///
/// Returns formatted text (same layout as DDG: `Title: / URL: / Snippet:`)
/// so the Search specialist's system prompt needs no changes. When Tavily
/// includes a top-level `answer` field it is prepended as `Answer: <text>`.
#[derive(Debug)]
pub struct TavilySearchTool {
    api_key: String,
    base_url: String,
}

impl TavilySearchTool {
    /// Construct with an explicit API key (base URL defaults to Tavily prod).
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.tavily.com".to_string(),
        }
    }

    /// Construct with a custom base URL — used in tests to point at wiremock.
    #[cfg(test)]
    pub fn new_with_base_url(api_key: String, base_url: impl Into<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.into(),
        }
    }

    /// Construct from the `TAVILY_API_KEY` environment variable.
    /// Returns `Err` with a clear message when the variable is not set.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("TAVILY_API_KEY")
            .map_err(|_| "TAVILY_API_KEY is not set — add it to .env or export it".to_string())?;
        Ok(Self::new(key))
    }
}

impl Tool for TavilySearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using Tavily. Returns citations with title, url, and snippet."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {"type": "string", "description": "The search query"}
            }
        })
    }

    fn execute(&self, args: &Value) -> Result<String, String> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| "missing 'query' argument".to_string())?;

        let url = format!("{}/search", self.base_url);
        let body = json!({
            "api_key": self.api_key,
            "query": query,
            "search_depth": "basic",
            "max_results": 5
        });

        let response_text = tokio::task::block_in_place(|| {
            reqwest::blocking::Client::new()
                .post(&url)
                .json(&body)
                .send()
                .and_then(|r| r.text())
                .map_err(|e| e.to_string())
        })?;

        let resp: TavilyResponse = serde_json::from_str(&response_text)
            .map_err(|e| format!("failed to parse Tavily response: {e}"))?;

        let mut output = String::new();

        if let Some(answer) = resp.answer.filter(|a| !a.is_empty()) {
            output.push_str(&format!("Answer: {answer}\n\n"));
        }

        if resp.results.is_empty() {
            output.push_str("No results found.");
            return Ok(output);
        }

        let formatted = resp
            .results
            .iter()
            .map(|r| format!("Title: {}\nURL: {}\nSnippet: {}", r.title, r.url, r.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        output.push_str(&formatted);
        Ok(output)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    // ── TavilySearchTool ─────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn tavily_happy_path_formats_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"title": "Rust Lang", "url": "https://rust-lang.org", "content": "Systems language."},
                    {"title": "Crates.io", "url": "https://crates.io", "content": "Package registry."}
                ]
            })))
            .mount(&server)
            .await;

        let tool = TavilySearchTool::new_with_base_url("test_key".into(), server.uri());
        let result = tool.execute(&json!({"query": "Rust"})).unwrap();

        assert!(result.contains("Title: Rust Lang"), "missing title");
        assert!(result.contains("URL: https://rust-lang.org"), "missing URL");
        assert!(
            result.contains("Snippet: Systems language."),
            "missing snippet"
        );
        assert!(result.contains("Title: Crates.io"), "missing second result");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tavily_answer_field_prepended() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "answer": "The current president is Donald Trump.",
                "results": [
                    {"title": "White House", "url": "https://whitehouse.gov", "content": "Official site."}
                ]
            })))
            .mount(&server)
            .await;

        let tool = TavilySearchTool::new_with_base_url("test_key".into(), server.uri());
        let result = tool
            .execute(&json!({"query": "current US president"}))
            .unwrap();

        assert!(
            result.starts_with("Answer: The current president is Donald Trump."),
            "answer must be prepended, got: {result:?}"
        );
        assert!(
            result.contains("Title: White House"),
            "result must follow answer"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tavily_empty_results_returns_placeholder() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .mount(&server)
            .await;

        let tool = TavilySearchTool::new_with_base_url("test_key".into(), server.uri());
        let result = tool.execute(&json!({"query": "xyzzy_no_match"})).unwrap();

        assert!(
            result.contains("No results found."),
            "empty results must return placeholder, got: {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tavily_http_error_returns_err() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let tool = TavilySearchTool::new_with_base_url(
            "test_key".into(),
            format!("http://127.0.0.1:{port}"),
        );
        assert!(
            tool.execute(&json!({"query": "test"})).is_err(),
            "connection refused must return Err"
        );
    }

    #[test]
    fn tavily_missing_api_key_returns_clear_error() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::remove_var("TAVILY_API_KEY");
        }
        let result = TavilySearchTool::from_env();
        assert!(result.is_err(), "missing TAVILY_API_KEY must be Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("TAVILY_API_KEY"),
            "error must name the missing var, got: {msg:?}"
        );
    }
}
