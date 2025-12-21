use std::{thread, time::Duration, net::{UdpSocket, SocketAddr}};
use std::fmt;
use std::fmt::Display;
use serde::{Serialize, Deserialize};
use bincode2::LengthOption::U32;
use log::info;
use crate::wavelog::{WavelogSettings, upload_wsjtx_qso_data};

// Settings from config file
#[derive(Debug, Deserialize)]
pub struct WsjtxSettings {
    pub host: String,
    pub port: u16,
    pub err_timeout: u64,
}

const SZ_RXBUF: usize = 1500; // close enough for a typical Ethernet MTU
const WSJTX_MAGIC: u32 = 0xadbccbda;
const SZ_HDR: usize = 12; // bytes of initial header

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub struct WSJTX_Heartbeat {
    id: String,
    max_schema_num: u32,
    version: String,
    revision: u32,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub struct WSJTX_Status {
    id: String,
    dial_frequency_hz: u64,
    mode: String,
    dx_call: String,
    report: String,
    tx_mode: String,
    tx_enabled: u8,
    transmitting: u8,
    decoding: u8,
    pad: u8,
    rx_df: u32,
    tx_df: u32,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub struct WSJTX_Decode {
    id: String,
    new: u8,
    time: u32,
    snr: i32,
    delta_t: f64,
    delta_f: u32,
    mode: String,
    message: String,
    low_confidence: u8,
    off_air: u8,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub struct WSJTX_LoggedADIF {
    id: String,
    adif_text: String
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub enum WSJTXMsg {
    Heartbeat(WSJTX_Heartbeat),
    Status(WSJTX_Status),
    Decode(WSJTX_Decode),
    Clear,
    Reply,
    QSOLogged,
    Close,
    Replay,
    HaltTx,
    FreeText,
    WSPRDecode,
    Location,
    LoggedADIF(WSJTX_LoggedADIF),
    HighlightCallsign,
    SwitchConfiguration,
    Configure,
}

impl Display for WSJTX_Heartbeat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Heartbeat id:{} max_schema_num:{} version:{} revision:{}",
               self.id, self.max_schema_num, self.version, self.revision)
    }
}

impl Display for WSJTX_Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Status id: {} dial_frequency_hz: {} mode: {} dxcall: {} report: {} \
                   tx_mode: {} tx_enabled: {} transmitting: {} decoding: {} \
                   rx_df: {} tx_df: {}",
               self.id, self.dial_frequency_hz, self.mode, self.dx_call, self.report, self.tx_mode,
               self.tx_enabled, self.transmitting, self.decoding, self.rx_df, self.tx_df)
    }
}

impl Display for WSJTX_Decode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decode: id: {} new: {} time: {} snr: {} delta_t: {} delta_f: {} \
            mode: {} message: {} low_confidence: {} off_air: {}",
            self.id, self.new, self.time, self.snr, self.delta_t, self.delta_f, self.mode,
            self.message, self.low_confidence, self.off_air)
    }
}

impl Display for WSJTX_LoggedADIF {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LoggedADIF: Logged id: {} ADIF text: {}", self.id, self.adif_text)
    }
}

impl Display for WSJTXMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WSJTXMsg::Heartbeat(msg)      => write!(f, "{}", msg),
            WSJTXMsg::Status(msg)         => write!(f, "{}", msg),
            WSJTXMsg::Decode(msg)         => write!(f, "{}", msg),
            WSJTXMsg::Clear               => write!(f, "Clear"),
            WSJTXMsg::Reply               => write!(f, "Reply"),
            WSJTXMsg::QSOLogged           => write!(f, "QSO Logged"),
            WSJTXMsg::Close               => write!(f, "Close"),
            WSJTXMsg::Replay              => write!(f, "Replay"),
            WSJTXMsg::HaltTx              => write!(f, "Halt Tx"),
            WSJTXMsg::FreeText            => write!(f, "Free Text"),
            WSJTXMsg::WSPRDecode          => write!(f, "WSPR Decode"),
            WSJTXMsg::Location            => write!(f, "Location"),
            WSJTXMsg::LoggedADIF(msg)     => write!(f, "{}", msg),
            WSJTXMsg::HighlightCallsign   => write!(f, "Highlight Callsign"),
            WSJTXMsg::SwitchConfiguration => write!(f, "Switch Configuration"),
            WSJTXMsg::Configure           => write!(f, "Configure"),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub struct WSJTXData {
    magic: u32,
    schema: u32,
    msg: WSJTXMsg,
}

