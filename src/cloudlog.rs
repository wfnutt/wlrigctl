use reqwest::{Client, Error};
use serde::Serialize;

#[derive(Serialize)]
pub struct RadioData {
    pub key: String,
    pub radio: String,
    pub frequency: String,
    pub mode: String,
    pub power: String,
}

pub async fn upload(url: &str, radio_data: &RadioData) -> Result<(), Error> {
    let client = Client::new();

    client.post(url)
        .json(&radio_data)
        .send()
        .await?;

    Ok(())
}
