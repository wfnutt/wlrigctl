use std::{thread, time::Duration, net::{UdpSocket, SocketAddr}};
use std::fmt;
use std::fmt::Display;
use serde::{Serialize, Deserialize};
use serde_json::json;
use bincode2::LengthOption::U32;
use crate::cloudlog::upload_wsjtx_qso_data;

const SZ_RXBUF: usize = 1500; // close enough for a typical Ethernet MTU
const WSJTX_MAGIC: u32 = 0xadbccbda;
const SZ_HDR: usize = 12; // bytes of initial header


#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(C)]
pub enum WSJTXMsg {
    Heartbeat {
        id: String,
        max_schema_num: u32,
        version: String,
        revision: u32,
    },
    Status {
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
    },
    Decode {
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
    },
    Clear,
    Reply,
    QSOLogged,
    Close,
    Replay,
    HaltTx,
    FreeText,
    WSPRDecode,
    Location,
    LoggedADIF {
        id: String,
        adif_text: String
    },
    HighlightCallsign,
    SwitchConfiguration,
    Configure,
}

impl Display for WSJTXMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WSJTXMsg::Heartbeat {
                id,
                max_schema_num,
                version,
                revision
            } => write!(f, "Heartbeat id:{} max_schema_num:{} version:{} revision:{}",
                id, max_schema_num, version, revision),
            WSJTXMsg::Status {
                id,
                dial_frequency_hz,
                mode,
                dx_call,
                report,
                tx_mode,
                tx_enabled,
                transmitting,
                decoding,
                pad: _,
                rx_df,
                tx_df,
            } => write!(f,
                "Status id: {} dial_frequency_hz: {} mode: {} dxcall: {} report: {} \
                tx_mode: {} tx_enabled: {} transmitting: {} decoding: {} \
                rx_df: {} tx_df: {}",
                id, dial_frequency_hz, mode, dx_call, report, tx_mode,
                tx_enabled, transmitting, decoding, rx_df, tx_df
            ),
            WSJTXMsg::Decode {
                id,
                new,
                time,
                snr,
                delta_t,
                delta_f,
                mode,
                message,
                low_confidence,
                off_air,
            } => write!(f, "Decode id: {} new: {} time: {} snr: {} delta_t: {} delta_f: {} \
                    mode: {} message: {} low_confidence: {} off_air: {}",
                    id, new, time, snr, delta_t, delta_f, mode, message, low_confidence, off_air
            ),
            WSJTXMsg::Clear               => write!(f, "Clear"),
            WSJTXMsg::Reply               => write!(f, "Reply"),
            WSJTXMsg::QSOLogged           => write!(f, "QSO Logged"),
            WSJTXMsg::Close               => write!(f, "Close"),
            WSJTXMsg::Replay              => write!(f, "Replay"),
            WSJTXMsg::HaltTx              => write!(f, "Halt Tx"),
            WSJTXMsg::FreeText            => write!(f, "Free Text"),
            WSJTXMsg::WSPRDecode          => write!(f, "WSPR Decode"),
            WSJTXMsg::Location            => write!(f, "Location"),
            WSJTXMsg::LoggedADIF {
                id,
                adif_text,
            } => write!(f, "Logged id: {} ADIF text: {}", id, adif_text),
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

pub async fn decode_hdr(buf: &[u8]) -> Result<(), WSJTXError> {
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
                //WSJTXMsg::Heartbeat { .. } => { println!("heartbeat"); Ok(())},
                //WSJTXMsg::Status { .. } => { println!("status"); Ok(())},
                //WSJTXMsg::Decode { .. } => {println!("decode"); Ok(())},
                WSJTXMsg::LoggedADIF {id: _, adif_text} => {
                    let json_data = json!({
                        "key": "wl678c05df0eb29",
                        "station_profile_id": 1,
                        "type": "adif",
                        "string": adif_text
                    });
                    match upload_wsjtx_qso_data("http://localhost/api/qso", &json_data).await {
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

async fn rxhandler(rxdata: &[u8], _src: SocketAddr) {
    match decode_hdr(rxdata).await {
        Ok(_) => (),
        Err(e) => println!("Error: {}", e)
    }
}

pub async fn wsjtx_rxloop(socket: UdpSocket) {
    loop {
        let mut buf = [0; SZ_RXBUF];

        match socket.recv_from(&mut buf) {
            Ok((amt, src)) => rxhandler(&buf[0..amt], src).await,
            Err(e) => {
                println!("Error: {}", e);
                thread::sleep(Duration::from_secs(3));
            },
        }
    }
}
