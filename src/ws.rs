use crate::wavelog::RadioData;
use futures_util::{SinkExt, StreamExt};
use log::{debug, info, warn};
use rcgen::generate_simple_self_signed;
use rustls::{Certificate, PrivateKey, ServerConfig};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use serde_derive::Deserialize;
use serde_json::json;
use std::fs::File;
use std::io::{self, BufReader};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize, Clone)]
pub struct WsSettings {
    /// Interface to bind on.  Defaults to `127.0.0.1`.
    pub host: Option<String>,
    /// TCP port to listen on.  Defaults to `54323` — the same port used by
    /// WaveLogGate, which is what Wavelog's built-in WebSocket consumer expects.
    pub port: Option<u16>,
    /// Path to a PEM-encoded TLS certificate file.  Required together with
    /// `tls_key`.  When absent a self-signed certificate is generated at
    /// startup; the browser will show a security warning until you either
    /// visit `https://127.0.0.1:<port>/` and accept the exception, or supply
    /// a trusted certificate via `mkcert` (recommended).
    pub tls_cert: Option<String>,
    /// Path to a PEM-encoded private key file (PKCS#8 or RSA).
    pub tls_key: Option<String>,
}

impl WsSettings {
    pub fn bind_addr(&self) -> SocketAddr {
        let host = self.host.as_deref().unwrap_or("127.0.0.1");
        let port = self.port.unwrap_or(54323);
        format!("{host}:{port}")
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:54323".parse().unwrap())
    }
}

