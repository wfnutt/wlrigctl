mod cloudlog;
mod flrig;
mod settings;

use std::{thread, time::Duration};
use log::debug;

use crate::cloudlog::RadioData;
use settings::Settings;

fn main() {
    env_logger::init();

    debug!("clrigctl started.\n");

    let settings = Settings::new().expect("Could not read settings.");

    let mut radio_data_current = RadioData {
        key: settings.cloudlog.key,
        radio: settings.cloudlog.identifier,
        frequency: String::from("14017000"),
        mode: String::from("CW"),
        power: String::from("5"),
    };

    let mut changes_detected = false;

    loop {
        let radio_data_new = match flrig::get_radio_data(&settings.flrig.host, &settings.flrig.port)
        {
            Ok(res) => res,
            Err(_) => {
                thread::sleep(Duration::from_secs(3));
                continue;
            }
        };

        if radio_data_current.frequency != radio_data_new.frequency
            || radio_data_current.mode != radio_data_new.mode
            || radio_data_current.power != radio_data_new.power
        {
            changes_detected = true;
            radio_data_current.frequency = radio_data_new.frequency;
            radio_data_current.mode = radio_data_new.mode;
            radio_data_current.power = radio_data_new.power;
        }

        if changes_detected {
            cloudlog::upload(&settings.cloudlog.url, &radio_data_current);
            changes_detected = false;
        }

        thread::sleep(Duration::from_secs(3));
    }
}
