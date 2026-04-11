use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Tool;

/// A single search result returned by the Search specialist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

// ─── DuckDuckGo response shape ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DdgResponse {
    #[serde(rename = "AbstractText", default)]
    abstract_text: String,
    #[serde(rename = "AbstractURL", default)]
    abstract_url: String,
    #[serde(rename = "AbstractSource", default)]
    abstract_source: String,
    #[serde(rename = "RelatedTopics", default)]
    related_topics: Vec<DdgTopic>,
}

#[derive(Debug, Deserialize)]
struct DdgTopic {
    #[serde(rename = "Text", default)]
    text: String,
    #[serde(rename = "FirstURL", default)]
    first_url: String,
}

/// Parse a DuckDuckGo Instant Answer API JSON string into `SearchResult`s.
///
/// Priority:
/// 1. `AbstractText` + `AbstractURL` (must be `https://`) → one result
/// 2. `RelatedTopics` (up to 3) with non-empty Text and `https://` FirstURL
/// 3. If nothing found → single placeholder result
fn parse_ddg(json_str: &str) -> Vec<SearchResult> {
    let resp: DdgResponse = match serde_json::from_str(json_str) {
        Ok(r) => r,
        Err(_) => return no_results(),
    };

    let mut results = Vec::new();

    // Only include abstract if it has a valid https:// URL — DDG occasionally
    // returns malformed or non-https URLs (e.g. "tokio.ts") that should be skipped.
    if !resp.abstract_text.is_empty() && resp.abstract_url.starts_with("https://") {
        results.push(SearchResult {
            title: if resp.abstract_source.is_empty() {
                "DuckDuckGo".into()
            } else {
                resp.abstract_source
            },
            url: resp.abstract_url,
            snippet: resp.abstract_text,
        });
    }

    for topic in resp.related_topics.into_iter().take(3) {
        // Only include topics with a valid https:// URL.
        if topic.text.is_empty() || !topic.first_url.starts_with("https://") {
            continue;
        }
        results.push(SearchResult {
            title: topic
                .first_url
                .split('/')
                .next_back()
                .unwrap_or("result")
                .replace('_', " "),
            url: topic.first_url,
            snippet: topic.text,
        });
    }

    if results.is_empty() {
        no_results()
    } else {
        results
    }
}

/// Format search results as human-readable text so the model synthesises from
/// them rather than copying the JSON verbatim into its answer.
fn format_results(results: &[SearchResult]) -> String {
    results
        .iter()
        .map(|r| format!("Title: {}\nURL: {}\nSnippet: {}", r.title, r.url, r.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn no_results() -> Vec<SearchResult> {
    vec![SearchResult {
        title: "No results".into(),
        url: String::new(),
        snippet: "No results found.".into(),
    }]
}

// ─── WebSearchTool ────────────────────────────────────────────────────────────

/// Calls the DuckDuckGo Instant Answer API (no API key required).
///
/// Returns a JSON-serialized `Vec<SearchResult>` as the observation string.
pub struct WebSearchTool {
    /// Base URL for the DuckDuckGo API. Overridable in tests.
    base_url: String,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            base_url: "https://api.duckduckgo.com".into(),
        }
    }

    /// Construct with a custom base URL — used in tests to point at wiremock.
    pub fn new_with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo. Returns citations with title, url, and snippet."
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

    /// Calls DuckDuckGo synchronously (uses `block_in_place` to avoid blocking
    /// the async executor). Returns a JSON array of `SearchResult` objects.
    fn execute(&self, args: &Value) -> Result<String, String> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| "missing 'query' argument".to_string())?;

        let url = format!(
            "{}/?q={}&format=json&no_redirect=1&no_html=1",
            self.base_url,
            urlencoding::encode(query)
        );

        // `reqwest::blocking` must not be called directly inside an async
        // context without signalling to tokio that the thread will block.
        let body = tokio::task::block_in_place(|| {
            reqwest::blocking::get(&url)
                .and_then(|r| r.text())
                .map_err(|e| e.to_string())
        })?;

        let results = parse_ddg(&body);
        Ok(format_results(&results))
    }
}

