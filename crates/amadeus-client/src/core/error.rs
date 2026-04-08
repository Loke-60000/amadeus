use std::path::PathBuf;

use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum AppError {
    #[error("unable to resolve the workspace layout from {manifest_dir}")]
    InvalidWorkspaceLayout { manifest_dir: PathBuf },
    #[error("no supported model3.json file was found in {model_root}")]
    MissingModel { model_root: PathBuf },
    #[error("invalid TTS request: {reason}")]
    InvalidTtsRequest { reason: String },
    #[error("unsupported TTS speaker: {speaker}")]
    UnsupportedTtsSpeaker { speaker: String },
    #[error("unsupported TTS language: {language}")]
    UnsupportedTtsLanguage { language: String },
    #[error("the local Christina TTS service is disabled")]
    TtsDisabled,
    #[error("the local TTS runtime is unavailable: {reason}")]
    TtsRuntimeUnavailable { reason: String },
    #[error("the local TTS synthesis request failed: {reason}")]
    TtsSynthesisFailed { reason: String },
    #[error("the local STT runtime is unavailable: {reason}")]
    SttRuntimeUnavailable { reason: String },
    #[error("the local STT transcription failed: {reason}")]
    SttTranscriptionFailed { reason: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type AppResult<T> = Result<T, AppError>;
