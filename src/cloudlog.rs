use reqwest::Client;
use serde::Serialize;

#[derive(Serialize)]
pub struct RadioData {
    pub key: String,
    pub radio: String,
    pub frequency: String,
    pub mode: String,
    pub power: String,
}

#[tokio::main]
pub async fn upload(url: &str, radio_data: &RadioData) {
    let _res = Client::new()
        .post(url)
        .json(&radio_data)
        .send()
        .await
        .unwrap();
}
