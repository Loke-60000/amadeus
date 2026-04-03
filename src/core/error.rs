use std::path::PathBuf;

use thiserror::Error;

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
    #[error("the Linux webview container could not be created")]
    MissingGtkContainer,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Tao(#[from] tao::error::OsError),
    #[error(transparent)]
    Wry(#[from] wry::Error),
    #[error(transparent)]
    Http(#[from] wry::http::Error),
}

pub type AppResult<T> = Result<T, AppError>;
