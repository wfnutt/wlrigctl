use std::env;
use std::path::PathBuf;
use config::{Config, ConfigError, File};
use serde_derive::Deserialize;

use crate::wsjtx::WsjtxSettings;

#[derive(Debug, Deserialize)]
pub struct Performance {
    pub interval: u64,
}

// XXX: Probably these settings should move to wavelog.rs
#[derive(Debug, Deserialize)]
pub struct Wavelog {
    pub url: String,
    pub key: String,
    pub identifier: String,
}

// XXX: Move to flrig.rs ?
#[derive(Debug, Deserialize)]
pub struct Flrig {
    pub host: String,
    pub port: String,
    pub maxpower: String,
    pub cwbandwidth: Option<u32>,
}

// XXX: Move to...? which mod is responsible for CAT, and how is that different to FLrig???
#[derive(Debug, Deserialize)]
pub struct CAT {
    pub host: String,
    pub port: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct Settings {
    pub performance: Performance,
    pub wavelog: Wavelog,
    pub flrig: Flrig,
    pub CAT: CAT,
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
