// audio/transcription/gemini_live/protocol.rs
//
// JSON messages for the Gemini Live (BidiGenerateContent) WebSocket, limited to what
// transcription needs. Behaviour notes below were verified against
// gemini-3.5-transcribe-live:
// - with automatic activity detection disabled, each segment sent between
//   `activityStart` and `activityEnd` is acknowledged with ACTIVITY_START, then any
//   `inputTranscription` finals and `generationComplete`, then ACTIVITY_END; a segment
//   without speech gets only the two acknowledgements;
// - with automatic detection, short utterances were often left unanswered after
//   `audioStreamEnd` until more audio arrived (and sometimes dropped);
// - auth failures close the socket during setup with 1007 (bad key) or 1008 (missing key);
// - the server sends goAway (timeLeft "50s") at 9:00 and closes with 1008 at ~9:50;
// - sessionResumption is accepted but no resumption handles are sent.

use base64::Engine;
use serde_json::{json, Value};

pub const MODEL: &str = "gemini-3.5-transcribe-live";
pub const WS_URL: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

#[derive(Debug, Clone, Default)]
pub struct SetupOptions {
    /// BCP-47 codes; empty = automatic detection.
    pub language_codes: Vec<String>,
    pub custom_vocabulary: Vec<String>,
    /// "VERBATIM" or "SMART"; None leaves the server default (VERBATIM).
    pub mode: Option<String>,
}

pub fn setup_message(opts: &SetupOptions) -> String {
    let mut transcription = json!({ "languageCodes": opts.language_codes });
    if !opts.custom_vocabulary.is_empty() {
        transcription["customVocabulary"] = json!(opts.custom_vocabulary);
    }
    if let Some(mode) = &opts.mode {
        transcription["mode"] = json!(mode);
    }
    json!({
        "setup": {
            "model": format!("models/{}", MODEL),
            "generationConfig": { "responseModalities": ["TEXT"] },
            "inputAudioTranscription": transcription,
            // Segment boundaries come from the app's VAD via activityStart/activityEnd.
            "realtimeInputConfig": { "automaticActivityDetection": { "disabled": true } },
        }
    })
    .to_string()
}

pub fn audio_message(pcm_s16le: &[u8]) -> String {
    json!({
        "realtimeInput": {
            "audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(pcm_s16le),
                "mimeType": "audio/pcm;rate=16000",
            }
        }
    })
    .to_string()
}

pub fn activity_start_message() -> String {
    json!({ "realtimeInput": { "activityStart": {} } }).to_string()
}

pub fn activity_end_message() -> String {
    json!({ "realtimeInput": { "activityEnd": {} } }).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    SetupComplete,
    Interim(String),
    Final(String),
    /// `generationComplete` / `turnComplete`: the server finished the current utterance.
    TurnDone,
    ActivityStart,
    ActivityEnd,
    GoAway { time_left: Option<String> },
    Closed { code: Option<u16>, reason: String },
}

/// Parse one server frame. A single frame can carry several events.
pub fn parse_server_message(raw: &str) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    let Ok(msg) = serde_json::from_str::<Value>(raw) else {
        return events;
    };

    if msg.get("setupComplete").is_some() {
        events.push(ServerEvent::SetupComplete);
    }
    if let Some(sc) = msg.get("serverContent") {
        if let Some(text) = sc.pointer("/interimInputTranscription/text").and_then(Value::as_str) {
            events.push(ServerEvent::Interim(text.to_string()));
        }
        if let Some(text) = sc.pointer("/inputTranscription/text").and_then(Value::as_str) {
            events.push(ServerEvent::Final(text.to_string()));
        }
        let done = |k: &str| sc.get(k).and_then(Value::as_bool).unwrap_or(false);
        if done("generationComplete") || done("turnComplete") {
            events.push(ServerEvent::TurnDone);
        }
    }
    match msg.pointer("/voiceActivity/type").and_then(Value::as_str) {
        Some("ACTIVITY_START") => events.push(ServerEvent::ActivityStart),
        Some("ACTIVITY_END") => events.push(ServerEvent::ActivityEnd),
        _ => {}
    }
    if let Some(go_away) = msg.get("goAway") {
        events.push(ServerEvent::GoAway {
            time_left: go_away.get("timeLeft").and_then(Value::as_str).map(str::to_string),
        });
    }
    events
}

/// Close codes that mean "retrying will not help" when received during setup
/// (invalid/missing API key, bad setup).
pub fn is_permanent_close(code: Option<u16>) -> bool {
    matches!(code, Some(1007) | Some(1008))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_includes_options() {
        let msg = setup_message(&SetupOptions {
            language_codes: vec!["en-IN".into()],
            custom_vocabulary: vec!["Meetily".into()],
            mode: Some("SMART".into()),
        });
        let v: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["setup"]["model"], "models/gemini-3.5-transcribe-live");
        assert_eq!(v["setup"]["inputAudioTranscription"]["languageCodes"][0], "en-IN");
        assert_eq!(v["setup"]["inputAudioTranscription"]["customVocabulary"][0], "Meetily");
        assert_eq!(v["setup"]["inputAudioTranscription"]["mode"], "SMART");
        assert_eq!(v["setup"]["realtimeInputConfig"]["automaticActivityDetection"]["disabled"], true);
    }

    #[test]
    fn setup_omits_empty_vocabulary() {
        let v: Value = serde_json::from_str(&setup_message(&SetupOptions::default())).unwrap();
        assert!(v["setup"]["inputAudioTranscription"].get("customVocabulary").is_none());
        assert_eq!(v["setup"]["inputAudioTranscription"]["languageCodes"], json!([]));
    }

    #[test]
    fn parses_real_server_frames() {
        assert_eq!(parse_server_message(r#"{"setupComplete": {}}"#), vec![ServerEvent::SetupComplete]);
        assert_eq!(
            parse_server_message(r#"{"serverContent": {"inputTranscription": {"text": "Okay team."}}}"#),
            vec![ServerEvent::Final("Okay team.".into())]
        );
        assert_eq!(
            parse_server_message(r#"{"serverContent": {"generationComplete": true}}"#),
            vec![ServerEvent::TurnDone]
        );
        assert_eq!(
            parse_server_message(r#"{"serverContent": {}, "voiceActivity": {"type": "ACTIVITY_START", "audioOffset": "2.400s"}}"#),
            vec![ServerEvent::ActivityStart]
        );
        assert_eq!(
            parse_server_message(r#"{"goAway": {"timeLeft": "50s"}}"#),
            vec![ServerEvent::GoAway { time_left: Some("50s".into()) }]
        );
        assert!(parse_server_message("not json").is_empty());
    }

    #[test]
    fn auth_close_codes_are_permanent() {
        assert!(is_permanent_close(Some(1007)));
        assert!(is_permanent_close(Some(1008)));
        assert!(!is_permanent_close(Some(1011)));
        assert!(!is_permanent_close(None));
    }
}
