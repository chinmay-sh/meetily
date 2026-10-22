// audio/transcription/gemini_live/provider.rs
//
// TranscriptionProvider backed by Gemini Live, with session rotation, multiple API keys
// and two ways of handling outages.
//
// - One WebSocket session is reused across VAD segments (keeps language context).
// - Sessions are rotated between segments before the ~10 minute connection cap, or as
//   soon as the server sends goAway. The new session is primed with the last
//   PRIME_SECS of the previous segment (overlapping audio) and the first result on it
//   is de-duplicated against the previously emitted text.
// - A segment that fails mid-stream is retried once on a fresh session; text from the
//   failed attempt is discarded, so nothing is emitted twice.
// - Keys are tried in order. When the key in use fails for any reason, the same segment
//   is retried immediately with the next key. Rejected keys (invalid/missing) are skipped
//   for the rest of the recording; keys with other errors (rate limits, quota, network)
//   are tried last for KEY_COOLDOWN.
// - When every key fails:
//   - Backup::Local: the segment is transcribed by the local engine;
//     MAX_CONSECUTIVE_FAILURES in a row pause Gemini for LOCAL_COOLDOWN.
//   - Backup::HoldAndRetry (cloud only): the segment is held and retried with backoff
//     while later segments queue up behind it, so the transcript catches up when Gemini
//     returns. After the recording stops, retrying gives up after RetryPolicy::after_stop.

use super::connection::{LiveConnection, LiveError, PACE_FACTOR};
use super::dedup::trim_overlap;
use super::protocol::{SetupOptions, MODEL};
use crate::audio::transcription::provider::{TranscriptionError, TranscriptionProvider, TranscriptResult};
use async_trait::async_trait;
use log::{info, warn};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Rotate before the server's goAway, which arrives at 9:00 (connection closes at ~9:50).
const ROTATE_AFTER: Duration = Duration::from_secs(8 * 60 + 30);
const PRIME_SECS: f64 = 2.0;
const MIN_PRIME_SAMPLES: usize = 8000; // 0.5 s
const KEY_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const LOCAL_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Status notices for the UI (key switches, outages, fallback).
pub type StatusCallback = Arc<dyn Fn(GeminiLiveStatus) + Send + Sync>;
/// Whether the recording is still live (false once Stop was pressed).
pub type LiveCheck = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Debug, Clone, serde::Serialize)]
pub struct GeminiLiveStatus {
    /// "key_switch" | "outage" | "recovered" | "gave_up" | "fallback" | "resumed" | "disabled"
    pub kind: &'static str,
    pub message: String,
}

/// What to do when every key fails.
pub enum Backup {
    Local(Arc<dyn TranscriptionProvider>),
    HoldAndRetry,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    /// How long to keep retrying held audio after the recording stopped.
    pub after_stop: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            after_stop: Duration::from_secs(3 * 60),
        }
    }
}

enum KeysError {
    /// Every key was rejected; retrying cannot help.
    AllRejected(String),
    /// At least one key failed for a reason that may go away (network, quota, server).
    Failed(String),
}

#[derive(Default, Clone)]
struct KeySlot {
    rejected: bool,
    cooling_until: Option<Instant>,
}

#[derive(Default)]
struct State {
    conn: Option<LiveConnection>,
    had_connection: bool,
    keys: Vec<KeySlot>,
    current: usize,
    last_text: String,
    tail_audio: Vec<f32>,
    dedup_next: bool,
    /// Late text from a retired session, prepended to the next result.
    carry_text: String,
    // Backup::Local
    consecutive_failures: u32,
    paused_until: Option<Instant>,
    rejected_notified: bool,
    // Backup::HoldAndRetry
    outage_since: Option<Instant>,
    stopped_at: Option<Instant>,
    gave_up: bool,
}

pub struct GeminiLiveProvider {
    endpoint: String,
    api_keys: Vec<String>,
    setup: SetupOptions,
    backup: Backup,
    retry: RetryPolicy,
    is_live: LiveCheck,
    on_status: StatusCallback,
    state: Mutex<State>,
}

fn key_hint(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{}", tail)
}