/// Build a [`TlsAcceptor`] from user-supplied PEM files.
fn load_tls_acceptor(cert_path: &str, key_path: &str) -> io::Result<TlsAcceptor> {
    let certs = {
        let f = File::open(cert_path)?;
        certs(&mut BufReader::new(f))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid cert PEM"))?
            .into_iter()
            .map(Certificate)
            .collect::<Vec<_>>()
    };

    let key = {
        let f = File::open(key_path)?;
        let mut r = BufReader::new(f);
        // Try PKCS#8 first (produced by mkcert and openssl genrsa -pkcs8)
        let pkcs8 = pkcs8_private_keys(&mut r)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid key PEM"))?;
        if let Some(k) = pkcs8.into_iter().next() {
            PrivateKey(k)
        } else {
            // Re-open and try RSA (PKCS#1 "BEGIN RSA PRIVATE KEY")
            let f2 = File::open(key_path)?;
            let rsa = rsa_private_keys(&mut BufReader::new(f2))
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid key PEM"))?;
            PrivateKey(
                rsa.into_iter()
                    .next()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key found in file"))?,
            )
        }
    };

    let config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Generate an ephemeral self-signed certificate valid for `127.0.0.1` and
/// `localhost`.  The cert is regenerated on every startup; the browser will
/// show a one-time security warning that the user must accept.
///
/// To avoid the warning, generate a locally-trusted certificate with mkcert:
///   `mkcert -install && mkcert 127.0.0.1 localhost`
/// then set `tls_cert` and `tls_key` in `[websocket]` to the produced files.
fn self_signed_tls_acceptor() -> io::Result<TlsAcceptor> {
    let sans = vec!["127.0.0.1".to_string(), "localhost".to_string()];
    let cert = generate_simple_self_signed(sans)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let cert_der = cert
        .serialize_der()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let key_der = cert.serialize_private_key_der();

    let config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(vec![Certificate(cert_der)], PrivateKey(key_der))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
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
    acceptor: TlsAcceptor,
) {
    let tls_stream = match acceptor.accept(stream).await {
        Ok(s) => s,
        Err(e) => {
            // A TLS handshake failure often means the browser visited the URL in
            // plain HTTP first; once the user accepts the cert exception it works.
            warn!("TLS handshake failed for {peer}: {e}");
            return;
        }
    };

    let ws_stream = match tokio_tungstenite::accept_async(tls_stream).await {
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
            _ = token.cancelled() => {
                debug!("WebSocket handler for {peer}: shutdown");
                break;
            }
            frame = source.next() => {
                match frame {
                    None | Some(Err(_)) => {
                        debug!("WebSocket client {peer} disconnected");
                        break;
                    }
                    Some(Ok(_)) => {} // ignore ping/pong/text from client
                }
            }
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

/// Spawn a WSS (TLS WebSocket) server that pushes live radio state to browser clients.
///
/// Each connecting client receives a JSON text frame on every rig-state change:
/// ```json
/// {"type":"radio_status","frequency":14074000,"mode":"USB","power":10.0,"radio":"IC-703","timestamp":1714000000000}
/// ```
/// The default port is 54323, matching WaveLogGate's WSS port so that Wavelog's
/// built-in WebSocket consumer works without modification.
///
/// TLS is mandatory because browsers refuse `ws://` connections from pages
/// served over HTTPS (mixed-content policy) and Wavelog's cat.js hardcodes
/// `wss://127.0.0.1:54323/`.  If no cert/key are configured, a self-signed
/// certificate is generated at startup.
pub fn ws_thread(
    settings: WsSettings,
    ws_tx: broadcast::Sender<Arc<RadioData>>,
    token: CancellationToken,
) {
    let addr = settings.bind_addr();

    let acceptor = match (settings.tls_cert.as_deref(), settings.tls_key.as_deref()) {
        (Some(cert), Some(key)) => {
            match load_tls_acceptor(cert, key) {
                Ok(a) => { info!("WebSocket TLS: loaded cert from {cert}"); a }
                Err(e) => { warn!("WebSocket TLS: failed to load cert/key: {e}; WebSocket server disabled"); return; }
            }
        }
        (None, None) => {
            match self_signed_tls_acceptor() {
                Ok(a) => {
                    warn!(
                        "WebSocket TLS: using a self-signed certificate. \
                         The browser will show a security warning. \
                         Visit https://{addr}/ in the browser and accept the exception, \
                         or use mkcert to generate a trusted cert (see example.toml)."
                    );
                    a
                }
                Err(e) => { warn!("WebSocket TLS: failed to generate self-signed cert: {e}; WebSocket server disabled"); return; }
            }
        }
        _ => {
            warn!("WebSocket config: tls_cert and tls_key must both be set or both absent; WebSocket server disabled");
            return;
        }
    };

    tokio::task::spawn(async move {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => {
                info!("WebSocket server listening on wss://{addr}");
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
                                acceptor.clone(),
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
        let s = WsSettings { host: None, port: None, tls_cert: None, tls_key: None };
        let addr = s.bind_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 54323);
    }

    #[test]
    fn bind_addr_explicit() {
        let s = WsSettings {
            host: Some("0.0.0.0".to_string()),
            port: Some(9000),
            tls_cert: None,
            tls_key: None,
        };
        let addr = s.bind_addr();
        assert_eq!(addr.ip().to_string(), "0.0.0.0");
        assert_eq!(addr.port(), 9000);
    }

    #[test]
    fn bind_addr_invalid_falls_back_to_loopback() {
        let s = WsSettings {
            host: Some("not-an-ip".to_string()),
            port: Some(54323),
            tls_cert: None,
            tls_key: None,
        };
        let addr = s.bind_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 54323);
    }

    #[test]
    fn self_signed_acceptor_builds_without_error() {
        // Exercises the rcgen + rustls path; if it compiles and produces an acceptor
        // the cert-generation chain is working.
        assert!(self_signed_tls_acceptor().is_ok());
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

    /// Non-numeric frequency/power strings must yield 0 rather than panic.
    #[test]
    fn radio_status_bad_numeric_fields_produce_zero() {
        let freq: u64 = "not-a-number".parse().unwrap_or(0);
        let power: f32 = "??".parse().unwrap_or(0.0);
        assert_eq!(freq, 0);
        assert_eq!(power, 0.0);
    }
}
