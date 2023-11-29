use serde::Serialize;
use serde_json;
use reqwest::Client;

#[derive(Serialize)]
pub struct RadioData {
    pub key: String,
    pub radio: String,
    pub frequency : String,
    pub mode: String,
}

#[tokio::main]
pub async fn upload(url: &str, radio_data: &RadioData) {
    let radio_data_json = serde_json::to_string(radio_data).unwrap();

    let res = Client::new().post(url).json(&radio_data_json).send().await.unwrap();

    println!("{}", radio_data_json);
}