impl GeminiLiveProvider {
    pub fn new(api_keys: Vec<String>, setup: SetupOptions, backup: Backup, on_status: StatusCallback) -> Self {
        let state = State { keys: vec![KeySlot::default(); api_keys.len()], ..Default::default() };
        Self {
            endpoint: super::protocol::WS_URL.to_string(),
            api_keys,
            setup,
            backup,
            retry: RetryPolicy::default(),
            is_live: Arc::new(|| true),
            on_status,
            state: Mutex::new(state),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_live_check(mut self, is_live: LiveCheck) -> Self {
        self.is_live = is_live;
        self
    }

    fn notify(&self, kind: &'static str, message: String) {
        info!("Gemini Live status [{}]: {}", kind, message);
        (self.on_status)(GeminiLiveStatus { kind, message });
    }

    fn backup_name(&self) -> &'static str {
        match &self.backup {
            Backup::Local(f) => f.provider_name(),
            Backup::HoldAndRetry => "none",
        }
    }

    async fn ensure_connection(&self, st: &mut State) -> Result<(), LiveError> {
        let reason = match st.conn.as_mut() {
            None => "no session",
            Some(c) => {
                if !c.check_usable() {
                    "server ended session"
                } else if c.age() >= ROTATE_AFTER {
                    "session age limit"
                } else {
                    return Ok(());
                }
            }
        };
        if st.had_connection {
            info!("🔄 Gemini Live: opening new session ({}, key #{})", reason, st.current + 1);
        }
        // Make before break: the old session (if any) stays open until the new one is ready.
        let key = &self.api_keys[st.current];
        let mut fresh = LiveConnection::connect_to(&self.endpoint, key, &self.setup).await?;
        if st.had_connection && st.tail_audio.len() >= MIN_PRIME_SAMPLES {
            match fresh.transcribe_segment(&st.tail_audio, PACE_FACTOR).await {
                Ok(_) => st.dedup_next = true,
                Err(LiveError::Auth(m)) => return Err(LiveError::Auth(m)),
                Err(e) => warn!("Gemini Live: priming new session failed ({}); continuing without overlap", e),
            }
        }
        if let Some(mut old) = st.conn.replace(fresh) {
            let late = old.take_carry().join(" ");
            if !late.is_empty() {
                st.carry_text = late;
            }
            old.close_in_background();
        }
        st.had_connection = true;
        Ok(())
    }

    /// One key: connect if needed, stream the segment; a mid-segment failure is retried
    /// once on a fresh session with the same key.
    async fn transcribe_with_key(&self, st: &mut State, audio: &[f32]) -> Result<String, LiveError> {
        let mut attempt = 0;
        loop {
            self.ensure_connection(st).await?;
            let conn = st.conn.as_mut().expect("connection ensured above");
            match conn.transcribe_segment(audio, PACE_FACTOR).await {
                Ok(text) => {
                    let text = if std::mem::take(&mut st.dedup_next) {
                        trim_overlap(&st.last_text, &text)
                    } else {
                        text
                    };
                    let text = match std::mem::take(&mut st.carry_text) {
                        late if late.is_empty() => text,
                        late if text.is_empty() => late,
                        late => format!("{} {}", late, text),
                    };
                    if !text.is_empty() {
                        st.last_text = text.clone();
                    }
                    let keep = (PRIME_SECS * 16000.0) as usize;
                    st.tail_audio = audio[audio.len().saturating_sub(keep)..].to_vec();
                    return Ok(text);
                }
                Err(LiveError::Auth(m)) => return Err(LiveError::Auth(m)),
                Err(e) => {
                    warn!("Gemini Live: segment attempt {} failed on key #{}: {}", attempt + 1, st.current + 1, e);
                    st.conn = None;
                    attempt += 1;
                    if attempt >= 2 {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Try every usable key, starting with the one in use; healthy keys before cooling ones.
    async fn transcribe_with_keys(&self, st: &mut State, audio: &[f32]) -> Result<String, KeysError> {
        let now = Instant::now();
        let n = self.api_keys.len();
        let mut order: Vec<usize> = (0..n).map(|i| (st.current + i) % n).filter(|&i| !st.keys[i].rejected).collect();
        order.sort_by_key(|&i| st.keys[i].cooling_until.map_or(false, |t| t > now));

        let mut last_error = String::from("no usable API key");
        let mut previous_failure: Option<String> = None;
        for i in order {
            if i != st.current {
                if let Some(old) = st.conn.take() {
                    old.close_in_background();
                }
                let from = st.current;
                st.current = i;
                if let Some(reason) = previous_failure.take() {
                    self.notify(
                        "key_switch",
                        format!(
                            "Gemini key #{} failed ({}). Switched to key #{} ({}).",
                            from + 1,
                            reason,
                            i + 1,
                            key_hint(&self.api_keys[i])
                        ),
                    );
                }
            }
            match self.transcribe_with_key(st, audio).await {
                Ok(text) => {
                    st.keys[i].cooling_until = None;
                    return Ok(text);
                }
                Err(LiveError::Auth(m)) => {
                    warn!("Gemini Live: key #{} rejected: {}", i + 1, m);
                    st.keys[i].rejected = true;
                    previous_failure = Some(m.clone());
                    last_error = m;
                }
                Err(LiveError::Transient(m)) => {
                    st.keys[i].cooling_until = Some(Instant::now() + KEY_COOLDOWN);
                    previous_failure = Some(m.clone());
                    last_error = m;
                }
            }
            st.conn = None;
        }
        if st.keys.iter().all(|k| k.rejected) {
            Err(KeysError::AllRejected(last_error))
        } else {
            Err(KeysError::Failed(last_error))
        }
    }

    async fn transcribe_with_local_backup(
        &self,
        local: &Arc<dyn TranscriptionProvider>,
        audio: Vec<f32>,
        language: Option<String>,
    ) -> Result<TranscriptResult, TranscriptionError> {
        let mut st = self.state.lock().await;
        let paused = st.paused_until.map_or(false, |t| Instant::now() < t);
        let any_key = st.keys.iter().any(|k| !k.rejected);
        if any_key && !paused {
            match self.transcribe_with_keys(&mut st, &audio).await {
                Ok(text) => {
                    st.consecutive_failures = 0;
                    if st.paused_until.take().is_some() {
                        self.notify("resumed", "Gemini Live transcription resumed.".into());
                    }
                    return Ok(TranscriptResult { text, confidence: None, is_partial: false });
                }
                Err(KeysError::AllRejected(msg)) => {
                    if !std::mem::replace(&mut st.rejected_notified, true) {
                        self.notify(
                            "disabled",
                            format!(
                                "Gemini rejected all API keys ({}). Using {} for the rest of this recording.",
                                msg,
                                self.backup_name()
                            ),
                        );
                    }
                }
                Err(KeysError::Failed(msg)) => {
                    st.consecutive_failures += 1;
                    if st.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        st.consecutive_failures = 0;
                        st.paused_until = Some(Instant::now() + LOCAL_COOLDOWN);
                        self.notify(
                            "fallback",
                            format!(
                                "Gemini Live keeps failing ({}). Using {} for the next {} minutes.",
                                msg,
                                self.backup_name(),
                                LOCAL_COOLDOWN.as_secs() / 60
                            ),
                        );
                    }
                }
            }
        }
        // While paused, Gemini is retried on the first segment after the cooldown ends.
        drop(st);
        local.transcribe(audio, language).await
    }

    async fn transcribe_holding(&self, audio: Vec<f32>) -> Result<TranscriptResult, TranscriptionError> {
        let empty = || Ok(TranscriptResult { text: String::new(), confidence: None, is_partial: false });
        // Held for the whole call: segments queue up behind this one in order.
        let mut st = self.state.lock().await;
        if st.gave_up {
            return empty();
        }
        let mut delay = self.retry.initial_delay;
        loop {
            match self.transcribe_with_keys(&mut st, &audio).await {
                Ok(text) => {
                    if let Some(since) = st.outage_since.take() {
                        self.notify(
                            "recovered",
                            format!(
                                "Gemini Live is reachable again after {}s. Catching up on the held audio.",
                                since.elapsed().as_secs()
                            ),
                        );
                    }
                    return Ok(TranscriptResult { text, confidence: None, is_partial: false });
                }
                Err(KeysError::AllRejected(msg)) => {
                    st.gave_up = true;
                    self.notify(
                        "disabled",
                        format!(
                            "Gemini rejected all API keys ({}). Transcription stopped for this recording; \
                             the audio is still being recorded. Check your keys in Settings → Transcription.",
                            msg
                        ),
                    );
                    return empty();
                }
                Err(KeysError::Failed(msg)) => {
                    if st.outage_since.is_none() {
                        st.outage_since = Some(Instant::now());
                        self.notify(
                            "outage",
                            format!(
                                "Gemini Live can't be reached ({}). Holding the audio and retrying; \
                                 the transcript will catch up.",
                                msg
                            ),
                        );
                    }
                    if !(self.is_live)() {
                        let stopped_at = *st.stopped_at.get_or_insert_with(Instant::now);
                        if stopped_at.elapsed() >= self.retry.after_stop {
                            st.gave_up = true;
                            self.notify(
                                "gave_up",
                                format!(
                                    "Gemini Live was still unreachable {}s after the recording stopped. \
                                     Some speech was not transcribed; the audio recording is saved.",
                                    self.retry.after_stop.as_secs()
                                ),
                            );
                            return empty();
                        }
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(self.retry.max_delay);
                }
            }
        }
    }
}

#[async_trait]
impl TranscriptionProvider for GeminiLiveProvider {
    async fn transcribe(
        &self,
        audio: Vec<f32>,
        language: Option<String>,
    ) -> Result<TranscriptResult, TranscriptionError> {
        match &self.backup {
            Backup::Local(local) => self.transcribe_with_local_backup(local, audio, language).await,
            Backup::HoldAndRetry => self.transcribe_holding(audio).await,
        }
    }

    async fn is_model_loaded(&self) -> bool {
        true
    }

    async fn get_current_model(&self) -> Option<String> {
        Some(MODEL.to_string())
    }

    fn provider_name(&self) -> &'static str {
        "Gemini Live"
    }
}
