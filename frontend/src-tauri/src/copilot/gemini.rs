// copilot/gemini.rs (fork addition)
//
// Gemini engine protocol: request body (Google Search plus the file tools), function calls
// and responses, and turning a reply plus its grounding metadata into a card.

use super::answer::{self, CopilotCard, CopilotFile, CopilotSource};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub args: Value,
    pub id: Option<String>,
}

/// Gemini puts the model name in the URL path, so it gets the strictest check.
pub fn valid_model(model: &str) -> bool {
    answer::valid_model(model) && !model.contains('/') && !model.contains(':')
}

pub fn request_body(system: &str, contents: &[Value], web_search: bool, with_files: bool) -> Value {
    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": contents
    });
    let mut tools = Vec::new();
    if with_files {
        let declarations: Vec<Value> = answer::file_tool_specs()
            .into_iter()
            .map(|(name, description, parameters)| json!({ "name": name, "description": description, "parameters": parameters }))
            .collect();
        tools.push(json!({ "functionDeclarations": declarations }));
    }
    if web_search {
        tools.push(json!({ "google_search": {} }));
    }
    if web_search && with_files {
        // Required when Google Search and custom functions are combined in one request.
        body["toolConfig"] = json!({
            "includeServerSideToolInvocations": true,
            "functionCallingConfig": { "mode": "VALIDATED" }
        });
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

pub fn function_calls(candidate: &Value) -> Vec<FunctionCall> {
    candidate["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            let call = part.get("functionCall")?;
            Some(FunctionCall {
                name: call["name"].as_str()?.to_string(),
                args: call["args"].clone(),
                id: call["id"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// Run a file tool for a function call. Returns the functionResponse part and, for a
/// successful read, the file.
pub fn run_tool(roots: &[PathBuf], call: &FunctionCall) -> (Value, Option<CopilotFile>) {
    let (result, file) = answer::run_file_tool(roots, &call.name, &call.args);
    let mut part = json!({ "functionResponse": { "name": call.name, "response": result } });
    if let Some(id) = &call.id {
        part["functionResponse"]["id"] = json!(id);
    }
    (part, file)
}

/// `None` when the model found nothing worth researching.
pub fn parse_card(candidate: &Value) -> Option<CopilotCard> {
    let text: String = candidate["content"]["parts"]
        .as_array()?
        .iter()
        .filter(|p| !p["thought"].as_bool().unwrap_or(false))
        .filter_map(|p| p["text"].as_str())
        .collect();
    let (topic, answer) = answer::parse_answer(&text)?;

    let metadata = &candidate["groundingMetadata"];
    let mut sources: Vec<CopilotSource> = Vec::new();
    for chunk in metadata["groundingChunks"].as_array().into_iter().flatten() {
        let web = &chunk["web"];
        if let Some(url) = web["uri"].as_str().filter(|u| u.starts_with("https://")) {
            if !sources.iter().any(|s| s.url == url) {
                let title = web["title"].as_str().unwrap_or(url).to_string();
                sources.push(CopilotSource { title, url: url.to_string() });
            }
        }
    }

    Some(CopilotCard {
        topic,
        answer,
        sources,
        files: Vec::new(),
        suggestions_html: metadata["searchEntryPoint"]["renderedContent"]
            .as_str()
            .filter(|h| !h.trim().is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::files;

    #[test]
    fn gemini_model_names_cannot_change_the_url_path() {
        assert!(valid_model("gemini-3.5-flash-lite"));
        assert!(!valid_model("models/x"));
        assert!(!valid_model("x:streamGenerateContent"));
    }

    #[test]
    fn request_body_includes_only_enabled_tools() {
        let contents = vec![json!({ "role": "user", "parts": [{ "text": "hi" }] })];
        let web = request_body("sys", &contents, true, false);
        assert_eq!(web["tools"], json!([{ "google_search": {} }]));
        assert!(web.get("toolConfig").is_none());
        let both = request_body("sys", &contents, true, true);
        assert_eq!(both["tools"][0]["functionDeclarations"][0]["name"], "search_files");
        assert_eq!(both["tools"][1], json!({ "google_search": {} }));
        assert_eq!(both["toolConfig"]["includeServerSideToolInvocations"], true);
        assert_eq!(both["systemInstruction"]["parts"][0]["text"], "sys");
        let files_only = request_body("sys", &contents, false, true);
        assert_eq!(files_only["tools"].as_array().unwrap().len(), 1);
        assert!(files_only.get("toolConfig").is_none());
        let none = request_body("sys", &contents, false, false);
        assert!(none.get("tools").is_none());
    }

    #[test]
    fn none_reply_means_no_card() {
        let body = json!({ "content": { "parts": [{ "text": "NONE." }] } });
        assert_eq!(parse_card(&body), None);
        assert_eq!(parse_card(&Value::Null), None);
    }

    #[test]
    fn parses_topic_answer_sources_and_suggestions() {
        let candidate = json!({
            "content": { "parts": [
                { "text": "thinking...", "thought": true },
                { "toolCall": { "toolType": "GOOGLE_SEARCH_WEB", "id": "t1" } },
                { "text": "TOPIC: Tesla Q3 deliveries\n- 497k cars delivered" }
            ]},
            "groundingMetadata": {
                "groundingChunks": [
                    { "web": { "uri": "https://a.example/1", "title": "a.example" } },
                    { "web": { "uri": "https://a.example/1", "title": "dup" } },
                    { "web": { "uri": "http://insecure.example", "title": "x" } }
                ],
                "searchEntryPoint": { "renderedContent": "<div>chips</div>" }
            }
        });
        let card = parse_card(&candidate).unwrap();
        assert_eq!(card.topic, "Tesla Q3 deliveries");
        assert_eq!(card.answer, "- 497k cars delivered");
        assert_eq!(card.sources, vec![CopilotSource { title: "a.example".into(), url: "https://a.example/1".into() }]);
        assert_eq!(card.suggestions_html.as_deref(), Some("<div>chips</div>"));
        assert!(function_calls(&candidate).is_empty());
    }

    #[test]
    fn function_calls_run_file_tools() {
        let dir = std::env::temp_dir().join(format!("meetily-copilot-gemini-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plan.md"), "Launch plan for Acme").unwrap();
        let roots = files::resolve_roots(&[files::display(&dir)]);

        let candidate = json!({ "content": { "role": "model", "parts": [
            { "functionCall": { "name": "search_files", "args": { "query": "acme" }, "id": "c1" }, "thoughtSignature": "sig" },
            { "functionCall": { "name": "read_file", "args": { "path": files::display(&dir.join("plan.md")) } } }
        ]}});
        let calls = function_calls(&candidate);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_deref(), Some("c1"));

        let (part, file) = run_tool(&roots, &calls[0]);
        assert_eq!(part["functionResponse"]["id"], "c1");
        assert_eq!(part["functionResponse"]["name"], "search_files");
        assert!(part["functionResponse"]["response"]["results"][0]["path"].as_str().unwrap().ends_with("plan.md"));
        assert!(file.is_none());

        let (part, file) = run_tool(&roots, &calls[1]);
        assert!(part["functionResponse"].get("id").is_none());
        assert_eq!(part["functionResponse"]["response"]["content"], "Launch plan for Acme");
        assert_eq!(file.unwrap().name, "plan.md");

        let outside = FunctionCall { name: "read_file".into(), args: json!({ "path": "C:\\Windows\\win.ini" }), id: None };
        assert!(run_tool(&roots, &outside).0["functionResponse"]["response"]["error"].is_string());
    }
}
