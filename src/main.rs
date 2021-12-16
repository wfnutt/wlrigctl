use std::io::prelude::*;
use std::net::TcpStream;
use std::str;

pub struct RigCtl
{
    pub stream: TcpStream,
}

impl RigCtl {
    pub fn new() -> Self {
        let stream = TcpStream::connect("127.0.0.1:4532").unwrap();
        RigCtl {
            stream: stream,
        }
    }

    pub fn get_frequency(&mut self) -> String {
        self.stream.write(b"f").unwrap();
        let mut buffer = [0; 64];
        self.stream.read(&mut buffer[..]).unwrap();
        let freq = str::from_utf8(&buffer).unwrap().lines().next().unwrap().to_string();
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

fn main() {
    let mut rigctl = RigCtl::new();
    let freq = rigctl.get_frequency();
    println!("Frequenz: {} Hz", freq);

    let (mode, passband) = rigctl.get_mode();
    println!("Mode: {} – Passband: {}", mode, passband);
}
