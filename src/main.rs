mod cat;
mod flrig;
mod settings;
mod wavelog;
mod wsjtx;

use std::process;
use std::sync::Arc;

use log::info;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::Response;
pub type HttpResponse = Response<Full<Bytes>>;

use crate::cat::CAT_thread;
use crate::settings::Settings;
use crate::wavelog::wavelog_thread;
use crate::wsjtx::wsjtx_thread;

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

    // polling of FLRig frequency. Issue http requests to wavelog to update live frequency
    wavelog_thread(settings.wavelog.clone(), rig.clone());

    // Separate thread for someone logging from WSJTX via UDP on port 2237
    wsjtx_thread(settings.WSJTX, settings.wavelog);

    // Keep the current thread for CAT control requests from Wavelog
    // We gateway these requests back to FLRig after a little bit of massaging
    CAT_thread(settings.CAT, &rig).await
}
