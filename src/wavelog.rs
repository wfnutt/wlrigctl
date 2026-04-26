use crate::flrig;
use log::{debug, info};
use reqwest::{Client, Error};
use serde::Serialize;
use serde_derive::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

// settings from .toml file
#[derive(Debug, Deserialize, Clone)]
pub struct WavelogSettings {
    pub url: String,
    pub qso_url: String,
    pub key: String,
    pub identifier: String,
    pub station_profile_id: u32,
    pub interval: u64,
    /// URL of this daemon's CAT HTTP server.  When set, it is included in every
    /// live-radio POST so Wavelog can auto-register the CAT callback and show a
    /// "QSY" button in the bandmap without any manual configuration.
    pub cat_url: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct RadioData {
    pub key: String,
    pub radio: String,
    pub frequency: String,
    pub mode: String,
    pub power: String,
    /// Omitted from JSON when absent so existing Wavelog installs that don't
    /// know about the field are not confused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat_url: Option<String>,
}

async fn upload_live_radio_data(
    client: &Client,
    settings: &WavelogSettings,
    radio_data: &RadioData,
) -> Result<(), Error> {
    client.post(&settings.url).json(radio_data).send().await?;

    Ok(())
}

pub async fn upload_wsjtx_qso_data(
    client: &Client,
    settings: &WavelogSettings,
    adif_text: String,
) -> Result<(), Error> {
    let qso_data: Value = json!({
        "key": &settings.key,
        "station_profile_id": settings.station_profile_id,
        "type": "adif",
        "string": adif_text
    });

    client
        .post(&settings.qso_url)
        .json(&qso_data)
        .send()
        .await?;

    Ok(())
}

pub fn wavelog_thread(
    settings: WavelogSettings,
    rig_poll: Arc<flrig::FLRig>,
    token: CancellationToken,
    ws_tx: broadcast::Sender<Arc<RadioData>>,
    cat_token: Arc<String>,
) {
    let cat_url = settings
        .cat_url
        .as_ref()
        .map(|url| format!("{}/{}", url.trim_end_matches('/'), cat_token));
    let mut radio_data_current = RadioData {
        key: settings.key.clone(),
        radio: settings.identifier.clone(),
        frequency: String::from(""),
        mode: String::from(""),
        power: String::from("0"),
        cat_url,
    };

    tokio::task::spawn(async move {
        let client = Client::new();
        loop {
            match rig_poll.get_radio_data().await {
                Ok(Some(radio_data_new)) => {
                    if radio_data_current.frequency != radio_data_new.frequency
                        || radio_data_current.mode != radio_data_new.mode
                        || radio_data_current.power != radio_data_new.power
                    {
                        radio_data_current.frequency = radio_data_new.frequency;
                        radio_data_current.mode = radio_data_new.mode;
                        radio_data_current.power = radio_data_new.power;

                        if let Err(e) =
                            upload_live_radio_data(&client, &settings, &radio_data_current).await
                        {
                            debug!("Wavelog upload failed (may be transient): {e}");
                        }
                        // Broadcast to WebSocket clients; ignored if no listeners.
                        let _ = ws_tx.send(Arc::new(radio_data_current.clone()));
                    }
                }
                Ok(None) => {} // FLRig reports nothing changed; skip this cycle
                Err(e) => info!("Got err:{:#?}", e),
            }

            tokio::select! {
                _ = token.cancelled() => {
                    info!("wavelog thread shutting down");
                    return;
                }
                _ = tokio::time::sleep(Duration::from_millis(settings.interval)) => {}
            }
        }
    });
}