// ─── LocalSearchTool ─────────────────────────────────────────────────────────

/// Searches local files with `grep -rn`.
///
/// Returns matching lines (file:line:content) trimmed to 2 000 characters.
pub struct LocalSearchTool;

impl Tool for LocalSearchTool {
    fn name(&self) -> &str {
        "local_search"
    }

    fn description(&self) -> &str {
        "Search within local files using grep. Returns matching file paths, line numbers, and content."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query", "path"],
            "properties": {
                "query": {"type": "string", "description": "Search pattern (grep regex)"},
                "path":  {"type": "string", "description": "Directory or file path to search"}
            }
        })
    }

    fn execute(&self, args: &Value) -> Result<String, String> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| "missing 'query' argument".to_string())?;
        let path = args["path"]
            .as_str()
            .ok_or_else(|| "missing 'path' argument".to_string())?;

        let output = std::process::Command::new("grep")
            .args(["-rn", "--max-count=20", query, path])
            .output()
            .map_err(|e| format!("failed to run grep: {e}"))?;

        // grep exits 1 when no matches found — that is not an error.
        if output.status.code() == Some(1) || output.stdout.is_empty() {
            return Ok("No matches found.".into());
        }

        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("grep error: {stderr}"));
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        const MAX: usize = 2000;
        let trimmed = if raw.len() > MAX { &raw[..MAX] } else { &raw };
        Ok(trimmed.to_string())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── parse_ddg ────────────────────────────────────────────────────────────

    #[test]
    fn parse_ddg_abstract_becomes_result() {
        let json_str = r#"{
            "AbstractText": "Paris is the capital of France.",
            "AbstractURL": "https://en.wikipedia.org/wiki/France",
            "AbstractSource": "Wikipedia",
            "RelatedTopics": []
        }"#;
        let results = parse_ddg(json_str);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Wikipedia");
        assert!(results[0].snippet.contains("Paris"));
        assert!(results[0].url.contains("wikipedia"));
    }

    #[test]
    fn parse_ddg_no_abstract_uses_related_topics() {
        let json_str = r#"{
            "AbstractText": "",
            "AbstractURL": "",
            "AbstractSource": "",
            "RelatedTopics": [
                {"Text": "France – A country in Europe", "FirstURL": "https://duckduckgo.com/France"}
            ]
        }"#;
        let results = parse_ddg(json_str);
        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("France"));
    }

    #[test]
    fn parse_ddg_empty_response_returns_no_results() {
        let json_str = r#"{
            "AbstractText": "",
            "AbstractURL": "",
            "AbstractSource": "",
            "RelatedTopics": []
        }"#;
        let results = parse_ddg(json_str);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "No results found.");
    }

    #[test]
    fn parse_ddg_caps_related_topics_at_three() {
        let topics: Vec<serde_json::Value> = (0..10)
            .map(|i| json!({"Text": format!("topic {i}"), "FirstURL": format!("https://example.com/{i}")}))
            .collect();
        let json_str = serde_json::to_string(
            &json!({"AbstractText":"","AbstractURL":"","AbstractSource":"","RelatedTopics":topics}),
        )
        .unwrap();
        let results = parse_ddg(&json_str);
        // no abstract + max 3 related topics
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn parse_ddg_invalid_json_returns_no_results() {
        let results = parse_ddg("not json");
        assert_eq!(results[0].snippet, "No results found.");
    }

    #[test]
    fn parse_ddg_filters_non_https_urls_from_related_topics() {
        let json_str = r#"{
            "AbstractText": "",
            "AbstractURL": "",
            "AbstractSource": "",
            "RelatedTopics": [
                {"Text": "Good result", "FirstURL": "https://example.com/good"},
                {"Text": "Bad http result", "FirstURL": "http://not-https.com/bad"},
                {"Text": "No protocol", "FirstURL": "tokio.ts"}
            ]
        }"#;
        let results = parse_ddg(json_str);
        assert_eq!(
            results.len(),
            1,
            "only https:// URLs should be included, got: {results:?}"
        );
        assert!(results[0].url.starts_with("https://"));
    }

    #[test]
    fn parse_ddg_filters_abstract_with_non_https_url() {
        let json_str = r#"{
            "AbstractText": "Some content",
            "AbstractURL": "http://not-https.com",
            "AbstractSource": "Test",
            "RelatedTopics": []
        }"#;
        let results = parse_ddg(json_str);
        // Abstract has non-https URL → filtered out → no results
        assert_eq!(
            results[0].snippet, "No results found.",
            "non-https abstract URL should be filtered out"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn web_search_returns_human_readable_text() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"AbstractText":"Paris is the capital.","AbstractURL":"https://en.wikipedia.org/wiki/France","AbstractSource":"Wikipedia","RelatedTopics":[]}"#,
            ))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new_with_base_url(server.uri());
        let result = tool
            .execute(&json!({"query": "capital of France"}))
            .unwrap();

        assert!(result.contains("Title:"), "result should have Title: field");
        assert!(result.contains("URL:"), "result should have URL: field");
        assert!(
            result.contains("Snippet:"),
            "result should have Snippet: field"
        );
        assert!(
            result.contains("Paris"),
            "result should contain the snippet content"
        );
        // Must NOT be a raw JSON array any more
        assert!(
            serde_json::from_str::<Vec<serde_json::Value>>(&result).is_err(),
            "result should not be JSON array"
        );
    }

    // ── WebSearchTool ────────────────────────────────────────────────────────

    #[test]
    fn web_search_schema_requires_query() {
        let tool = WebSearchTool::new();
        let schema = tool.schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r.as_str() == Some("query")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn web_search_parses_ddg_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"AbstractText":"Paris is the capital.","AbstractURL":"https://en.wikipedia.org/wiki/France","AbstractSource":"Wikipedia","RelatedTopics":[]}"#,
            ))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new_with_base_url(server.uri());
        let result = tool
            .execute(&json!({"query": "capital of France"}))
            .unwrap();
        assert!(
            result.contains("Paris"),
            "result should contain snippet content"
        );
        assert!(
            result.contains("https://en.wikipedia.org/wiki/France"),
            "result should contain URL"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn web_search_no_results_returns_placeholder() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"AbstractText":"","AbstractURL":"","AbstractSource":"","RelatedTopics":[]}"#,
            ))
            .mount(&server)
            .await;

        let tool = WebSearchTool::new_with_base_url(server.uri());
        let result = tool.execute(&json!({"query": "xyzzy"})).unwrap();
        assert!(
            result.contains("No results found."),
            "empty DDG response should produce placeholder text"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn web_search_http_error_returns_err() {
        // Bind and drop to get a closed port
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let tool = WebSearchTool::new_with_base_url(format!("http://127.0.0.1:{port}"));
        assert!(tool.execute(&json!({"query": "test"})).is_err());
    }

    // ── LocalSearchTool ──────────────────────────────────────────────────────

    #[test]
    fn local_search_schema_requires_query_and_path() {
        let tool = LocalSearchTool;
        let schema = tool.schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r.as_str() == Some("query")));
        assert!(required.iter().any(|r| r.as_str() == Some("path")));
    }

    #[test]
    fn local_search_returns_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world\nfoo bar\nhello again\n").unwrap();

        let tool = LocalSearchTool;
        let result = tool
            .execute(&json!({"query": "hello", "path": dir.path().to_str().unwrap()}))
            .unwrap();
        assert!(result.contains("hello"));
    }

    #[test]
    fn local_search_no_matches_returns_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "nothing interesting here\n").unwrap();

        let tool = LocalSearchTool;
        let result = tool
            .execute(&json!({"query": "xyzzy_no_match", "path": dir.path().to_str().unwrap()}))
            .unwrap();
        assert_eq!(result, "No matches found.");
    }

    #[test]
    fn local_search_trims_long_output() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.txt");
        // Write 500 lines containing "needle"
        let content: String = (0..500).map(|i| format!("needle line {i}\n")).collect();
        std::fs::write(&file, content).unwrap();

        let tool = LocalSearchTool;
        let result = tool
            .execute(&json!({"query": "needle", "path": dir.path().to_str().unwrap()}))
            .unwrap();
        assert!(result.len() <= 2000);
    }
}
