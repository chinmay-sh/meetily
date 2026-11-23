// copilot/gemini.rs (fork addition)
//
// Gemini request/response handling for the copilot: prompts, the request body (Google
// Search plus the file tools), parsing answers and function calls, and running the file
// tools. No network or app state here, so it can be unit-tested.

use super::files;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const MAX_TRANSCRIPT_CHARS: usize = 20_000;

const ROLE: &str = "You are a research assistant silently listening to a live meeting. \
You receive the latest part of the meeting transcript. It comes from speech recognition, so expect \
misheard words and a mix of Hindi and English.

If the input contains a 'Question from the user', always answer that question, using the transcript \
only as context.

Otherwise decide whether the participants just raised something you can help with right now: a \
factual question, or a specific company, product, person, market, event, regulation or number they are \
discussing. Ignore small talk and scheduling. Ignore topics listed under 'Already covered' unless \
something new about them was asked. If nothing qualifies, reply with exactly: NONE

";

const WEB_ONLY: &str = "Ignore the participants' own internal work, which the web cannot know about. \
Search the web before answering.

";

const WITH_FILES: &str = "Use Google Search for public facts. For the user's own work (their \
documents, code, clients, projects, earlier decisions), use the search_files and read_file tools, \
which can only see these folders:
{folders}
In automatic mode, only search files when someone refers to a document, file, report, code or earlier \
decision, or asks something the user's files can answer. search_files matches keywords in file names \
and contents, so try different keywords if the first search finds nothing. Read the most relevant \
files before answering and name the files you used.

";

const FORMAT: &str = "When you answer, reply in English in exactly this format:
TOPIC: <3 to 8 word title>
<2 to 5 short '- ' bullet points with the key facts, most recent first, with dates where relevant>

No preamble, no closing remarks, no other formatting.";

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSource {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotFile {
    pub name: String,
    pub path: String,
}

impl CopilotFile {
    fn new(path: String) -> Self {
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        Self { name, path }
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotCard {
    pub topic: String,
    pub answer: String,
    pub sources: Vec<CopilotSource>,
    /// Files the model read while answering.
    pub files: Vec<CopilotFile>,
    /// Google's Search Suggestions widget (HTML). Its terms require showing it with the answer.
    pub suggestions_html: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub args: Value,
    pub id: Option<String>,
}

/// The model name goes into the request URL, so allow only model-id characters.
pub fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 80
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

pub fn is_rate_limited(status: u16, body: &str) -> bool {
    let body = body.to_lowercase();
    status == 429 || body.contains("resource_exhausted") || body.contains("quota")
}

fn tail(text: &str, max_chars: usize) -> &str {
    match text.char_indices().rev().nth(max_chars.saturating_sub(1)) {
        Some((i, _)) if max_chars > 0 => &text[i..],
        _ => text,
    }
}

pub fn system_prompt(folders: &[String]) -> String {
    let mut prompt = String::from(ROLE);
    if folders.is_empty() {
        prompt.push_str(WEB_ONLY);
    } else {
        let list: Vec<String> = folders.iter().map(|f| format!("- {}", f)).collect();
        prompt.push_str(&WITH_FILES.replace("{folders}", &list.join("\n")));
    }
    prompt.push_str(FORMAT);
    prompt
}

pub fn build_prompt(transcript: &str, covered: &[String], question: Option<&str>) -> String {
    let mut prompt = String::new();
    if !covered.is_empty() {
        prompt.push_str("Already covered:\n");
        for topic in covered.iter().take(30) {
            prompt.push_str("- ");
            prompt.push_str(topic);
            prompt.push('\n');
        }
        prompt.push('\n');
    }
    let transcript = tail(transcript.trim(), MAX_TRANSCRIPT_CHARS);
    prompt.push_str("Latest meeting transcript:\n");
    prompt.push_str(if transcript.is_empty() { "(nothing said yet)" } else { transcript });
    if let Some(q) = question {
        prompt.push_str("\n\nQuestion from the user: ");
        prompt.push_str(q);
    }
    prompt
}

fn file_tools() -> Value {
    json!({ "functionDeclarations": [
        {
            "name": "search_files",
            "description": "Search the user's allowed folders. Matches keywords against file names and file contents (text, code, Markdown, PDF, Word, PowerPoint, Excel). Returns up to 10 files, best first, each with a snippet around the first match.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Keywords, e.g. 'acme pricing proposal'" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "read_file",
            "description": "Read the text of a file found with search_files, up to 20000 characters at a time.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The exact path returned by search_files" },
                    "start": { "type": "integer", "description": "Character offset to continue reading from (default 0)" }
                },
                "required": ["path"]
            }
        }
    ]})
}

