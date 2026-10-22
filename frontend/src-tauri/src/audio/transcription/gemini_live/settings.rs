// audio/transcription/gemini_live/settings.rs
//
// Gemini Live settings, stored in the `gemini_live_settings` table.

use super::protocol::SetupOptions;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

pub const MAX_VOCABULARY_TERMS: usize = 1000;
pub const MAX_API_KEYS: usize = 10;

/// `fallback_provider` value for cloud-only mode: no local model, failed audio is
/// held and retried until Gemini is reachable again.
pub const FALLBACK_NONE: &str = "none";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiLiveSettings {
    /// "roman" | "devanagari" | "auto"
    pub language_mode: String,
    pub custom_vocabulary: Vec<String>,
    /// "VERBATIM" | "SMART"
    pub transcription_mode: String,
    /// "none" (cloud only) | "parakeet" | "localWhisper"
    pub fallback_provider: String,
}

impl Default for GeminiLiveSettings {
    fn default() -> Self {
        Self {
            language_mode: "roman".into(),
            custom_vocabulary: Vec::new(),
            transcription_mode: "VERBATIM".into(),
            fallback_provider: FALLBACK_NONE.into(),
        }
    }
}

impl GeminiLiveSettings {
    /// Clamp unknown values to defaults and clean the vocabulary list.
    pub fn normalized(mut self) -> Self {
        if !matches!(self.language_mode.as_str(), "roman" | "devanagari" | "auto") {
            self.language_mode = "roman".into();
        }
        if !matches!(self.transcription_mode.as_str(), "VERBATIM" | "SMART") {
            self.transcription_mode = "VERBATIM".into();
        }
        if !matches!(self.fallback_provider.as_str(), FALLBACK_NONE | "parakeet" | "localWhisper") {
            self.fallback_provider = FALLBACK_NONE.into();
        }
        let mut seen = std::collections::HashSet::new();
        self.custom_vocabulary = self
            .custom_vocabulary
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
            .take(MAX_VOCABULARY_TERMS)
            .collect();
        self
    }

    pub fn uses_local_fallback(&self) -> bool {
        self.fallback_provider != FALLBACK_NONE
    }

    /// Language codes were chosen from live tests with Hindi-English speech:
    /// en-IN returns consistent romanized Hinglish, hi-IN consistent Devanagari (English
    /// terms stay in Latin), and auto-detect mixes the two scripts between segments.
    pub fn setup_options(&self) -> SetupOptions {
        let language_codes = match self.language_mode.as_str() {
            "devanagari" => vec!["hi-IN".to_string()],
            "auto" => Vec::new(),
            _ => vec!["en-IN".to_string()],
        };
        SetupOptions {
            language_codes,
            custom_vocabulary: self.custom_vocabulary.clone(),
            mode: Some(self.transcription_mode.clone()),
        }
    }
}

/// Trim, drop empties and duplicates, keep order, cap the count.
pub fn clean_keys(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    keys.into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty() && seen.insert(k.clone()))
        .take(MAX_API_KEYS)
        .collect()
}

/// Settings plus the ordered API keys saved for Gemini Live (primary first).
pub async fn load(pool: &SqlitePool) -> Result<(GeminiLiveSettings, Vec<String>), sqlx::Error> {
    let row: Option<(Option<String>, String, String, String, String, String)> = sqlx::query_as(
        "SELECT api_key, api_keys, language_mode, custom_vocabulary, transcription_mode, fallback_provider \
         FROM gemini_live_settings WHERE id = '1'",
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some((legacy_key, keys, language_mode, vocabulary, transcription_mode, fallback_provider)) => {
            let mut keys: Vec<String> = serde_json::from_str(&keys).unwrap_or_default();
            if keys.is_empty() {
                // Single key saved before multiple keys were supported.
                keys.extend(legacy_key);
            }
            (
                GeminiLiveSettings {
                    language_mode,
                    custom_vocabulary: serde_json::from_str(&vocabulary).unwrap_or_default(),
                    transcription_mode,
                    fallback_provider,
                }
                .normalized(),
                clean_keys(keys),
            )
        }
        None => (GeminiLiveSettings::default(), Vec::new()),
    })
}

