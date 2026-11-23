// copilot/mod.rs (fork addition)
//
// Meeting copilot: answers questions raised in a live meeting with Gemini, using Google
// Search and, if the user allowed folders, read-only search of their files. The frontend
// decides when to ask; this module talks to Gemini so API keys never reach the UI.
// Answers are not stored: Google's grounding terms forbid caching results.

mod files;
mod gemini;

use crate::state::AppState;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};

pub use gemini::{CopilotCard, CopilotFile, CopilotSource};

const TIMEOUT: Duration = Duration::from_secs(45);
/// Gemini turns per answer; each turn may run several file lookups.
const MAX_ROUNDS: usize = 6;
const FOLDERS_FILE: &str = "copilot-folders.json";

fn folders_file<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(FOLDERS_FILE))
        .map_err(|e| format!("App data folder unavailable: {}", e))
}

/// POST `body`, trying keys from `first` on; moves to the next key when one is rate
/// limited or rejected. Returns the response and the index of the key that worked.
async fn post(
    client: &reqwest::Client,
    url: &str,
    keys: &[String],
    first: usize,
    body: &Value,
) -> Result<(Value, usize), String> {
    let mut last_error = "No Gemini API key available.".to_string();
    for (i, key) in keys.iter().enumerate().skip(first) {
        let response = client
            .post(url)
            .header("x-goog-api-key", key.as_str())
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Could not reach Gemini: {}", e))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| format!("Gemini response error: {}", e))?;
        if status == 200 {
            let json = serde_json::from_str(&text).map_err(|e| format!("Unreadable Gemini response: {}", e))?;
            return Ok((json, i));
        }
        log::warn!("Copilot: Gemini key #{} failed (HTTP {})", i + 1, status);
        if gemini::is_rate_limited(status, &text) {
            last_error = "All Gemini API keys are rate limited. Try again in a minute.".into();
            continue;
        }
        last_error = format!("Gemini error (HTTP {}): {}", status, text.chars().take(300).collect::<String>());
        if !matches!(status, 401 | 403) && !text.contains("API_KEY_INVALID") {
            // A bad request or model name fails the same way with every key.
            return Err(last_error);
        }
    }
    Err(last_error)
}

/// Research the latest meeting context. With `question`, always answers it; without,
/// returns `None` when nothing in the transcript is worth looking up.
#[tauri::command]
pub async fn copilot_research<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    model: String,
    transcript: String,
    covered: Vec<String>,
    question: Option<String>,
) -> Result<Option<CopilotCard>, String> {
    let model = model.trim();
    if !gemini::valid_model(model) {
        return Err("Invalid model name.".into());
    }
    let question = question.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    let keys = crate::audio::transcription::gemini_live::resolve_api_keys(state.db_manager.pool()).await?;
    if keys.is_empty() {
        return Err("The copilot needs a Gemini API key. Add one in Settings → Transcription → Gemini Live.".into());
    }

    let folders = files::load_folders(&folders_file(&app)?);
    let roots = files::resolve_roots(&folders);
    let with_files = !roots.is_empty();
    let prompt_folders: &[String] = if with_files { &folders } else { &[] };
    let system = gemini::system_prompt(prompt_folders);
    let mut contents = vec![json!({
        "role": "user",
        "parts": [{ "text": gemini::build_prompt(&transcript, &covered, question.as_deref()) }]
    })];

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;
    let url = format!("{}/{}:generateContent", gemini::ENDPOINT, model);
    let mut key_index = 0;
    let mut files_read: Vec<CopilotFile> = Vec::new();

    for _ in 0..MAX_ROUNDS {
        let body = gemini::request_body(&system, &contents, with_files);
        let (response, used) = post(&client, &url, &keys, key_index, &body).await?;
        key_index = used;
        let candidate = response["candidates"].get(0).cloned().unwrap_or(Value::Null);
        let calls = gemini::function_calls(&candidate);

        if calls.is_empty() {
            let card = gemini::parse_card(&candidate).map(|mut card| {
                card.files = files_read;
                card
            });
            if card.is_none() && question.is_some() {
                return Err("Gemini returned no answer. Try rephrasing the question.".into());
            }
            return Ok(card);
        }

        // The model's turn goes back unchanged: it carries thought signatures and the
        // server-side search calls that Gemini requires in the next request.
        contents.push(candidate["content"].clone());
        let tool_roots = roots.clone();
        let results = tokio::task::spawn_blocking(move || {
            calls.iter().map(|call| gemini::run_tool(&tool_roots, call)).collect::<Vec<_>>()
        })
        .await
        .map_err(|e| format!("File lookup failed: {}", e))?;

        let mut parts = Vec::new();
        for (part, file) in results {
            if let Some(file) = file.filter(|f| !files_read.contains(f)) {
                files_read.push(file);
            }
            parts.push(part);
        }
        contents.push(json!({ "role": "user", "parts": parts }));
    }
    Err("The copilot made too many file lookups without answering. Try a more specific question.".into())
}

#[tauri::command]
pub async fn copilot_get_folders<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    Ok(files::load_folders(&folders_file(&app)?))
}

/// Let the user pick a folder the copilot may read; returns the updated list.
#[tauri::command]
pub async fn copilot_add_folder<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picker = app.clone();
    let picked = tokio::task::spawn_blocking(move || picker.dialog().file().blocking_pick_folder())
        .await
        .map_err(|e| format!("Folder dialog failed: {}", e))?;
    let file = folders_file(&app)?;
    let folders = files::load_folders(&file);
    let Some(picked) = picked else {
        return Ok(folders);
    };
    let folders = files::add_folder(folders, &PathBuf::from(picked.to_string()))?;
    files::save_folders(&file, &folders)?;
    log::info!("Copilot: folder access granted ({} folders)", folders.len());
    Ok(folders)
}

#[tauri::command]
pub async fn copilot_remove_folder<R: Runtime>(app: AppHandle<R>, folder: String) -> Result<Vec<String>, String> {
    let file = folders_file(&app)?;
    let folders: Vec<String> = files::load_folders(&file).into_iter().filter(|f| *f != folder).collect();
    files::save_folders(&file, &folders)?;
    Ok(folders)
}

/// Open a file the copilot used, with the system's default app. Only files inside the
/// allowed folders can be opened.
#[tauri::command]
pub async fn copilot_open_file<R: Runtime>(app: AppHandle<R>, path: String) -> Result<(), String> {
    let roots = files::resolve_roots(&files::load_folders(&folders_file(&app)?));
    let path = files::resolve_inside(&roots, &path)?;
    // Desktop launchers can block; keep them off the async workers (as open_external_url does).
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(windows)]
        let result = open::that_detached(&path);
        #[cfg(not(windows))]
        let result = open::that(&path);
        result.map_err(|e| format!("Failed to open file: {}", e))
    })
    .await
    .map_err(|e| format!("Failed to open file: {}", e))?
}
