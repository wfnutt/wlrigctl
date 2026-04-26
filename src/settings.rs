use config::{Config, ConfigError, File};
use home::home_dir;
use serde_derive::Deserialize;
use std::env;
use std::path::PathBuf;

use crate::cat::CatSettings;
use crate::flrig::FlrigSettings;
use crate::wavelog::WavelogSettings;
use crate::ws::WsSettings;
use crate::wsjtx::WsjtxSettings;

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub wavelog: WavelogSettings,
    pub flrig: FlrigSettings,
    #[serde(rename = "CAT")]
    pub cat: CatSettings,
    #[serde(rename = "WSJTX")]
    pub wsjtx: WsjtxSettings,
    /// WebSocket server settings.  The [websocket] section is optional; when
    /// absent all defaults apply (127.0.0.1:54323, self-signed TLS cert).
    #[serde(default)]
    pub websocket: WsSettings,
}

impl Settings {
    /// Returns the XDG-aware config directory for this application,
    /// e.g. `~/.config/wlrigctl`.  Used by callers that need to store
    /// auxiliary files (WebSocket TLS cert, etc.) alongside the config.
    pub fn config_dir() -> Result<PathBuf, ConfigError> {
        let home =
            home_dir().ok_or_else(|| ConfigError::Message("No home directory found".into()))?;
        let app_name = env!("CARGO_PKG_NAME");
        let base_dir: PathBuf = match env::var("XDG_CONFIG_HOME") {
            Ok(val) => val.into(),
            Err(_) => home.join(".config"),
        };
        Ok(base_dir.join(app_name))
    }

    pub fn new() -> Result<Self, ConfigError> {
        let config_file = Self::config_dir()?.join("config.toml");

        let config_path = config_file
            .to_str()
            .ok_or_else(|| ConfigError::Message("Config path not valid UTF-8".into()))?;

        let settings = Config::builder()
            .add_source(File::with_name(config_path))
            .build()?;

        settings.try_deserialize()
    }
}
