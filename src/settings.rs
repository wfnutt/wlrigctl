use std::env;
use std::path::PathBuf;
use config::{Config, ConfigError, File};
use serde_derive::Deserialize;

use crate::wavelog::WavelogSettings;
use crate::flrig::FlrigSettings;
use crate::cat::CatSettings;
use crate::wsjtx::WsjtxSettings;

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct Settings {
    pub wavelog: WavelogSettings,
    pub flrig: FlrigSettings,
    pub CAT: CatSettings,
    pub WSJTX: WsjtxSettings,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {

        let home_dir = env::home_dir()
            .ok_or_else(|| ConfigError::Message("No home directory found".into()))?;

        let app_name = env!("CARGO_PKG_NAME");

        let base_dir: PathBuf = match env::var("XDG_CONFIG_HOME") {
            Ok(val) => val.into(),
            Err(_) => home_dir.join(".config"),
        };

        let config_file = base_dir.join(app_name).join("config.toml");

        let config_path = config_file.to_str()
            .ok_or_else(|| ConfigError::Message("Config path not valid UTF-8".into()))?;

        let settings = Config::builder()
            .add_source(File::with_name(config_path))
            .build()?;

        settings.try_deserialize()
    }
}
