mod cloudlog;
mod settings;
mod flrig;

use settings::Settings;
use crate::cloudlog::RadioData;

fn main() {
    let settings = Settings::new().expect("Could not read settings.");

    let mut radio_data_current = RadioData {
        key: settings.cloudlog.key,
        radio: settings.cloudlog.identifier,
        frequency: String::from("14017000"),
        mode: String::from("CW"),
        power: String::from("5"),
    };

    let radio_data_new = flrig::get_radio_data(&settings.flrig.host, &settings.flrig.port).unwrap();

    radio_data_current.frequency = radio_data_new.frequency;
    radio_data_current.mode = radio_data_new.mode;
    radio_data_current.power = radio_data_new.power;

    cloudlog::upload(&settings.cloudlog.url, &radio_data_current);
}
