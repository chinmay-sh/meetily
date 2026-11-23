// copilot/web.rs (fork addition)
//
// Web search for the custom-model engine, via the Tavily Search API
// (https://docs.tavily.com/documentation/api-reference/endpoint/search). A basic search
// costs 1 credit; free keys get 1,000 credits a month.

use super::answer::CopilotSource;
use serde_json::{json, Value};

pub const SEARCH_URL: &str = "https://api.tavily.com/search";
const MAX_RESULTS: u32 = 5;
const MAX_CONTENT_CHARS: usize = 1_500;

#[derive(Debug, Clone, PartialEq)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub content: String,
    pub published: Option<String>,
}

pub fn tool_spec() -> (&'static str, &'static str, Value) {
    (
        "web_search",
        "Search the web. Returns up to 5 results, each with title, url, an excerpt and (for news) the publish date.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "topic": {
                    "type": "string",
                    "enum": ["general", "news"],
                    "description": "'news' for recent events and company or market news, otherwise 'general'"
                }
            },
            "required": ["query"]
        }),
    )
}

pub fn request_body(query: &str, topic: &str) -> Value {
    let topic = if topic == "news" { "news" } else { "general" };
    json!({ "query": query, "topic": topic, "search_depth": "basic", "max_results": MAX_RESULTS })
}

pub fn parse_results(body: &Value) -> Vec<WebResult> {
    body["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let url = r["url"].as_str()?.to_string();
            Some(WebResult {
                title: r["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(&url).to_string(),
                content: r["content"].as_str().unwrap_or("").chars().take(MAX_CONTENT_CHARS).collect(),
                published: r["published_date"].as_str().map(str::to_string),
                url,
            })
        })
        .collect()
}

/// What the model sees as the tool result.
pub fn tool_result(results: &[WebResult]) -> Value {
    let results: Vec<Value> = results
        .iter()
        .map(|r| json!({ "title": r.title, "url": r.url, "content": r.content, "published": r.published }))
        .collect();
    json!({ "results": results })
}

/// Results that can be shown as clickable sources (the app only opens https links).
pub fn sources(results: &[WebResult]) -> Vec<CopilotSource> {
    results
        .iter()
        .filter(|r| r.url.starts_with("https://"))
        .map(|r| CopilotSource { title: r.title.clone(), url: r.url.clone() })
        .collect()
}

fn error_message(status: u16) -> String {
    match status {
        401 => "The Tavily API key is invalid.".into(),
        429 => "Tavily rate limit reached. Try again shortly.".into(),
        432 | 433 => "Tavily usage limit reached (out of search credits).".into(),
        other => format!("Tavily search failed (HTTP {}).", other),
    }
}

pub async fn search(client: &reqwest::Client, api_key: &str, query: &str, topic: &str) -> Result<Vec<WebResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Empty search query.".into());
    }
    let response = client
        .post(SEARCH_URL)
        .bearer_auth(api_key)
        .json(&request_body(query, topic))
        .send()
        .await
        .map_err(|e| format!("Could not reach Tavily: {}", e))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(error_message(status));
    }
    let body: Value = response.json().await.map_err(|e| format!("Unreadable Tavily response: {}", e))?;
    Ok(parse_results(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_requests_and_parses_results() {
        assert_eq!(request_body("acme", "news")["topic"], "news");
        assert_eq!(request_body("acme", "finance")["topic"], "general");
        assert_eq!(request_body("acme", "")["max_results"], 5);

        let body = json!({ "query": "acme", "results": [
            { "title": "Acme raises $10M", "url": "https://news.example/acme", "content": "Acme raised...", "score": 0.9, "published_date": "Tue, 11 Mar 2025 17:00:00 GMT" },
            { "title": "", "url": "http://plain.example/x", "content": "x".repeat(5000) },
            { "title": "no url" }
        ]});
        let results = parse_results(&body);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].published.as_deref(), Some("Tue, 11 Mar 2025 17:00:00 GMT"));
        assert_eq!(results[1].title, "http://plain.example/x");
        assert_eq!(results[1].content.len(), MAX_CONTENT_CHARS);
        assert_eq!(sources(&results), vec![CopilotSource { title: "Acme raises $10M".into(), url: "https://news.example/acme".into() }]);
        assert_eq!(tool_result(&results)["results"][0]["url"], "https://news.example/acme");
        assert!(parse_results(&json!({})).is_empty());
    }

    #[test]
    fn explains_errors() {
        assert!(error_message(401).contains("invalid"));
        assert!(error_message(432).contains("credits"));
        assert!(error_message(500).contains("500"));
    }
}
