//! Removed backend cloud speech-to-text path.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};

use crate::openhuman::config::Config;
use crate::rpc::RpcOutcome;

/// Maximum base64 audio input length (~25MB base64 → ~18MB decoded audio).
/// Prevents OOM on unreasonably large payloads (multi-hour recordings).
const MAX_AUDIO_BASE64_LEN: usize = 33_554_432;

/// Default model id sent to the backend. The backend's controller currently
/// resolves this to whichever provider it has configured for audio
/// transcription (today: GMI Whisper). Callers can override.
const DEFAULT_MODEL: &str = "whisper-v1";

/// Caller-tunable knobs.
#[derive(Debug, Default, Clone)]
pub struct CloudTranscribeOptions {
    pub model: Option<String>,
    pub language: Option<String>,
    pub mime_type: Option<String>,
    /// Original file name hint (e.g. `audio.webm`). Some upstream providers
    /// sniff the extension; without one we fall back to `audio.webm`.
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudTranscribeResult {
    pub text: String,
}

pub async fn transcribe_cloud(
    _config: &Config,
    audio_base64: &str,
    opts: &CloudTranscribeOptions,
) -> Result<RpcOutcome<CloudTranscribeResult>, String> {
    let trimmed = audio_base64.trim();
    if trimmed.is_empty() {
        return Err("audio_base64 is required".to_string());
    }
    if trimmed.len() > MAX_AUDIO_BASE64_LEN {
        return Err(format!(
            "audio_base64 exceeds maximum size ({}MB)",
            MAX_AUDIO_BASE64_LEN / 1_048_576
        ));
    }
    let audio_bytes = BASE64
        .decode(trimmed)
        .map_err(|e| format!("invalid base64 audio: {e}"))?;
    if audio_bytes.is_empty() {
        return Err("decoded audio is empty".to_string());
    }
    let _model = opts
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    Err("cloud STT is unavailable in this build; use provider `whisper`".to_string())
}
