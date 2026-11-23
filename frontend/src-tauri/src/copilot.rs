// copilot.rs (fork addition)
//
// Meeting copilot: answers questions raised in a live meeting with Gemini + Google
// Search grounding. The frontend decides when to ask; this module only talks to Gemini,
// so the API keys never reach the UI. Nothing is stored: Google's grounding terms forbid
// caching results.

use crate::state::AppState;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const TIMEOUT: Duration = Duration::from_secs(45);
const MAX_TRANSCRIPT_CHARS: usize = 20_000;

const SYSTEM_PROMPT: &str = "You are a research assistant silently listening to a live meeting. \
You receive the latest part of the meeting transcript. It comes from speech recognition, so expect \
misheard words and a mix of Hindi and English.

If the input contains a 'Question from the user', always answer that question, using the transcript \
only as context.

Otherwise decide whether the participants just raised something where fresh facts from the web would \
help them right now: a factual question, or a specific company, product, person, market, event, \
regulation or number they are discussing. Ignore small talk, scheduling, and their own internal work \
that the web cannot know about. Ignore topics listed under 'Already covered' unless something new about \
them was asked. If nothing qualifies, reply with exactly: NONE

When you answer, search the web first and reply in English in exactly this format:
TOPIC: <3 to 8 word title>
<2 to 5 short '- ' bullet points with the key facts, most recent first, with dates where relevant>

No preamble, no closing remarks, no other formatting.";

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSource {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotCard {
    pub topic: String,
    pub answer: String,
    pub sources: Vec<CopilotSource>,
    /// Google's Search Suggestions widget (HTML). Its terms require showing it with the answer.
    pub suggestions_html: Option<String>,
}

/// The model name goes into the request URL, so allow only model-id characters.
fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 80
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

fn tail(text: &str, max_chars: usize) -> &str {
    match text.char_indices().rev().nth(max_chars.saturating_sub(1)) {
        Some((i, _)) if max_chars > 0 => &text[i..],
        _ => text,
    }
}

fn build_prompt(transcript: &str, covered: &[String], question: Option<&str>) -> String {
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

/// `None` when the model found nothing worth researching.
fn parse_response(body: &Value) -> Option<CopilotCard> {
    let candidate = body.get("candidates")?.get(0)?;
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
        suggestions_html: metadata["searchEntryPoint"]["renderedContent"]
            .as_str()
            .filter(|h| !h.trim().is_empty())
            .map(str::to_string),
    })
}

fn is_rate_limited(status: u16, body: &str) -> bool {
    let body = body.to_lowercase();
    status == 429 || body.contains("resource_exhausted") || body.contains("quota")
}

/// Research the latest meeting context. With `question`, always answers it; without,
/// returns `None` when nothing in the transcript is worth looking up.
#[tauri::command]
pub async fn copilot_research(
    state: tauri::State<'_, AppState>,
    model: String,
    transcript: String,
    covered: Vec<String>,
    question: Option<String>,
) -> Result<Option<CopilotCard>, String> {
    let model = model.trim();
    if !valid_model(model) {
        return Err("Invalid model name.".into());
    }
    let question = question.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    let keys = crate::audio::transcription::gemini_live::resolve_api_keys(state.db_manager.pool()).await?;
    if keys.is_empty() {
        return Err("The copilot needs a Gemini API key. Add one in Settings → Transcription → Gemini Live.".into());
    }

    let body = json!({
        "systemInstruction": { "parts": [{ "text": SYSTEM_PROMPT }] },
        "contents": [{
            "role": "user",
            "parts": [{ "text": build_prompt(&transcript, &covered, question.as_deref()) }]
        }],
        "tools": [{ "google_search": {} }]
    });
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;
    let url = format!("{}/{}:generateContent", ENDPOINT, model);

    // Try keys in order; move to the next one when a key is rate limited or rejected.
    let mut last_error = String::new();
    for (i, key) in keys.iter().enumerate() {
        let response = client
            .post(&url)
            .header("x-goog-api-key", key.as_str())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Could not reach Gemini: {}", e))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| format!("Gemini response error: {}", e))?;

        if status == 200 {
            let json: Value = serde_json::from_str(&text).map_err(|e| format!("Unreadable Gemini response: {}", e))?;
            let card = parse_response(&json);
            if card.is_none() && question.is_some() {
                return Err("Gemini returned no answer. Try rephrasing the question.".into());
            }
            return Ok(card);
        }
        let snippet: String = text.chars().take(300).collect();
        log::warn!("Copilot: Gemini key #{} failed (HTTP {})", i + 1, status);
        if is_rate_limited(status, &text) {
            last_error = "All Gemini API keys are rate limited. Try again in a minute.".into();
            continue;
        }
        last_error = format!("Gemini error (HTTP {}): {}", status, snippet);
        if !matches!(status, 401 | 403) && !text.contains("API_KEY_INVALID") {
            // A bad request or model name fails the same way with every key.
            return Err(last_error);
        }
    }
    Err(last_error)
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
    fn prompt_includes_covered_topics_and_question() {
        let p = build_prompt("we should check Tesla", &["Nvidia earnings".to_string()], Some("Tesla stock today?"));
        assert!(p.contains("Already covered:\n- Nvidia earnings"));
        assert!(p.contains("we should check Tesla"));
        assert!(p.ends_with("Question from the user: Tesla stock today?"));
        assert!(build_prompt("", &[], None).contains("(nothing said yet)"));
    }

    #[test]
    fn none_reply_means_no_card() {
        let body = json!({"candidates": [{"content": {"parts": [{"text": "NONE."}]}}]});
        assert_eq!(parse_response(&body), None);
        assert_eq!(parse_response(&json!({"candidates": []})), None);
    }

    #[test]
    fn parses_topic_answer_sources_and_suggestions() {
        let body = json!({"candidates": [{
            "content": {"parts": [
                {"text": "thinking...", "thought": true},
                {"text": "TOPIC: Tesla Q3 deliveries\n- 497k cars delivered"}
            ]},
            "groundingMetadata": {
                "groundingChunks": [
                    {"web": {"uri": "https://a.example/1", "title": "a.example"}},
                    {"web": {"uri": "https://a.example/1", "title": "dup"}},
                    {"web": {"uri": "http://insecure.example", "title": "x"}}
                ],
                "searchEntryPoint": {"renderedContent": "<div>chips</div>"}
            }
        }]});
        let card = parse_response(&body).unwrap();
        assert_eq!(card.topic, "Tesla Q3 deliveries");
        assert_eq!(card.answer, "- 497k cars delivered");
        assert_eq!(card.sources, vec![CopilotSource { title: "a.example".into(), url: "https://a.example/1".into() }]);
        assert_eq!(card.suggestions_html.as_deref(), Some("<div>chips</div>"));
    }
}