async fn ensure_row(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let defaults = GeminiLiveSettings::default();
    sqlx::query(
        "INSERT OR IGNORE INTO gemini_live_settings (id, language_mode, custom_vocabulary, transcription_mode, fallback_provider) \
         VALUES ('1', $1, '[]', $2, $3)",
    )
    .bind(&defaults.language_mode)
    .bind(&defaults.transcription_mode)
    .bind(&defaults.fallback_provider)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn save(pool: &SqlitePool, settings: &GeminiLiveSettings) -> Result<(), sqlx::Error> {
    ensure_row(pool).await?;
    let vocabulary = serde_json::to_string(&settings.custom_vocabulary).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "UPDATE gemini_live_settings SET language_mode = $1, custom_vocabulary = $2, \
         transcription_mode = $3, fallback_provider = $4 WHERE id = '1'",
    )
    .bind(&settings.language_mode)
    .bind(&vocabulary)
    .bind(&settings.transcription_mode)
    .bind(&settings.fallback_provider)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the ordered key list.
pub async fn save_keys(pool: &SqlitePool, keys: Vec<String>) -> Result<Vec<String>, sqlx::Error> {
    ensure_row(pool).await?;
    let keys = clean_keys(keys);
    sqlx::query("UPDATE gemini_live_settings SET api_keys = $1, api_key = NULL WHERE id = '1'")
        .bind(serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into()))
        .execute(pool)
        .await?;
    Ok(keys)
}

/// The Gemini key saved for summaries (Settings → Summary), used when no
/// transcription-specific key is set.
pub async fn summary_gemini_key(pool: &SqlitePool) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT geminiApiKey FROM settings WHERE id = '1' LIMIT 1")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
        .filter(|k| !k.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_modes_map_to_codes() {
        let mut s = GeminiLiveSettings::default();
        assert_eq!(s.setup_options().language_codes, vec!["en-IN"]);
        s.language_mode = "devanagari".into();
        assert_eq!(s.setup_options().language_codes, vec!["hi-IN"]);
        s.language_mode = "auto".into();
        assert!(s.setup_options().language_codes.is_empty());
    }

    #[test]
    fn normalizes_values_and_vocabulary() {
        let s = GeminiLiveSettings {
            language_mode: "klingon".into(),
            custom_vocabulary: vec![" Meetily ".into(), "".into(), "meetily".into(), "BigQuery".into()],
            transcription_mode: "loud".into(),
            fallback_provider: "deepgram".into(),
        }
        .normalized();
        assert_eq!(s.language_mode, "roman");
        assert_eq!(s.transcription_mode, "VERBATIM");
        assert_eq!(s.fallback_provider, FALLBACK_NONE);
        assert_eq!(s.custom_vocabulary, vec!["Meetily", "BigQuery"]);
    }

    #[test]
    fn caps_vocabulary_size() {
        let s = GeminiLiveSettings {
            custom_vocabulary: (0..1500).map(|i| format!("term{}", i)).collect(),
            ..Default::default()
        }
        .normalized();
        assert_eq!(s.custom_vocabulary.len(), MAX_VOCABULARY_TERMS);
    }

    #[test]
    fn cleans_keys() {
        let keys = clean_keys(vec![" a ".into(), "".into(), "b".into(), "a".into()]);
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn save_and_load_through_real_migrations() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        assert_eq!(load(&pool).await.unwrap(), (GeminiLiveSettings::default(), vec![]));

        let s = GeminiLiveSettings {
            language_mode: "devanagari".into(),
            custom_vocabulary: vec!["Meetily".into()],
            transcription_mode: "SMART".into(),
            fallback_provider: "localWhisper".into(),
        };
        save(&pool, &s).await.unwrap();
        save_keys(&pool, vec!["key-1".into(), "key-2".into()]).await.unwrap();
        assert_eq!(load(&pool).await.unwrap(), (s.clone(), vec!["key-1".to_string(), "key-2".to_string()]));

        // Saving settings keeps the keys.
        save(&pool, &GeminiLiveSettings::default()).await.unwrap();
        assert_eq!(load(&pool).await.unwrap().1.len(), 2);

        // A single key saved by the first version is picked up as the primary.
        save_keys(&pool, vec![]).await.unwrap();
        sqlx::query("UPDATE gemini_live_settings SET api_key = 'legacy' WHERE id = '1'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(load(&pool).await.unwrap().1, vec!["legacy"]);

        // Falls back to the summary Gemini key.
        assert_eq!(summary_gemini_key(&pool).await, None);
        sqlx::query("INSERT INTO settings (id, provider, model, whisperModel, geminiApiKey) VALUES ('1', 'ollama', 'm', 'w', 'summary-key')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(summary_gemini_key(&pool).await, Some("summary-key".into()));
    }
}
