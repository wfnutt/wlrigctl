use std::io::prelude::*;
use std::net::TcpStream;
use std::str;
use chrono::{Local};
use serde::{Deserialize, Serialize};

pub struct RigCtl {
    pub stream: TcpStream,
}

impl RigCtl {
    pub fn new() -> Result<Self, std::io::Error> {
        let stream = TcpStream::connect("127.0.0.1:4532")?;
        Ok(RigCtl { stream })
    }

    pub fn get_frequency(&mut self) -> String {
        self.stream.write(b"f").unwrap();
        let mut buffer = [0; 64];
        self.stream.read(&mut buffer[..]).unwrap();
        let freq = str::from_utf8(&buffer)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        freq
    }

    pub fn get_mode(&mut self) -> (String, String) {
        self.stream.write(b"m").unwrap();
        let mut buffer = [0; 64];
        self.stream.read(&mut buffer[..]).unwrap();
        let mut retvals = str::from_utf8(&buffer).unwrap().lines();
        let mode = retvals.next().unwrap().to_string();
        let passband = retvals.next().unwrap().to_string();
        (mode, passband)
    }
}

#[derive(Serialize, Deserialize)]
struct RadioData {
    key: String,
    radio: String,
    frequency: String,
    mode: String,
    timestamp: String,
}

fn main() {
    match RigCtl::new() {
        Ok(mut rigctl) => {
            let key = "".to_string();

            let radio = "clrigctl".to_string();

            let frequency = rigctl.get_frequency();
            //println!("Frequenz: {} Hz", frequency);

            let (mode, _) = rigctl.get_mode();
            //println!("Mode: {}", mode);

            let timestamp = Local::now().format("%Y/%m/%d %H:%M").to_string();
            //println!("Timestamp: {}", timestamp);

            let radiodata = RadioData {key, radio, frequency, mode, timestamp};
            let radiodata_json = serde_json::to_string(&radiodata).unwrap();

            let client = reqwest::blocking::Client::new();
            let resp = client.post("https://cloudlog.rustysoft.de/index.php/api/radio").body(radiodata_json).send();

            println!("{:?}", resp);
        }
        Err(e) => {
            println!("{}", e);
        }
    }
}


