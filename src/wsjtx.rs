use crate::wavelog::{upload_wsjtx_qso_data, WavelogSettings};
use bincode2::LengthOption::U32;
use log::{debug, error, info};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::Display;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

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
pub struct WsjtxHeartbeat {
    id: String,
    max_schema_num: u32,
    version: String,
    revision: u32,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct WsjtxStatus {
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
pub struct WsjtxDecode {
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
pub struct WsjtxLoggedAdif {
    id: String,
    adif_text: String,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum WsjtxMsg {
    Heartbeat(WsjtxHeartbeat),
    Status(WsjtxStatus),
    Decode(WsjtxDecode),
    Clear,
    Reply,
    QSOLogged,
    Close,
    Replay,
    HaltTx,
    FreeText,
    WSPRDecode,
    Location,
    LoggedADIF(WsjtxLoggedAdif),
    HighlightCallsign,
    SwitchConfiguration,
    Configure,
}

impl Display for WsjtxHeartbeat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Heartbeat id:{} max_schema_num:{} version:{} revision:{}",
            self.id, self.max_schema_num, self.version, self.revision
        )
    }
}

impl Display for WsjtxStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Status id: {} dial_frequency_hz: {} mode: {} dxcall: {} report: {} \
                   tx_mode: {} tx_enabled: {} transmitting: {} decoding: {} \
                   rx_df: {} tx_df: {}",
            self.id,
            self.dial_frequency_hz,
            self.mode,
            self.dx_call,
            self.report,
            self.tx_mode,
            self.tx_enabled,
            self.transmitting,
            self.decoding,
            self.rx_df,
            self.tx_df
        )
    }
}

impl Display for WsjtxDecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Decode: id: {} new: {} time: {} snr: {} delta_t: {} delta_f: {} \
            mode: {} message: {} low_confidence: {} off_air: {}",
            self.id,
            self.new,
            self.time,
            self.snr,
            self.delta_t,
            self.delta_f,
            self.mode,
            self.message,
            self.low_confidence,
            self.off_air
        )
    }
}

impl Display for WsjtxLoggedAdif {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LoggedADIF: Logged id: {} ADIF text: {}",
            self.id, self.adif_text
        )
    }
}

impl Display for WsjtxMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsjtxMsg::Heartbeat(msg) => write!(f, "{}", msg),
            WsjtxMsg::Status(msg) => write!(f, "{}", msg),
            WsjtxMsg::Decode(msg) => write!(f, "{}", msg),
            WsjtxMsg::Clear => write!(f, "Clear"),
            WsjtxMsg::Reply => write!(f, "Reply"),
            WsjtxMsg::QSOLogged => write!(f, "QSO Logged"),
            WsjtxMsg::Close => write!(f, "Close"),
            WsjtxMsg::Replay => write!(f, "Replay"),
            WsjtxMsg::HaltTx => write!(f, "Halt Tx"),
            WsjtxMsg::FreeText => write!(f, "Free Text"),
            WsjtxMsg::WSPRDecode => write!(f, "WSPR Decode"),
            WsjtxMsg::Location => write!(f, "Location"),
            WsjtxMsg::LoggedADIF(msg) => write!(f, "{}", msg),
            WsjtxMsg::HighlightCallsign => write!(f, "Highlight Callsign"),
            WsjtxMsg::SwitchConfiguration => write!(f, "Switch Configuration"),
            WsjtxMsg::Configure => write!(f, "Configure"),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct WsjtxData {
    magic: u32,
    schema: u32,
    msg: WsjtxMsg,
}

#[derive(Debug)]
pub enum WsjtxError {
    DatagramTooShort(String),
    DeserializationFailure(String),
    BadMajick(String),
    UnsupportedSchema(String),
    QSOUploadFailed(String),
}

impl Display for WsjtxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsjtxError::DatagramTooShort(msg) => write!(f, "DatagramTooShort: {}", msg),
            WsjtxError::DeserializationFailure(msg) => write!(f, "DeserializationFailure: {}", msg),
            WsjtxError::BadMajick(msg) => write!(f, "BadMajick: {}", msg),
            WsjtxError::UnsupportedSchema(msg) => write!(f, "UnsupportedSchema: {}", msg),
            WsjtxError::QSOUploadFailed(msg) => write!(f, "QSOUploadFailed: {}", msg),
        }
    }
}

