use config::{Config, ConfigError, File};
use serde_derive::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Performance {
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
pub struct Cloudlog {
    pub url: String,
    pub key: String,
    pub identifier: String,
}

#[derive(Debug, Deserialize)]
pub struct Flrig {
    pub host: String,
    pub port: String,
    pub maxpower: String,
}

#[derive(Debug, Deserialize)]
pub struct CAT {
    pub host: String,
    pub port: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct Settings {
    pub performance: Performance,
    pub cloudlog: Cloudlog,
    pub flrig: Flrig,
    pub CAT: CAT,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let home_dir = home::home_dir()
            .ok_or_else(|| ConfigError::Message("No home directory found".into()))?;
        let mut config_file = home_dir.into_os_string();
        config_file.push("/.config/clrigctl.toml");

        let config_path = config_file.to_str()
            .ok_or_else(|| ConfigError::Message("Config path not valid UTF-8".into()))?;

        let settings = Config::builder()
            .add_source(File::with_name(config_path))
            .build()?;

        settings.try_deserialize()
    }
}
