// copilot/answer.rs (fork addition)
//
// Pieces shared by both copilot engines (Gemini, custom OpenAI-compatible model): card
// types, prompts, parsing the model's answer, and the file tools. No network or app state.

use super::files;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

const WEB_GOOGLE: &str = "Search the web before answering questions about public facts.

";

const WEB_TOOL: &str = "Use the web_search tool for public facts and recent news: search before \
answering, and try a different query if the results are poor.

";

const WEB_NONE: &str = "You have no web access. Answer public questions from your own knowledge and \
say when the information may be out of date. In automatic mode, reply NONE unless you are confident.

";

const NO_FILES: &str = "Ignore the participants' own internal work, which you cannot know about.

";

const FILES: &str = "For the user's own work (their documents, code, clients, projects, earlier \
decisions), use the search_files and read_file tools, which can only see these folders:
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

/// How the model reaches the web.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Web {
    /// Gemini's built-in Google Search grounding.
    GoogleSearch,
    /// The web_search tool (Tavily).
    Tool,
    None,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
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
    /// Google's Search Suggestions widget (HTML), Gemini engine only. Its terms require
    /// showing it with the answer.
    pub suggestions_html: Option<String>,
}

/// Model names can end up in request URLs, so allow only model-id characters.
pub fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '/' | ':'))
        && !model.contains("..")
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

pub fn system_prompt(web: Web, folders: &[String]) -> String {
    let mut prompt = String::from(ROLE);
    prompt.push_str(match web {
        Web::GoogleSearch => WEB_GOOGLE,
        Web::Tool => WEB_TOOL,
        Web::None => WEB_NONE,
    });
    if folders.is_empty() {
        prompt.push_str(NO_FILES);
    } else {
        let list: Vec<String> = folders.iter().map(|f| format!("- {}", f)).collect();
        prompt.push_str(&FILES.replace("{folders}", &list.join("\n")));
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

/// The model's reply as (topic, answer); `None` when it found nothing worth answering.
/// Reasoning blocks some models print (`<think>…</think>`) are dropped.
pub fn parse_answer(text: &str) -> Option<(String, String)> {
    static REASONING: OnceLock<Regex> = OnceLock::new();
    let text = REASONING
        .get_or_init(|| Regex::new(r"(?s)<think(?:ing)?>.*?</think(?:ing)?>").expect("valid regex"))
        .replace_all(text, "");
    let text = text.trim();
    let first_line = text.lines().next().unwrap_or("").trim().trim_matches('*').trim_end_matches('.');
    if text.is_empty() || first_line.eq_ignore_ascii_case("NONE") {
        return None;
    }
    // Tolerate markdown around the marker, e.g. "**TOPIC:** ...".
    let marker = text.find("TOPIC:").filter(|&i| text[..i].trim_matches(|c: char| c == '*' || c == '#' || c == ' ').is_empty());
    Some(match marker {
        Some(i) => {
            let rest = text[i + "TOPIC:".len()..].trim_start_matches('*');
            let (topic, answer) = rest.split_once('\n').unwrap_or((rest, ""));
            (topic.trim().trim_matches('*').trim().to_string(), answer.trim().to_string())
        }
        None => (String::new(), text.to_string()),
    })
}

/// (name, description, JSON-schema parameters) of the file tools.
pub fn file_tool_specs() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "search_files",
            "Search the user's allowed folders. Matches keywords against file names and file contents (text, code, Markdown, PDF, Word, PowerPoint, Excel). Returns up to 10 files, best first, each with a snippet around the first match.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Keywords, e.g. 'acme pricing proposal'" }
                },
                "required": ["query"]
            }),
        ),
        (
            "read_file",
            "Read the text of a file found with search_files, up to 20000 characters at a time.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The exact path returned by search_files" },
                    "start": { "type": "integer", "description": "Character offset to continue reading from (default 0)" }
                },
                "required": ["path"]
            }),
        ),
    ]
}

/// Run a file tool. Returns the result for the model and, for a successful read, the file.
pub fn run_file_tool(roots: &[PathBuf], name: &str, args: &Value) -> (Value, Option<CopilotFile>) {
    match name {
        "search_files" => {
            let hits = files::search(roots, args["query"].as_str().unwrap_or(""));
            let results: Vec<Value> = hits.iter().map(|h| json!({ "path": h.path, "snippet": h.snippet })).collect();
            (json!({ "results": results }), None)
        }
        "read_file" => {
            let start = args["start"].as_f64().unwrap_or(0.0).max(0.0) as usize;
            match files::read(roots, args["path"].as_str().unwrap_or(""), start) {
                Ok((path, content)) => (json!({ "path": path, "content": content }), Some(CopilotFile::new(path))),
                Err(error) => (json!({ "error": error }), None),
            }
        }
        other => (json!({ "error": format!("Unknown tool: {}", other) }), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_are_restricted() {
        assert!(valid_model("gemini-3.5-flash-lite"));
        assert!(valid_model("devin/swe-1-6-slow"));
        assert!(valid_model("qwen2.5:7b"));
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

        let web_only = system_prompt(Web::GoogleSearch, &[]);
        assert!(web_only.contains("Ignore the participants' own internal work"));
        assert!(!web_only.contains("web_search"));
        let with_files = system_prompt(Web::Tool, &["C:\\Docs".to_string()]);
        assert!(with_files.contains("web_search tool"));
        assert!(with_files.contains("folders:\n- C:\\Docs\n"));
        assert!(!with_files.contains("{folders}"));
        assert!(system_prompt(Web::None, &[]).contains("no web access"));
    }

    #[test]
    fn parses_answers() {
        assert_eq!(parse_answer("NONE."), None);
        assert_eq!(parse_answer("**NONE**"), None);
        assert_eq!(parse_answer("<think>hmm, nothing</think>\nNONE"), None);
        assert_eq!(parse_answer("   "), None);
        assert_eq!(
            parse_answer("<think>plan</think>TOPIC: Tesla Q3\n- 497k delivered"),
            Some(("Tesla Q3".into(), "- 497k delivered".into()))
        );
        assert_eq!(
            parse_answer("**TOPIC:** Acme deal\n- Signed in May"),
            Some(("Acme deal".into(), "- Signed in May".into()))
        );
        assert_eq!(parse_answer("- just bullets"), Some((String::new(), "- just bullets".into())));
    }
}