impl std::error::Error for WsjtxError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wavelog::WavelogSettings;
    use reqwest::Client;

    fn dummy_settings() -> WavelogSettings {
        WavelogSettings {
            url: "http://localhost/api/radio".to_string(),
            qso_url: "http://localhost/api/qso".to_string(),
            key: "test".to_string(),
            identifier: "test-rig".to_string(),
            station_profile_id: 1,
            interval: 1000,
            cat_url: None,
        }
    }

    // Serialise a WsjtxData packet using the same bincode2 config as the live code.
    fn make_packet(magic: u32, schema: u32, msg: WsjtxMsg) -> Vec<u8> {
        bincode2::config()
            .big_endian()
            .string_length(U32)
            .array_length(U32)
            .serialize(&WsjtxData { magic, schema, msg })
            .unwrap()
    }

    #[tokio::test]
    async fn decode_hdr_too_short() {
        let result = decode_hdr(&Client::new(), dummy_settings(), &[0u8; 4]).await;
        assert!(matches!(result, Err(WsjtxError::DatagramTooShort(_))));
    }

    #[tokio::test]
    async fn decode_hdr_bad_magic() {
        let buf = make_packet(0xDEAD_BEEF, 2, WsjtxMsg::Clear);
        let result = decode_hdr(&Client::new(), dummy_settings(), &buf).await;
        assert!(matches!(result, Err(WsjtxError::BadMajick(_))));
    }

    #[tokio::test]
    async fn decode_hdr_unsupported_schema() {
        let buf = make_packet(WSJTX_MAGIC, 3, WsjtxMsg::Clear);
        let result = decode_hdr(&Client::new(), dummy_settings(), &buf).await;
        assert!(matches!(result, Err(WsjtxError::UnsupportedSchema(_))));
    }
}

pub async fn decode_hdr(
    client: &Client,
    wavelog_settings: WavelogSettings,
    buf: &[u8],
) -> Result<(), WsjtxError> {
    if buf.len() < SZ_HDR {
        let errmsg = "Datagram too short for WSJTX header".to_string();
        return Err(WsjtxError::DatagramTooShort(errmsg));
    }

    match bincode2::config()
        .big_endian()
        .string_length(U32)
        .array_length(U32)
        .deserialize::<WsjtxData>(buf)
    {
        Ok(wsjtx) => {
            if wsjtx.magic != WSJTX_MAGIC {
                let errmsg = format!("Bad majick: {}", wsjtx.magic);
                return Err(WsjtxError::BadMajick(errmsg));
            }
            if wsjtx.schema != 2 {
                let errmsg = format!("Schema: {}; only schema 2 so far", wsjtx.schema);
                return Err(WsjtxError::UnsupportedSchema(errmsg));
            }
            match wsjtx.msg {
                WsjtxMsg::LoggedADIF(msg) => {
                    match upload_wsjtx_qso_data(client, &wavelog_settings, msg.adif_text).await {
                        Ok(_) => Ok(()),
                        Err(_) => Err(WsjtxError::QSOUploadFailed("upload failure".to_string())),
                    }
                }
                msg => {
                    debug!("{}", msg);
                    Ok(())
                }
            }
        }
        Err(_) => {
            let errmsg = "Couldn't deserialize datagram into WSJTX header".to_string();
            Err(WsjtxError::DeserializationFailure(errmsg))
        }
    }
}

async fn rxhandler(
    client: &Client,
    wavelog_settings: WavelogSettings,
    rxdata: &[u8],
    _src: SocketAddr,
) {
    match decode_hdr(client, wavelog_settings, rxdata).await {
        Ok(_) => (),
        Err(e) => error!("{}", e),
    }
}

async fn wsjtx_rxloop(
    wavelog_settings: WavelogSettings,
    socket: UdpSocket,
    err_timeout: u64,
    token: CancellationToken,
) {
    let client = Client::new();
    loop {
        let mut buf = [0; SZ_RXBUF];

        tokio::select! {
            _ = token.cancelled() => {
                info!("wsjtx thread shutting down");
                return;
            }
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((amt, src)) => rxhandler(&client, wavelog_settings.clone(), &buf[0..amt], src).await,
                    Err(e) => {
                        error!("UDP receive error: {}", e);
                        tokio::select! {
                            _ = token.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_secs(err_timeout)) => {}
                        }
                    }
                }
            }
        }
    }
}

pub fn wsjtx_thread(
    wsjtx_settings: WsjtxSettings,
    wavelog_settings: WavelogSettings,
    token: CancellationToken,
) {
    let url = format!("{0}:{1}", wsjtx_settings.host, wsjtx_settings.port);
    info!("Listening for WSJT-X QSO logs on: {url}");
    tokio::task::spawn(async move {
        match UdpSocket::bind(&url).await {
            Err(e) => error!("couldn't create socket for WSJTX QSO logging: {e}"),
            Ok(socket) => {
                wsjtx_rxloop(wavelog_settings, socket, wsjtx_settings.err_timeout, token).await
            }
        }
    });
}