#[derive(Debug)]
pub enum WSJTXError {
    DatagramTooShort(String),
    DeserializationFailure(String),
    BadMajick(String),
    UnsupportedSchema(String),
    QSOUploadFailed(String),
}

impl Display for WSJTXError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WSJTXError::DatagramTooShort(msg)       => write!(f, "DatagramTooShort: {}", msg),
            WSJTXError::DeserializationFailure(msg) => write!(f, "DeserializationFailure: {}", msg),
            WSJTXError::BadMajick(msg)              => write!(f, "BadMajick: {}", msg),
            WSJTXError::UnsupportedSchema(msg)      => write!(f, "UnsupportedSchema: {}", msg),
            WSJTXError::QSOUploadFailed(msg)        => write!(f, "QSOUploadFailed: {}", msg),
        }
    }
}

impl std::error::Error for WSJTXError {}

pub async fn decode_hdr(wavelog_settings: WavelogSettings,
                        buf: &[u8])
-> Result<(), WSJTXError> {
    if buf.len() < SZ_HDR {
        let errmsg = "Datagram too short for WSJTX header".to_string();
        return Err(WSJTXError::DatagramTooShort(errmsg))
    }

    match bincode2::config()
        .big_endian()
        .string_length(U32)
        .array_length(U32)
        .deserialize::<WSJTXData>(buf) {
        Ok(wsjtx) => {
            if wsjtx.magic != WSJTX_MAGIC {
                let errmsg = format!("Bad majick: {}", wsjtx.magic);
                return Err(WSJTXError::BadMajick(errmsg));
            }
            if wsjtx.schema != 2 {
                let errmsg = format!("Schema: {}; only schema 2 so far", wsjtx.schema);
                return Err(WSJTXError::UnsupportedSchema(errmsg));
            }
            match wsjtx.msg {
                //WSJTXMsg::Heartbeat(msg) => { println!("heartbeat"); Ok(())},
                //WSJTXMsg::Status(msg)    => { println!("status"); Ok(())},
                //WSJTXMsg::Decode(msg)    => { println!("decode"); Ok(())},
                WSJTXMsg::LoggedADIF(msg)  => {
                    match upload_wsjtx_qso_data(wavelog_settings, msg.adif_text).await {
                        Ok(_) => Ok(()),
                        Err(_) => Err(WSJTXError::QSOUploadFailed("upload failure".to_string())),

                    }
                },
                msg => {println!("msg: {}", msg); Ok(())},
            }
        },
        Err(_) => {
            let errmsg = "Couldn't deserialize datagram into WSJTX header".to_string();
            Err(WSJTXError::DeserializationFailure(errmsg))
        }
    }
}

async fn rxhandler(wavelog_settings: WavelogSettings,
                   rxdata: &[u8], _src: SocketAddr) {
    match decode_hdr(wavelog_settings, rxdata).await {
        Ok(_) => (),
        Err(e) => println!("Error: {}", e)
    }
}

async fn wsjtx_rxloop(wavelog_settings: WavelogSettings,
                      socket: UdpSocket, err_timeout: u64) {
    loop {
        let mut buf = [0; SZ_RXBUF];

        match socket.recv_from(&mut buf) {
            Ok((amt, src)) => rxhandler(wavelog_settings.clone(), &buf[0..amt], src).await,
            Err(e) => {
                println!("Error: {}", e);
                thread::sleep(Duration::from_secs(err_timeout));
            },
        }
    }
}

pub fn wsjtx_thread(wsjtx_settings: WsjtxSettings, wavelog_settings: WavelogSettings) {
    let url = format!("{0}:{1}", wsjtx_settings.host, wsjtx_settings.port);
    info!("Listening for WSJTX QSO logs on: {url}");
    tokio::task::spawn(async move {
        let socket = UdpSocket::bind(url);
        match socket {
            Err(e) => println!("couldn't create socket for WSJTX QSO logging: {e}"),
            Ok(socket) => wsjtx_rxloop(wavelog_settings, socket, wsjtx_settings.err_timeout).await,
        }
    });
}
