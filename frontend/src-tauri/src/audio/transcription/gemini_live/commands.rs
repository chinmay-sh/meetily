// audio/transcription/gemini_live/commands.rs
//
// Tauri commands for the Gemini Live settings panel and onboarding. Saved keys never
// leave the backend; the UI only sees their last 4 characters.

use super::connection::LiveConnection;
use super::settings::{self, GeminiLiveSettings};
use crate::state::AppState;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiLiveSettingsView {
    #[serde(flatten)]
    pub settings: GeminiLiveSettings,
    /// Last 4 characters of each saved key, primary first.
    pub api_key_hints: Vec<String>,
    /// A Gemini key is saved for summaries and will be used if no key is set here.
    pub has_summary_api_key: bool,
}

fn hint(key: &str) -> String {
    key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect()
}

async fn view(pool: &sqlx::SqlitePool) -> Result<GeminiLiveSettingsView, String> {
    let (settings, keys) = settings::load(pool).await.map_err(|e| e.to_string())?;
    Ok(GeminiLiveSettingsView {
        settings,
        api_key_hints: keys.iter().map(|k| hint(k)).collect(),
        has_summary_api_key: settings::summary_gemini_key(pool).await.is_some(),
    })
}

#[tauri::command]
pub async fn gemini_live_get_settings(state: tauri::State<'_, AppState>) -> Result<GeminiLiveSettingsView, String> {
    view(state.db_manager.pool()).await
}

#[tauri::command]
pub async fn gemini_live_save_settings(
    state: tauri::State<'_, AppState>,
    settings: GeminiLiveSettings,
) -> Result<GeminiLiveSettingsView, String> {
    let pool = state.db_manager.pool();
    settings::save(pool, &settings.normalized()).await.map_err(|e| e.to_string())?;
    log::info!("Gemini Live settings saved");
    view(pool).await
}

/// Append a key (it becomes a backup unless it is the first one).
#[tauri::command]
pub async fn gemini_live_add_api_key(
    state: tauri::State<'_, AppState>,
    api_key: String,
) -> Result<GeminiLiveSettingsView, String> {
    let pool = state.db_manager.pool();
    let (_, mut keys) = settings::load(pool).await.map_err(|e| e.to_string())?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err("The API key is empty.".into());
    }
    if keys.contains(&api_key) {
        return Err("This key is already saved.".into());
    }
    if keys.len() >= settings::MAX_API_KEYS {
        return Err(format!("You can save up to {} keys.", settings::MAX_API_KEYS));
    }
    keys.push(api_key);
    settings::save_keys(pool, keys).await.map_err(|e| e.to_string())?;
    log::info!("Gemini Live API key added");
    view(pool).await
}

#[tauri::command]
pub async fn gemini_live_remove_api_key(
    state: tauri::State<'_, AppState>,
    index: usize,
) -> Result<GeminiLiveSettingsView, String> {
    let pool = state.db_manager.pool();
    let (_, mut keys) = settings::load(pool).await.map_err(|e| e.to_string())?;
    if index >= keys.len() {
        return Err("No key at that position.".into());
    }
    keys.remove(index);
    settings::save_keys(pool, keys).await.map_err(|e| e.to_string())?;
    view(pool).await
}

/// Move a key to a new position (0 = primary).
#[tauri::command]
pub async fn gemini_live_move_api_key(
    state: tauri::State<'_, AppState>,
    index: usize,
    to_index: usize,
) -> Result<GeminiLiveSettingsView, String> {
    let pool = state.db_manager.pool();
    let (_, mut keys) = settings::load(pool).await.map_err(|e| e.to_string())?;
    if index >= keys.len() || to_index >= keys.len() {
        return Err("No key at that position.".into());
    }
    let key = keys.remove(index);
    keys.insert(to_index, key);
    settings::save_keys(pool, keys).await.map_err(|e| e.to_string())?;
    view(pool).await
}

/// Open a session to check a key and the network: a pasted `api_key`, the saved key at
/// `index`, or (neither given) the first key that would be used.
#[tauri::command]
pub async fn gemini_live_test_connection(
    state: tauri::State<'_, AppState>,
    api_key: Option<String>,
    index: Option<usize>,
) -> Result<String, String> {
    let pool = state.db_manager.pool();
    let key = match (api_key.filter(|k| !k.trim().is_empty()), index) {
        (Some(k), _) => k.trim().to_string(),
        (None, Some(i)) => {
            let (_, keys) = settings::load(pool).await.map_err(|e| e.to_string())?;
            keys.get(i).cloned().ok_or("No key at that position.")?
        }
        (None, None) => super::resolve_api_keys(pool)
            .await?
            .into_iter()
            .next()
            .ok_or("No API key saved yet. Paste your Gemini API key first.")?,
    };
    let (settings, _) = settings::load(pool).await.map_err(|e| e.to_string())?;
    let conn = LiveConnection::connect(&key, &settings.setup_options())
        .await
        .map_err(|e| e.to_string())?;
    conn.close_in_background();
    Ok(format!("Key …{} connected to {} successfully.", hint(&key), super::protocol::MODEL))
}

/// Open the folder containing the diagnostics log (`gemini-live.log`).
#[tauri::command]
pub async fn gemini_live_open_log_folder<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<String, String> {
    let dir = super::log_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create log folder: {}", e))?;
    let folder = dir.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let opener = "xdg-open";

    std::process::Command::new(opener)
        .arg(&folder)
        .spawn()
        .map_err(|e| format!("Failed to open folder: {}", e))?;
    Ok(super::diag::path_in(&dir).to_string_lossy().to_string())
}
