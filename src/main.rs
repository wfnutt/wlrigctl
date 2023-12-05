mod cloudlog;
mod flrig;
mod settings;

use log::debug;
use std::{process, thread, time::Duration};

use crate::cloudlog::RadioData;
use settings::Settings;

fn main() {
    env_logger::init();

    debug!("clrigctl started.\n");

    let settings = Settings::new().unwrap_or_else(|err| {
        eprintln!("Could not read settings: {}", err);
        process::exit(1)
    });

    let mut radio_data_current = RadioData {
        key: settings.cloudlog.key,
        radio: settings.cloudlog.identifier,
        frequency: String::from(""),
        mode: String::from(""),
        //power: String::from(&settings.power),
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
        //|| radio_data_current.power != radio_data_new.power
        {
            changes_detected = true;
            radio_data_current.frequency = radio_data_new.frequency;
            radio_data_current.mode = radio_data_new.mode;
            //radio_data_current.power = radio_data_new.power;
        }

        if changes_detected {
            cloudlog::upload(&settings.cloudlog.url, &radio_data_current);
            changes_detected = false;
        }

        thread::sleep(Duration::from_secs(3));
    }
}
