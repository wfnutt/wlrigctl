use crate::cloudlog::RadioData;

use log::debug;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use reqwest::{self, header::CONTENT_TYPE, Error};
use std::result::Result;

pub fn get_radio_data(fl_host: &str, fl_port: &str) -> Result<RadioData, Error> {
    let cmd_freq = "rig.get_vfo";
    let cmd_mode = "rig.get_mode";
    let cmd_power = "rig.get_power";

    let client = reqwest::Client::new();

    let freq = get_value_from_flrig(fl_host, fl_port, &client, &cmd_freq)?;
    let mode = get_value_from_flrig(fl_host, fl_port, &client, &cmd_mode)?;
    let power = get_value_from_flrig(fl_host, fl_port, &client, &cmd_power)?;

    let freq = parse_xml(&freq);
    let mode = parse_xml(&mode);
    let power = parse_xml(&power);

    debug!("freq: {freq} --- mode: {mode} --- power: {power}");

    let radio_data = RadioData {
        key: String::from(""),
        radio: String::from(""),
        frequency: freq,
        mode,
        power,
    };

    Ok(radio_data)
}

#[tokio::main]
async fn get_value_from_flrig(
    fl_host: &str,
    fl_port: &str,
    client: &reqwest::Client,
    cmd: &str,
) -> Result<String, Error> {
    let xml_cmd = create_xml_cmd(&cmd);

    let res = client
        .post(fl_host.to_owned() + ":" + fl_port)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(xml_cmd)
        .send()
        .await?;

    let body = res.text().await?;

    Ok(body)
}

fn create_xml_cmd(cmd: &str) -> String {
    format!("<?xml version=\"1.0\"?><methodCall><methodName>{cmd}</methodName><params></params></methodCall>")
}

fn parse_xml(xml: &str) -> String {
    let mut reader = Reader::from_str(&xml);
    reader.trim_text(true);

    let mut value = String::new();

    loop {
        let mut _found = false;

        match reader.read_event() {
            Err(e) => panic!("Error at position {}: {:?}", reader.buffer_position(), e),

            Ok(Event::Eof) => break,

            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"value" => (), //found = true,
                _ => (),
            },
            Ok(Event::Text(e)) => value = e.unescape().unwrap().to_owned().to_string(),

            _ => (),
        }
    }

    value
}
