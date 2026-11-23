// copilot/custom.rs (fork addition)
//
// Custom-model engine: any OpenAI-compatible chat-completions endpoint (by default the one
// configured for summaries), using tool calls for web search (Tavily) and the user's files.

use super::answer::{self, CopilotCard, CopilotFile, CopilotSource, Web};
use super::web;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Model turns per answer; each turn may run several tool calls.
const MAX_ROUNDS: usize = 8;
const MAX_SOURCES: usize = 8;

pub struct Endpoint<'a> {
    /// Base URL, e.g. "https://host/v1".
    pub base_url: &'a str,
    pub api_key: Option<&'a str>,
    pub model: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

pub fn endpoint_url(base_url: &str, path: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), path)
}

pub fn tools(web_search: bool, files: bool) -> Vec<Value> {
    let mut specs = Vec::new();
    if web_search {
        specs.push(web::tool_spec());
    }
    if files {
        specs.extend(answer::file_tool_specs());
    }
    specs
        .into_iter()
        .map(|(name, description, parameters)| {
            json!({ "type": "function", "function": { "name": name, "description": description, "parameters": parameters } })
        })
        .collect()
}

pub fn request_body(model: &str, messages: &[Value], tools: &[Value]) -> Value {
    let mut body = json!({ "model": model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

pub fn tool_calls(message: &Value) -> Vec<ToolCall> {
    message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|call| {
            let function = &call["function"];
            let args = match &function["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                Value::Null => json!({}),
                other => other.clone(),
            };
            Some(ToolCall {
                id: call["id"].as_str().unwrap_or("").to_string(),
                name: function["name"].as_str()?.to_string(),
                args,
            })
        })
        .collect()
}

/// The assistant turn to send back. Tool calls go back unchanged: some gateways keep
/// provider data (such as Gemini thought signatures) inside them.
pub fn assistant_turn(message: &Value) -> Value {
    json!({
        "role": "assistant",
        "content": message.get("content").cloned().unwrap_or(Value::Null),
        "tool_calls": message["tool_calls"]
    })
}

pub fn final_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
        _ => String::new(),
    }
}

