use crate::wavelog::RadioData;
use futures_util::{SinkExt, StreamExt};
use log::{debug, info, warn};
use rcgen::generate_simple_self_signed;
use rustls::{Certificate, PrivateKey, ServerConfig};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use serde_derive::Deserialize;
use serde_json::json;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// Configuration for the WebSocket server.
///
/// The entire `[websocket]` section is optional in `config.toml`.  When absent,
/// all fields take their defaults and the server starts automatically on
/// `127.0.0.1:54323` using a persistent self-signed TLS certificate.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct WsSettings {
    /// Interface to bind on.  Defaults to `127.0.0.1`.
    pub host: Option<String>,
    /// TCP port to listen on.  Defaults to `54323` — the port Wavelog's
    /// `cat.js` connects to (`wss://127.0.0.1:54323/`).
    pub port: Option<u16>,
    /// Path to a PEM-encoded TLS certificate.  Must be set together with
    /// `tls_key`.  When both are absent a self-signed certificate is
    /// generated and saved in the wlrigctl config directory so it survives
    /// restarts.  Use `mkcert` to generate a browser-trusted cert — see
    /// `example.toml` for instructions.
    pub tls_cert: Option<String>,
    /// Path to a PEM-encoded private key (PKCS#8 or RSA).
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

// ── TLS helpers ──────────────────────────────────────────────────────────────

/// Build a [`TlsAcceptor`] from PEM files provided by the user.
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
        let pkcs8 = pkcs8_private_keys(&mut r)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid key PEM"))?;
        if let Some(k) = pkcs8.into_iter().next() {
            PrivateKey(k)
        } else {
            let f2 = File::open(key_path)?;
            let rsa = rsa_private_keys(&mut BufReader::new(f2))
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid key PEM"))?;
            PrivateKey(
                rsa.into_iter().next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "no private key found in file")
                })?,
            )
        }
    };

    let config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(io::Error::other)?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Build a [`TlsAcceptor`] directly from DER-encoded cert and key bytes.
fn tls_acceptor_from_der(cert_der: Vec<u8>, key_der: Vec<u8>) -> io::Result<TlsAcceptor> {
    let config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(vec![Certificate(cert_der)], PrivateKey(key_der))
        .map_err(io::Error::other)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Return a [`TlsAcceptor`] backed by a self-signed certificate that is
/// **saved to `config_dir`** so it persists across daemon restarts.
///
/// On first run the cert is generated and written to
/// `config_dir/ws-cert.pem` + `config_dir/ws-key.pem`.  On subsequent
/// runs the saved files are loaded.  If the saved files are corrupt they
/// are regenerated.
///
/// Because the certificate is stable across restarts, the browser only
/// needs to accept the one-time security exception once.  Users who want
/// to avoid the exception entirely should use `mkcert` (see `example.toml`).
fn persistent_self_signed_acceptor(config_dir: &Path) -> io::Result<TlsAcceptor> {
    let cert_path = config_dir.join("ws-cert.pem");
    let key_path  = config_dir.join("ws-key.pem");

    if cert_path.exists() && key_path.exists() {
        let cert_str = cert_path.to_str().unwrap_or("");
        let key_str  = key_path.to_str().unwrap_or("");
        match load_tls_acceptor(cert_str, key_str) {
            Ok(a) => {
                info!("WebSocket TLS: loaded saved certificate from {}", cert_path.display());
                return Ok(a);
            }
            Err(e) => warn!("WebSocket TLS: saved cert/key invalid ({e}), regenerating"),
        }
    }

    let sans = vec!["127.0.0.1".to_string(), "localhost".to_string()];
    let cert = generate_simple_self_signed(sans)
        .map_err(io::Error::other)?;

    let cert_der = cert.serialize_der()
        .map_err(io::Error::other)?;
    let key_der  = cert.serialize_private_key_der();
    let cert_pem = cert.serialize_pem()
        .map_err(io::Error::other)?;
    let key_pem  = cert.serialize_private_key_pem();

    fs::create_dir_all(config_dir)?;
    fs::write(&cert_path, cert_pem)?;
    fs::write(&key_path,  key_pem)?;
    info!(
        "WebSocket TLS: generated self-signed certificate, saved to {}",
        cert_path.display()
    );
    warn!(
        "WebSocket TLS: browser will show a security warning. \
         Visit https://{} in your browser and accept the certificate \
         exception once. It will not be asked again unless you delete {}.",
        "127.0.0.1:54323",
        cert_path.display()
    );

    tls_acceptor_from_der(cert_der, key_der)
}

// ── Per-client handler ────────────────────────────────────────────────────────

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
                    Some(Ok(_)) => {}
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

// ── Public entry point ────────────────────────────────────────────────────────

/// Spawn the WSS server.
///
/// `config_dir` is the application's XDG config directory
/// (`~/.config/wlrigctl`).  The auto-generated self-signed certificate is
/// stored there so it persists across restarts.
pub fn ws_thread(
    settings: WsSettings,
    config_dir: PathBuf,
    ws_tx: broadcast::Sender<Arc<RadioData>>,
    token: CancellationToken,
) {
    let addr = settings.bind_addr();

    let acceptor = match (settings.tls_cert.as_deref(), settings.tls_key.as_deref()) {
        (Some(cert), Some(key)) => {
            match load_tls_acceptor(cert, key) {
                Ok(a) => { info!("WebSocket TLS: loaded cert from {cert}"); a }
                Err(e) => {
                    warn!("WebSocket TLS: failed to load cert/key: {e}; WebSocket server disabled");
                    return;
                }
            }
        }
        (None, None) => {
            match persistent_self_signed_acceptor(&config_dir) {
                Ok(a) => a,
                Err(e) => {
                    warn!("WebSocket TLS: cert setup failed: {e}; WebSocket server disabled");
                    return;
                }
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_give_expected_addr() {
        let s = WsSettings::default();
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
    fn bind_addr_invalid_host_falls_back_to_loopback() {
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
    fn cert_is_saved_and_reloaded_from_disk() {
        let dir = std::env::temp_dir().join("wlrigctl-ws-test");
        // Clean up any previous run so we exercise the generate path.
        let _ = std::fs::remove_dir_all(&dir);

        // First call: should generate and save.
        let a1 = persistent_self_signed_acceptor(&dir);
        assert!(a1.is_ok(), "cert generation failed: {:?}", a1.err());
        assert!(dir.join("ws-cert.pem").exists());
        assert!(dir.join("ws-key.pem").exists());

        // Second call: should load from disk (same cert, no regeneration).
        let a2 = persistent_self_signed_acceptor(&dir);
        assert!(a2.is_ok(), "cert reload failed: {:?}", a2.err());
    }

    #[test]
    fn corrupt_saved_cert_triggers_regeneration() {
        let dir = std::env::temp_dir().join("wlrigctl-ws-test-corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Write garbage files.
        std::fs::write(dir.join("ws-cert.pem"), b"not a cert").unwrap();
        std::fs::write(dir.join("ws-key.pem"),  b"not a key").unwrap();

        // Should regenerate rather than fail.
        let result = persistent_self_signed_acceptor(&dir);
        assert!(result.is_ok(), "regeneration after corrupt files failed: {:?}", result.err());
    }

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

    #[test]
    fn radio_status_bad_numeric_fields_produce_zero() {
        let freq:  u64 = "not-a-number".parse().unwrap_or(0);
        let power: f32 = "??".parse().unwrap_or(0.0);
        assert_eq!(freq,  0);
        assert_eq!(power, 0.0);
    }
}
