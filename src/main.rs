mod cat;
mod flrig;
mod settings;
mod wavelog;
mod ws;
mod wsjtx;

use std::process;
use std::sync::Arc;

use log::info;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::cat::CAT_thread;
use crate::settings::Settings;
use crate::wavelog::wavelog_thread;
use crate::ws::ws_thread;
use crate::wsjtx::wsjtx_thread;

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init();

    let appname = env!("CARGO_PKG_NAME");
    let appver = env!("CARGO_PKG_VERSION");

    info!("{appname} v{appver} started.");

    let settings = Settings::new().unwrap_or_else(|err| {
        eprintln!("Could not read settings: {err}");
        process::exit(1)
    });

    let radio_id: String = settings.wavelog.identifier.clone();
    let rig = Arc::new(flrig::FLRig::new(settings.flrig, radio_id));

    let token = CancellationToken::new();

    // Channel for streaming live radio state to WebSocket clients.
    // Capacity 4: clients that fall behind are dropped without blocking the poll loop.
    let (ws_tx, _) = broadcast::channel::<Arc<wavelog::RadioData>>(4);

    // polling of FLRig frequency. Issue http requests to wavelog to update live frequency
    wavelog_thread(settings.wavelog.clone(), rig.clone(), token.clone(), ws_tx.clone());

    // Separate thread for someone logging from WSJTX via UDP on port 2237
    wsjtx_thread(settings.wsjtx, settings.wavelog, token.clone());

    // WebSocket server: push live rig state to browser clients.
    // Always started; [websocket] section in config.toml is optional.
    let config_dir = Settings::config_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    ws_thread(settings.websocket, config_dir, ws_tx.clone(), token.clone());

    // Keep the current thread for CAT control requests from Wavelog
    // We gateway these requests back to FLRig after a little bit of massaging
    tokio::select! {
        result = CAT_thread(settings.cat, &rig, token.clone()) => result,
        _ = shutdown_signal() => {
            info!("Shutdown signal received, stopping tasks");
            token.cancel();
            Ok(())
        }
    }
}
