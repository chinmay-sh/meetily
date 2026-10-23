// audio/transcription/gemini_live/diag.rs
//
// Plain-text diagnostics log for Gemini Live (sessions, per-segment timings, errors,
// key switches). The desktop app has no visible console, so this file is what users can
// send when transcription misbehaves. Never contains API keys or transcript text.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILE_NAME: &str = "gemini-live.log";
const MAX_BYTES: u64 = 5 * 1024 * 1024;

static LOG: Mutex<Option<(PathBuf, File)>> = Mutex::new(None);

/// Open (or reopen) the log in `dir`. A log over MAX_BYTES is moved to `.old` first.
pub fn init(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    *LOG.lock().unwrap() = Some((path.clone(), file));
    Ok(path)
}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Append one timestamped line (no-op until `init`).
pub fn line(message: impl AsRef<str>) {
    if let Ok(mut guard) = LOG.lock() {
        if let Some((_, file)) = guard.as_mut() {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let _ = writeln!(file, "{} {}", now, message.as_ref());
        }
    }
}

#[macro_export]
macro_rules! gemini_diag {
    ($($arg:tt)*) => {
        $crate::audio::transcription::gemini_live::diag::line(format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_lines_and_rotates_large_logs() {
        let dir = std::env::temp_dir().join(format!("gemini-diag-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();

        let path = init(&dir).unwrap();
        line("session opened");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.ends_with("session opened\n"), "{}", content);
        assert!(dir.join("gemini-live.log.old").exists(), "oversized log rotated");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