pub fn model_ids(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn authorized(request: reqwest::RequestBuilder, api_key: Option<&str>) -> reqwest::RequestBuilder {
    match api_key.filter(|k| !k.trim().is_empty()) {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

pub async fn list_models(client: &reqwest::Client, base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, String> {
    let response = authorized(client.get(endpoint_url(base_url, "models")), api_key)
        .send()
        .await
        .map_err(|e| format!("Could not reach the custom model endpoint: {}", e))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("The endpoint did not list its models (HTTP {}).", status));
    }
    let body: Value = response.json().await.map_err(|e| format!("Unreadable model list: {}", e))?;
    Ok(model_ids(&body))
}

/// Ask the model, running its tool calls, until it answers. `prompt` is the user turn
/// from `answer::build_prompt`. Returns `None` when the model found nothing worth answering.
pub async fn research(
    client: &reqwest::Client,
    endpoint: &Endpoint<'_>,
    tavily_key: Option<&str>,
    folders: &[String],
    roots: &[PathBuf],
    prompt: String,
) -> Result<Option<CopilotCard>, String> {
    let with_files = !roots.is_empty();
    let web_mode = if tavily_key.is_some() { Web::Tool } else { Web::None };
    let prompt_folders: &[String] = if with_files { folders } else { &[] };
    let tools = tools(tavily_key.is_some(), with_files);
    let mut messages = vec![
        json!({ "role": "system", "content": answer::system_prompt(web_mode, prompt_folders) }),
        json!({ "role": "user", "content": prompt }),
    ];
    let url = endpoint_url(endpoint.base_url, "chat/completions");
    let mut sources: Vec<CopilotSource> = Vec::new();
    let mut files_read: Vec<CopilotFile> = Vec::new();

    for _ in 0..MAX_ROUNDS {
        let request = client.post(&url).json(&request_body(endpoint.model, &messages, &tools));
        let response = authorized(request, endpoint.api_key)
            .send()
            .await
            .map_err(|e| format!("Could not reach the custom model: {}", e))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| format!("Custom model response error: {}", e))?;
        if status != 200 {
            let snippet: String = text.chars().take(300).collect();
            return Err(format!("Custom model error (HTTP {}): {}", status, snippet));
        }
        let body: Value = serde_json::from_str(&text).map_err(|e| format!("Unreadable custom model response: {}", e))?;
        let message = &body["choices"][0]["message"];
        let calls = tool_calls(message);

        if calls.is_empty() {
            return Ok(answer::parse_answer(&final_text(message)).map(|(topic, answer)| CopilotCard {
                topic,
                answer,
                sources,
                files: files_read,
                suggestions_html: None,
            }));
        }

        messages.push(assistant_turn(message));
        for call in calls {
            let result = match (call.name.as_str(), tavily_key) {
                ("web_search", Some(key)) => {
                    let query = call.args["query"].as_str().unwrap_or("");
                    match web::search(client, key, query, call.args["topic"].as_str().unwrap_or("general")).await {
                        Ok(results) => {
                            for source in web::sources(&results) {
                                if sources.len() < MAX_SOURCES && !sources.contains(&source) {
                                    sources.push(source);
                                }
                            }
                            web::tool_result(&results)
                        }
                        Err(error) => json!({ "error": error }),
                    }
                }
                ("web_search", None) => json!({ "error": "Web search is not available." }),
                _ => {
                    let (roots, name, args) = (roots.to_vec(), call.name.clone(), call.args.clone());
                    let (result, file) = tokio::task::spawn_blocking(move || answer::run_file_tool(&roots, &name, &args))
                        .await
                        .map_err(|e| format!("File lookup failed: {}", e))?;
                    if let Some(file) = file.filter(|f| !files_read.contains(f)) {
                        files_read.push(file);
                    }
                    result
                }
            };
            messages.push(json!({ "role": "tool", "tool_call_id": call.id, "content": result.to_string() }));
        }
    }
    Err("The copilot made too many lookups without answering. Try a more specific question.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_depend_on_what_is_available() {
        assert!(tools(false, false).is_empty());
        let names = |t: Vec<Value>| t.iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(tools(true, false)), vec!["web_search"]);
        assert_eq!(names(tools(true, true)), vec!["web_search", "search_files", "read_file"]);
        assert_eq!(tools(false, true)[0]["type"], "function");

        let body = request_body("m", &[json!({ "role": "user", "content": "hi" })], &[]);
        assert!(body.get("tools").is_none());
        assert_eq!(endpoint_url("https://h/v1/", "models"), "https://h/v1/models");
    }

    #[test]
    fn parses_tool_calls_and_replies() {
        let message = json!({
            "role": "assistant", "content": null, "reasoning_content": null,
            "tool_calls": [
                { "id": "call_1", "type": "function", "function": { "name": "web_search", "arguments": "{\n \"query\": \"acme news\", \"topic\": \"news\"}" }, "extra_content": { "google": { "thought_signature": "sig" } } },
                { "id": "call_2", "type": "function", "function": { "name": "read_file", "arguments": "not json" } },
                { "type": "function", "function": { "arguments": "{}" } }
            ]
        });
        let calls = tool_calls(&message);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], ToolCall { id: "call_1".into(), name: "web_search".into(), args: json!({ "query": "acme news", "topic": "news" }) });
        assert_eq!(calls[1].args, Value::Null);

        let turn = assistant_turn(&message);
        assert_eq!(turn["tool_calls"][0]["extra_content"]["google"]["thought_signature"], "sig");
        assert!(turn.get("reasoning_content").is_none());

        assert_eq!(final_text(&json!({ "content": "TOPIC: x\n- y" })), "TOPIC: x\n- y");
        assert_eq!(final_text(&json!({ "content": [{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }] })), "ab");
        assert_eq!(final_text(&json!({ "content": null })), "");
        assert!(tool_calls(&json!({ "content": "hi" })).is_empty());
    }

    #[test]
    fn lists_model_ids() {
        let body = json!({ "data": [{ "id": "b" }, { "id": "a" }, { "id": "b" }, { "object": "model" }] });
        assert_eq!(model_ids(&body), vec!["a", "b"]);
    }
}
