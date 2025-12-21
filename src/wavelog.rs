use std::sync::Arc;
use reqwest::{Client, Error};
use serde::Serialize;
use serde_json::{json, Value};
use serde_derive::Deserialize;
use tokio::time::Duration;
use log::info;
use crate::flrig;

// settings from .toml file
#[derive(Debug, Deserialize, Clone)]
pub struct WavelogSettings {
    pub url: String,
    pub qso_url: String,
    pub key: String,
    pub identifier: String,
    pub station_profile_id: u32,
    pub interval: u64,
}

#[derive(Serialize)]
pub struct RadioData {
    pub key: String,
    pub radio: String,
    pub frequency: String,
    pub mode: String,
    pub power: String,
}

async fn upload_live_radio_data(settings: WavelogSettings, radio_data: &RadioData)
-> Result<(), Error> {

    let client = Client::new();

    client.post(settings.url.clone())
        .json(&radio_data)
        .send()
        .await?;

    Ok(())
}

pub async fn upload_wsjtx_qso_data(settings: WavelogSettings, adif_text: String)
-> Result<(), Error> {

    let client = Client::new();

    let qso_data: Value = json!({
        "key": settings.key.clone(),
        "station_profile_id": settings.station_profile_id.clone(),
        "type": "adif",
        "string": adif_text
    });

    client.post(settings.qso_url.clone())
        .json(&qso_data)
        .send()
        .await?;

    Ok(())
}

pub fn wavelog_thread(settings: WavelogSettings, rig_poll: Arc<flrig::FLRig>) {

    let mut radio_data_current = RadioData {
        key: settings.key.clone(),
        radio: settings.identifier.clone(),
        frequency: String::from(""),
        mode: String::from(""),
        power: String::from("0"),
    };

    tokio::task::spawn(async move {
        loop {
            // MIGHT be able to call rig.get_update() here; it'll return NIL if nothing changed
            // XXX: FIXME
            // We should also aim to reuse the single TCP connection for repeated requests, rather
            // than a new TCP socket request for every poll (Yuck!)
            //
            // If get_update() says somthing happened, try using system.multicall() to get multiple
            // fields from flrig in one go.
            //
            // NOTE that we might also need to do an initial start-of-day rig.get_info() to
            // establish initial data
            match rig_poll.get_radio_data().await {
                Ok(radio_data_new) => {
                    if radio_data_current.frequency != radio_data_new.frequency
                        || radio_data_current.mode != radio_data_new.mode
                        || radio_data_current.power != radio_data_new.power
                    {
                        radio_data_current.frequency = radio_data_new.frequency;
                        radio_data_current.mode = radio_data_new.mode;
                        radio_data_current.power = radio_data_new.power;

                        // If attempt to push VFO info to wavelog fails this time,
                        // maybe the failure might be transient, and we should try next time
                        let _result = upload_live_radio_data(settings.clone(), &radio_data_current)
                            .await;
                    }
                }
                Err(e) => info!("Got err:{:#?}", e),
            }

            tokio::time::sleep(Duration::from_millis(settings.interval)).await;
        }
    });
}
