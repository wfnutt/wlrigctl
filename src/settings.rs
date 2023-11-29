use config::{Config, ConfigError, File};
use home;
use serde_derive::Deserialize;

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct Cloudlog {
    url: String,
    key: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct Flrig {
    host: String,
    port: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
pub struct Settings {
    cloudlog: Cloudlog,
    flrig: Flrig,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let mut config_file = home::home_dir().unwrap().into_os_string();
        config_file.push("/.config/clrigctl.toml");

        let settings = Config::builder()
            .add_source(File::with_name(config_file.to_str().unwrap()))
            .build()?;

        settings.try_deserialize()
    }
}
