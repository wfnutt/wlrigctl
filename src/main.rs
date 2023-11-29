mod cloudlog;
mod settings;

use settings::Settings;
use crate::cloudlog::RadioData;

fn main() {
    let settings = Settings::new().expect("Could not read settings.");

    let rd = RadioData {
        key: settings.cloudlog.key,
        radio: settings.cloudlog.identifier,
        frequency: String::from("14017000"),
        mode: String::from("CW"),
        power: String::from("5"),
    };

    cloudlog::upload(&settings.cloudlog.url, &rd);
}
