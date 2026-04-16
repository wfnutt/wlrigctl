use crate::wavelog::RadioData;
use futures_util::{SinkExt, StreamExt};
use log::{debug, info, warn};
use serde_derive::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize, Clone)]
pub struct WsSettings {
    /// Interface to bind on; defaults to 127.0.0.1 when absent.
    pub host: Option<String>,
    /// TCP port to listen on; defaults to 54322 when absent.
    pub port: Option<u16>,
}

impl WsSettings {
    pub fn bind_addr(&self) -> SocketAddr {
        let host = self.host.as_deref().unwrap_or("127.0.0.1");
        let port = self.port.unwrap_or(54322);
        format!("{host}:{port}")
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:54322".parse().unwrap())
    }
}

/// Broadcast radio state to a single connected WebSocket client.
///
/// Runs for the lifetime of one connection.  Subscribes to the shared broadcast
/// channel and forwards each update as a JSON text frame.  Torn down cleanly
/// on cancellation or client disconnect.
async fn handle_client(
    stream: TcpStream,
    peer: SocketAddr,
    mut rx: broadcast::Receiver<Arc<RadioData>>,
    token: CancellationToken,
) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!("WebSocket handshake failed for {peer}: {e}");
            return;
        }
    };
    info!("WebSocket client connected: {peer}");

    let (mut sink, mut source) = ws_stream.split();

    loop {
        tokio::select! {
            // Graceful shutdown.
            _ = token.cancelled() => {
                debug!("WebSocket handler for {peer}: shutdown");
                break;
            }
            // Discard inbound frames; close when client disconnects.
            frame = source.next() => {
                match frame {
                    None | Some(Err(_)) => {
                        debug!("WebSocket client {peer} disconnected");
                        break;
                    }
                    Some(Ok(_)) => {} // ignore ping/pong/text from client
                }
            }
            // Forward the next radio-state update.
            update = rx.recv() => {
                let data = match update {
                    Ok(d) => d,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client {peer} lagged by {n} messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let msg = json!({
                    "type":      "radio_status",
                    "frequency": data.frequency.parse::<u64>().unwrap_or(0),
                    "mode":      data.mode,
                    "power":     data.power.parse::<f32>().unwrap_or(0.0),
                    "radio":     data.radio,
                    "timestamp": std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis())
                                    .unwrap_or(0),
                });
                if sink.send(Message::Text(msg.to_string())).await.is_err() {
                    debug!("WebSocket send to {peer} failed; closing");
                    break;
                }
            }
        }
    }

    info!("WebSocket client disconnected: {peer}");
}

/// Spawn a WebSocket server that pushes live radio state to browser clients.
///
/// Each connecting client receives a JSON text frame on every rig-state change:
/// ```json
/// {"type":"radio_status","frequency":14225000,"mode":"USB","power":10.0,"radio":"IC-703","timestamp":1714000000000}
/// ```
/// This matches the format emitted by WaveLogGate so that Wavelog's own
/// WebSocket consumer works without modification.
pub fn ws_thread(
    settings: WsSettings,
    ws_tx: broadcast::Sender<Arc<RadioData>>,
    token: CancellationToken,
) {
    let addr = settings.bind_addr();

    tokio::task::spawn(async move {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => {
                info!("WebSocket server listening on ws://{addr}");
                l
            }
            Err(e) => {
                warn!("WebSocket server could not bind to {addr}: {e}");
                return;
            }
        };

        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    info!("WebSocket server shutting down");
                    return;
                }
                result = listener.accept() => {
                    match result {
                        Err(e) => { warn!("WebSocket accept error: {e}"); }
                        Ok((stream, peer)) => {
                            let rx = ws_tx.subscribe();
                            tokio::task::spawn(handle_client(
                                stream,
                                peer,
                                rx,
                                token.clone(),
                            ));
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_addr_defaults() {
        let s = WsSettings { host: None, port: None };
        let addr = s.bind_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 54322);
    }

    #[test]
    fn bind_addr_explicit() {
        let s = WsSettings {
            host: Some("0.0.0.0".to_string()),
            port: Some(9000),
        };
        let addr = s.bind_addr();
        assert_eq!(addr.ip().to_string(), "0.0.0.0");
        assert_eq!(addr.port(), 9000);
    }

    #[test]
    fn bind_addr_invalid_falls_back_to_loopback() {
        // An unparseable host string should yield the safe 127.0.0.1:54322 default.
        let s = WsSettings {
            host: Some("not-an-ip".to_string()),
            port: Some(54322),
        };
        let addr = s.bind_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 54322);
    }

    /// Verify the JSON message shape matches what Wavelog expects.
    #[test]
    fn radio_status_message_shape() {
        let data = RadioData {
            key: "k".to_string(),
            radio: "IC-703".to_string(),
            frequency: "14074000".to_string(),
            mode: "USB".to_string(),
            power: "10".to_string(),
            cat_url: None,
        };
        let msg = json!({
            "type":      "radio_status",
            "frequency": data.frequency.parse::<u64>().unwrap_or(0),
            "mode":      data.mode,
            "power":     data.power.parse::<f32>().unwrap_or(0.0),
            "radio":     data.radio,
            "timestamp": 0u64,
        });
        assert_eq!(msg["type"],      "radio_status");
        assert_eq!(msg["frequency"], 14074000u64);
        assert_eq!(msg["mode"],      "USB");
        assert_eq!(msg["power"],     10.0f64);
        assert_eq!(msg["radio"],     "IC-703");
    }

    /// Non-numeric frequency/power strings should produce 0 rather than panic.
    #[test]
    fn radio_status_bad_numeric_fields_produce_zero() {
        let data = RadioData {
            key: String::new(),
            radio: "test".to_string(),
            frequency: "not-a-number".to_string(),
            mode: "USB".to_string(),
            power: "??".to_string(),
            cat_url: None,
        };
        let freq: u64 = data.frequency.parse().unwrap_or(0);
        let power: f32 = data.power.parse().unwrap_or(0.0);
        assert_eq!(freq, 0);
        assert_eq!(power, 0.0);
    }
}
