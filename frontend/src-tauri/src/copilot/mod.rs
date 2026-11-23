// copilot/mod.rs (fork addition)
//
// Meeting copilot: answers questions raised in a live meeting, using the web and (if the
// user allowed folders) read-only search of their files. Two engines:
// - "gemini": Gemini with Google Search grounding, using the Gemini Live API keys.
// - "custom": any OpenAI-compatible model (the summary model's endpoint), with Tavily for
//   web search.
// The frontend decides when to ask; this module holds the keys so they never reach the UI.
// Answers are not stored.

mod answer;
mod custom;
mod files;
mod gemini;
mod web;

use crate::state::AppState;
use answer::Web;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};

pub use answer::{CopilotCard, CopilotFile, CopilotSource};

/// Per request; "high" thinking models can take most of a minute per turn.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Gemini turns per answer; each turn may run several file lookups.
const GEMINI_MAX_ROUNDS: usize = 6;
const FOLDERS_FILE: &str = "copilot-folders.json";
const SETTINGS_FILE: &str = "copilot-settings.json";

fn app_file<R: Runtime>(app: &AppHandle<R>, name: &str) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(name))
        .map_err(|e| format!("App data folder unavailable: {}", e))
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

// ---------- settings (Tavily key) ----------

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredSettings {
    #[serde(default, rename = "tavilyApiKey", skip_serializing_if = "Option::is_none")]
    tavily_api_key: Option<String>,
}

