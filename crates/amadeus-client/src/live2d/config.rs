use std::path::{Path, PathBuf};

use crate::core::error::{AppError, AppResult};

const PREFERRED_MODELS: &[&str] = &["amadeus expressions.model3.json", "amadeusV1.model3.json"];

#[derive(Clone, Debug)]
pub struct Live2dPaths {
    pub model_path: PathBuf,
}

impl Live2dPaths {
    pub fn discover(assets_root: &Path) -> AppResult<Self> {
        let model_root = assets_root.join("model");

        if !model_root.is_dir() {
            return Err(AppError::InvalidWorkspaceLayout {
                manifest_dir: assets_root.to_path_buf(),
            });
        }

        let model_path = PREFERRED_MODELS
            .iter()
            .map(|name| model_root.join(name))
            .find(|path| path.exists())
            .ok_or_else(|| AppError::MissingModel {
                model_root: model_root.clone(),
            })?;

        Ok(Self { model_path })
    }
}