pub fn request_body(system: &str, contents: &[Value], with_files: bool) -> Value {
    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": contents,
        "tools": [{ "google_search": {} }]
    });
    if with_files {
        body["tools"] = json!([{ "google_search": {} }, file_tools()]);
        // Required when Google Search and custom functions are combined in one request.
        body["toolConfig"] = json!({
            "includeServerSideToolInvocations": true,
            "functionCallingConfig": { "mode": "VALIDATED" }
        });
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

fn function_response(call: &FunctionCall, result: Value) -> Value {
    let mut part = json!({ "functionResponse": { "name": call.name, "response": result } });
    if let Some(id) = &call.id {
        part["functionResponse"]["id"] = json!(id);
    }
    part
}

/// Run one file tool. Returns the functionResponse part and, for a successful read, the file.
pub fn run_tool(roots: &[PathBuf], call: &FunctionCall) -> (Value, Option<CopilotFile>) {
    let (result, file) = match call.name.as_str() {
        "search_files" => {
            let hits = files::search(roots, call.args["query"].as_str().unwrap_or(""));
            let results: Vec<Value> = hits.iter().map(|h| json!({ "path": h.path, "snippet": h.snippet })).collect();
            (json!({ "results": results }), None)
        }
        "read_file" => {
            let start = call.args["start"].as_f64().unwrap_or(0.0).max(0.0) as usize;
            match files::read(roots, call.args["path"].as_str().unwrap_or(""), start) {
                Ok((path, content)) => (json!({ "path": path, "content": content }), Some(CopilotFile::new(path))),
                Err(error) => (json!({ "error": error }), None),
            }
        }
        other => (json!({ "error": format!("Unknown tool: {}", other) }), None),
    };
    (function_response(call, result), file)
}

/// `None` when the model found nothing worth researching.
pub fn parse_card(candidate: &Value) -> Option<CopilotCard> {
    let text: String = candidate["content"]["parts"]
        .as_array()?
        .iter()
        .filter(|p| !p["thought"].as_bool().unwrap_or(false))
        .filter_map(|p| p["text"].as_str())
        .collect();
    let text = text.trim();
    let first_line = text.lines().next().unwrap_or("").trim().trim_end_matches('.');
    if text.is_empty() || first_line.eq_ignore_ascii_case("NONE") {
        return None;
    }

    let (topic, answer) = match text.strip_prefix("TOPIC:") {
        Some(rest) => {
            let (topic, answer) = rest.split_once('\n').unwrap_or((rest, ""));
            (topic.trim().to_string(), answer.trim().to_string())
        }
        None => (String::new(), text.to_string()),
    };

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

    #[test]
    fn model_names_are_restricted() {
        assert!(valid_model("gemini-3.5-flash-lite"));
        assert!(!valid_model(""));
        assert!(!valid_model("../files/x"));
        assert!(!valid_model("m?key=1"));
    }

    #[test]
    fn tail_keeps_the_last_chars() {
        assert_eq!(tail("héllo world", 5), "world");
        assert_eq!(tail("abc", 10), "abc");
    }

    #[test]
    fn prompts() {
        let p = build_prompt("we should check Tesla", &["Nvidia earnings".to_string()], Some("Tesla stock today?"));
        assert!(p.contains("Already covered:\n- Nvidia earnings"));
        assert!(p.contains("we should check Tesla"));
        assert!(p.ends_with("Question from the user: Tesla stock today?"));
        assert!(build_prompt("", &[], None).contains("(nothing said yet)"));

        assert!(system_prompt(&[]).contains("Ignore the participants' own internal work"));
        let with_files = system_prompt(&["C:\\Docs".to_string()]);
        assert!(with_files.contains("folders:\n- C:\\Docs\n"));
        assert!(!with_files.contains("{folders}"));
    }

    #[test]
    fn request_body_adds_file_tools_only_with_folders() {
        let contents = vec![json!({ "role": "user", "parts": [{ "text": "hi" }] })];
        let web = request_body("sys", &contents, false);
        assert_eq!(web["tools"], json!([{ "google_search": {} }]));
        assert!(web.get("toolConfig").is_none());
        let both = request_body("sys", &contents, true);
        assert_eq!(both["tools"][1]["functionDeclarations"][0]["name"], "search_files");
        assert_eq!(both["toolConfig"]["includeServerSideToolInvocations"], true);
        assert_eq!(both["systemInstruction"]["parts"][0]["text"], "sys");
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