fn load_settings<R: Runtime>(app: &AppHandle<R>) -> Result<StoredSettings, String> {
    let file = app_file(app, SETTINGS_FILE)?;
    Ok(std::fs::read_to_string(file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

fn save_settings<R: Runtime>(app: &AppHandle<R>, settings: &StoredSettings) -> Result<(), String> {
    let file = app_file(app, SETTINGS_FILE)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not save copilot settings: {}", e))?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(file, json).map_err(|e| format!("Could not save copilot settings: {}", e))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSettingsView {
    /// Last 4 characters of the saved Tavily key.
    tavily_key_hint: Option<String>,
    /// Model configured for summaries on the custom OpenAI-compatible endpoint, if any.
    custom_default_model: Option<String>,
}

async fn settings_view<R: Runtime>(app: &AppHandle<R>, pool: &sqlx::SqlitePool) -> Result<CopilotSettingsView, String> {
    let tavily_key_hint = load_settings(app)?
        .tavily_api_key
        .map(|k| k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect());
    let custom_default_model = custom_config(pool).await.ok().map(|c| c.model);
    Ok(CopilotSettingsView { tavily_key_hint, custom_default_model })
}

async fn custom_config(pool: &sqlx::SqlitePool) -> Result<crate::summary::CustomOpenAIConfig, String> {
    crate::database::repositories::setting::SettingsRepository::get_custom_openai_config(pool)
        .await
        .map_err(|e| format!("Could not read the custom model settings: {}", e))?
        .filter(|c| !c.endpoint.trim().is_empty())
        .ok_or_else(|| "Set up a custom OpenAI-compatible model in Settings → Summary first.".to_string())
}

#[tauri::command]
pub async fn copilot_get_settings<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<CopilotSettingsView, String> {
    settings_view(&app, state.db_manager.pool()).await
}

/// Save (or, with an empty key, remove) the Tavily API key.
#[tauri::command]
pub async fn copilot_set_tavily_key<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    api_key: String,
) -> Result<CopilotSettingsView, String> {
    let key = api_key.trim();
    let mut settings = load_settings(&app)?;
    settings.tavily_api_key = (!key.is_empty()).then(|| key.to_string());
    save_settings(&app, &settings)?;
    log::info!("Copilot: Tavily key {}", if key.is_empty() { "removed" } else { "saved" });
    settings_view(&app, state.db_manager.pool()).await
}

/// Models offered by the custom OpenAI-compatible endpoint.
#[tauri::command]
pub async fn copilot_list_models(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let config = custom_config(state.db_manager.pool()).await?;
    custom::list_models(&http_client()?, &config.endpoint, config.api_key.as_deref()).await
}

// ---------- research ----------

/// Research the latest meeting context with the chosen `engine` ("gemini" or "custom").
/// With `question`, always answers it; without, returns `None` when nothing in the
/// transcript is worth looking up. An empty `model` means the engine's default.
#[tauri::command]
pub async fn copilot_research<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    engine: String,
    model: String,
    transcript: String,
    covered: Vec<String>,
    question: Option<String>,
) -> Result<Option<CopilotCard>, String> {
    let question = question.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    let prompt = answer::build_prompt(&transcript, &covered, question.as_deref());
    let folders = files::load_folders(&app_file(&app, FOLDERS_FILE)?);
    let roots = files::resolve_roots(&folders);
    let pool = state.db_manager.pool();
    let client = http_client()?;

    let card = if engine == "custom" {
        let config = custom_config(pool).await?;
        let model = Some(model.trim()).filter(|m| !m.is_empty()).unwrap_or(config.model.trim());
        if !answer::valid_model(model) {
            return Err("Invalid model name.".into());
        }
        let tavily_key = load_settings(&app)?.tavily_api_key;
        let endpoint = custom::Endpoint { base_url: &config.endpoint, api_key: config.api_key.as_deref(), model };
        custom::research(&client, &endpoint, tavily_key.as_deref(), &folders, &roots, prompt).await?
    } else {
        research_gemini(&client, pool, model.trim(), &folders, &roots, prompt).await?
    };

    if card.is_none() && question.is_some() {
        return Err("The model returned no answer. Try rephrasing the question.".into());
    }
    Ok(card)
}

/// POST `body` to Gemini, trying keys from `first` on; moves to the next key when one is
/// rate limited or rejected. Returns the response and the index of the key that worked.
async fn post_gemini(
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
        if answer::is_rate_limited(status, &text) {
            last_error = "All Gemini API keys are rate limited. Try again in a minute, or switch the copilot to your custom model.".into();
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

async fn research_gemini(
    client: &reqwest::Client,
    pool: &sqlx::SqlitePool,
    model: &str,
    folders: &[String],
    roots: &[PathBuf],
    prompt: String,
) -> Result<Option<CopilotCard>, String> {
    if !gemini::valid_model(model) {
        return Err("Invalid model name.".into());
    }
    let keys = crate::audio::transcription::gemini_live::resolve_api_keys(pool).await?;
    if keys.is_empty() {
        return Err("The copilot needs a Gemini API key. Add one in Settings → Transcription → Gemini Live.".into());
    }
    let with_files = !roots.is_empty();
    let prompt_folders: &[String] = if with_files { folders } else { &[] };
    let system = answer::system_prompt(Web::GoogleSearch, prompt_folders);
    let mut contents = vec![json!({ "role": "user", "parts": [{ "text": prompt }] })];
    let url = format!("{}/{}:generateContent", gemini::ENDPOINT, model);
    let mut key_index = 0;
    let mut files_read: Vec<CopilotFile> = Vec::new();

    for _ in 0..GEMINI_MAX_ROUNDS {
        let body = gemini::request_body(&system, &contents, with_files);
        let (response, used) = post_gemini(client, &url, &keys, key_index, &body).await?;
        key_index = used;
        let candidate = response["candidates"].get(0).cloned().unwrap_or(Value::Null);
        let calls = gemini::function_calls(&candidate);

        if calls.is_empty() {
            return Ok(gemini::parse_card(&candidate).map(|mut card| {
                card.files = files_read;
                card
            }));
        }

        // The model's turn goes back unchanged: it carries thought signatures and the
        // server-side search calls that Gemini requires in the next request.
        contents.push(candidate["content"].clone());
        let tool_roots = roots.to_vec();
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

// ---------- folders ----------

#[tauri::command]
pub async fn copilot_get_folders<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    Ok(files::load_folders(&app_file(&app, FOLDERS_FILE)?))
}

/// Let the user pick a folder the copilot may read; returns the updated list.
#[tauri::command]
pub async fn copilot_add_folder<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picker = app.clone();
    let picked = tokio::task::spawn_blocking(move || picker.dialog().file().blocking_pick_folder())
        .await
        .map_err(|e| format!("Folder dialog failed: {}", e))?;
    let file = app_file(&app, FOLDERS_FILE)?;
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
    let file = app_file(&app, FOLDERS_FILE)?;
    let folders: Vec<String> = files::load_folders(&file).into_iter().filter(|f| *f != folder).collect();
    files::save_folders(&file, &folders)?;
    Ok(folders)
}

/// Open a file the copilot used, with the system's default app. Only files inside the
/// allowed folders can be opened.
#[tauri::command]
pub async fn copilot_open_file<R: Runtime>(app: AppHandle<R>, path: String) -> Result<(), String> {
    let roots = files::resolve_roots(&files::load_folders(&app_file(&app, FOLDERS_FILE)?));
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
