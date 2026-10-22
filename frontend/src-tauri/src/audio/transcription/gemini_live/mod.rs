// audio/transcription/gemini_live/mod.rs
//
// Gemini Live (gemini-3.5-transcribe-live) cloud transcription engine. Fork addition:
// everything lives in this module; the rest of the app only gains `"geminiLive"`
// match arms that call into the functions below.

pub mod commands;
pub mod connection;
pub mod dedup;
pub mod pcm;
pub mod protocol;
pub mod provider;
pub mod settings;
#[cfg(test)]
mod tests;

use super::engine::TranscriptionEngine;
use super::provider::TranscriptionProvider;
use log::{info, warn};
use provider::{Backup, GeminiLiveProvider, GeminiLiveStatus, StatusCallback};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Value of `transcript_settings.provider` that selects this engine.
pub const PROVIDER_ID: &str = "geminiLive";

fn pool<R: Runtime>(app: &AppHandle<R>) -> sqlx::SqlitePool {
    app.state::<crate::state::AppState>().db_manager.pool().clone()
}

/// Keys to use, in order: the Gemini Live keys if any, otherwise the Gemini key saved
/// for summaries.
pub async fn resolve_api_keys(pool: &sqlx::SqlitePool) -> Result<Vec<String>, String> {
    let (_, keys) = settings::load(pool).await.map_err(|e| e.to_string())?;
    if !keys.is_empty() {
        return Ok(keys);
    }
    Ok(settings::summary_gemini_key(pool).await.into_iter().collect())
}

const NO_KEY: &str = "Gemini Live needs an API key. Add one in Settings → Transcription.";

/// Load the configured local engine (auto-selecting a downloaded model) for fallback.
async fn load_fallback<R: Runtime>(
    app: &AppHandle<R>,
    fallback_provider: &str,
) -> Result<Arc<dyn TranscriptionProvider>, String> {
    if fallback_provider == "localWhisper" {
        crate::whisper_engine::commands::whisper_init().await?;
        crate::whisper_engine::commands::whisper_validate_model_ready_with_config(app).await?;
        let engine = crate::whisper_engine::commands::WHISPER_ENGINE
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or("Whisper engine not initialized")?;
        Ok(Arc::new(super::WhisperProvider::new(engine)))
    } else {
        crate::parakeet_engine::commands::parakeet_init().await?;
        crate::parakeet_engine::commands::parakeet_validate_model_ready_with_config(app).await?;
        let engine = crate::parakeet_engine::commands::PARAKEET_ENGINE
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or("Parakeet engine not initialized")?;
        Ok(Arc::new(super::ParakeetProvider::new(engine)))
    }
}

fn fallback_label(fallback_provider: &str) -> &'static str {
    if fallback_provider == "localWhisper" { "Whisper" } else { "Parakeet" }
}

/// Pre-recording check: an API key exists and, if a local backup is selected, its model
/// is ready.
pub async fn validate_ready<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let pool = pool(app);
    if resolve_api_keys(&pool).await?.is_empty() {
        return Err(NO_KEY.into());
    }
    let (settings, _) = settings::load(&pool).await.map_err(|e| e.to_string())?;
    if settings.uses_local_fallback() {
        load_fallback(app, &settings.fallback_provider).await.map(|_| ()).map_err(|e| {
            format!(
                "Gemini Live is set to use {} as a backup, but it isn't ready ({}). Download a {} model, \
                 or set the backup engine to \"None (cloud only)\" in Settings → Transcription.",
                fallback_label(&settings.fallback_provider),
                e,
                fallback_label(&settings.fallback_provider)
            )
        })?;
    }
    info!("✅ Gemini Live ready (backup: {})", settings.fallback_provider);
    Ok(())
}

pub async fn build_engine<R: Runtime>(app: &AppHandle<R>) -> Result<TranscriptionEngine, String> {
    let pool = pool(app);
    let api_keys = resolve_api_keys(&pool).await?;
    if api_keys.is_empty() {
        return Err(NO_KEY.into());
    }
    let (settings, _) = settings::load(&pool).await.map_err(|e| e.to_string())?;

    let backup = if settings.uses_local_fallback() {
        match load_fallback(app, &settings.fallback_provider).await {
            Ok(local) => Backup::Local(local),
            Err(e) => {
                warn!("Gemini Live: local backup unavailable ({}); holding and retrying instead", e);
                Backup::HoldAndRetry
            }
        }
    } else {
        Backup::HoldAndRetry
    };

    let status_app = app.clone();
    let on_status: StatusCallback = Arc::new(move |status: GeminiLiveStatus| {
        let _ = status_app.emit("gemini-live-status", &status);
    });

    info!(
        "🌐 Initializing Gemini Live transcription ({} key(s), backup: {}, language mode: {}, vocabulary: {} terms, mode: {})",
        api_keys.len(),
        settings.fallback_provider,
        settings.language_mode,
        settings.custom_vocabulary.len(),
        settings.transcription_mode
    );
    let provider = GeminiLiveProvider::new(api_keys, settings.setup_options(), backup, on_status)
        .with_live_check(Arc::new(crate::audio::recording_commands::recording_live));
    Ok(TranscriptionEngine::Provider(Arc::new(provider)))
}

/// Called from the stop path: free the local backup model, if one is used.
pub async fn unload_fallback_engine<R: Runtime>(app: &AppHandle<R>) {
    let fallback_provider = settings::load(&pool(app))
        .await
        .map(|(s, _)| s.fallback_provider)
        .unwrap_or_else(|_| settings::FALLBACK_NONE.into());
    let unloaded = match fallback_provider.as_str() {
        "localWhisper" => {
            let engine = crate::whisper_engine::commands::WHISPER_ENGINE.lock().unwrap().as_ref().cloned();
            match engine {
                Some(e) => e.unload_model().await,
                None => false,
            }
        }
        "parakeet" => {
            let engine = crate::parakeet_engine::commands::PARAKEET_ENGINE.lock().unwrap().as_ref().cloned();
            match engine {
                Some(e) => e.unload_model().await,
                None => false,
            }
        }
        _ => return,
    };
    info!("Gemini Live: backup {} model unloaded: {}", fallback_provider, unloaded);
}
