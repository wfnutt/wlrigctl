use reqwest::{Client, Error};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct RadioData {
    pub key: String,
    pub radio: String,
    pub frequency: String,
    pub mode: String,
    pub power: String,
}

pub async fn upload_live_radio_data(url: &str, radio_data: &RadioData) -> Result<(), Error> {
    let client = Client::new();

    client.post(url)
        .json(&radio_data)
        .send()
        .await?;

    Ok(())
}

pub async fn upload_wsjtx_qso_data(url: &str, qso_data: &Value) -> Result<(), Error> {
    let client = Client::new();

    client.post(url)
        .json(&qso_data)
        .send()
        .await?;

    Ok(())
}
