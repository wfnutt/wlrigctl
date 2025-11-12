mod cloudlog;
mod flrig;
mod settings;
mod wsjtx;

use std::sync::Arc;

use log::{debug, info};
use std::process;
use std::str::FromStr;

use crate::cloudlog::RadioData;
use crate::flrig::Mode;
use crate::wsjtx::wsjtx_rxloop;
use settings::Settings;
use url::Url;

use std::net::{IpAddr, SocketAddr};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};

pub type HttpResponse = Response<Full<Bytes>>;

use std::convert::Infallible;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use std::{net::{UdpSocket}}; // XXX: prolly clash with tokio??

use tokio::time::Duration;

#[derive(Copy, Clone, Debug)]
enum WavelogMode {
    Cw,
    Phone,
    Digi,
}

impl FromStr for WavelogMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cw" => Ok(WavelogMode::Cw),
            "phone" => Ok(WavelogMode::Phone),
            "digi" => Ok(WavelogMode::Digi),
            _ => Err(()),
        }
    }
}

// If dial frequency is between any of these and +3kHz, then mode should probably be set for FT8
// 160m: 1.840 MHz
// 80m: 3.575 MHz
// 40m: 7.074 MHz
// 30m: 10.136 MHz
// 20m: 14.074 MHz
// 17m: 18.100 MHz
// 15m: 21.074 MHz
// 12m: 24.915 MHz
// 10m: 28.074 MHz
// 6m: 50.313 MHz
fn is_ft8(freq_hz: f64) -> bool {

    const LO_ALLOWANCE: f64 = 2_000.0;
    const HI_ALLOWANCE: f64 = 3_000.0;
    const FT8: [f64; 10] = [
        1_840_000.0,
        3_575_000.0,
        7_074_000.0,
        10_136_000.0,
        14_074_000.0,
        18_100_000.0,
        21_074_000.0,
        24_915_000.0,
        28_074_000.0,
        50_313_000.0,
    ];

    for ft8_lower in FT8 {
        if freq_hz >= ft8_lower - LO_ALLOWANCE && freq_hz < ft8_lower + HI_ALLOWANCE {
            return true;
        }
    }

    false
}

#[derive(Debug)]
struct Qsy {
    freq: f64,
    mode: WavelogMode,
}

fn http_err_str(
    status: StatusCode,
    msg: impl Into<String>,) -> HttpResponse {
    match Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(msg.into())))
    {
        Ok(resp) => resp,
        Err(_) => {
            // Satisfy Infallible for caller
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from_static(b"internal server error")))
                .unwrap_or_else(|_| {
                    // minimal response
                    Response::new(Full::new(Bytes::from_static(b"internal server error")))
                })
        }
    }
}


/// Parse '/14030000/cw' into a typed struct
fn parse_qsy_path(req: &Request<Incoming>) -> Result<Qsy, HttpResponse> {
    let parts: Vec<&str> = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect();

    if parts.len() != 2 {
        return Err(http_err_str(StatusCode::BAD_REQUEST,
                                "Expected /<freq>/<mode>"));
    }

    let freq: u32 = parts[0].parse::<u32>()
        .map_err(|_| http_err_str(StatusCode::BAD_REQUEST,
                                  "Frequency must be a positive integer"))?;

    let mode = parts[1].parse::<WavelogMode>()
        .map_err(|_| http_err_str(StatusCode::BAD_REQUEST,
                                  "Invalid mode"))?;
    Ok(Qsy { freq: freq as f64, mode })
}

fn wavelog_bandlist_to_flrig_mode(freq: f64, mode: WavelogMode) -> Mode {
    if is_ft8(freq) {
            Mode::D_USB
    } else {
        match mode {
            WavelogMode::Cw => Mode::CW,
            WavelogMode::Phone => if freq < 10_000_000.0 {
                Mode::LSB
            } else {
                Mode::USB
            },
            WavelogMode::Digi => Mode::RTTY,
        }
    }
}


async fn qsy(
    rig: Arc<flrig::FLRig>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {

    info!("qsy() called with: {}", &req.uri().path());

    let qsyinfo = match parse_qsy_path(&req) {
        Err(e) => return Ok(e), // Infallible
        Ok(q) => q,
    };

    info!("Got freq:{} mode:{:?}", qsyinfo.freq, qsyinfo.mode);
    let freq: f64 = qsyinfo.freq;
    let mode = wavelog_bandlist_to_flrig_mode(freq, qsyinfo.mode);

    if let Err(e) = rig.set_vfo(freq).await {
        return Ok(http_err_str(StatusCode::INTERNAL_SERVER_ERROR,
                               format!("Failed to set frequency: {e}")))
    };

    if let Err(e) = rig.set_mode(mode).await {
        return Ok(http_err_str(StatusCode::INTERNAL_SERVER_ERROR,
                               format!("Failed to set mode: {e}")))
    }

    let success: String = format!("QSY to {freq}Hz, mode:{mode:?}");
    Ok(Response::new(Full::new(Bytes::from(success))))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init();

    info!("clrigctl started with prototype Power and BandList support.\n");

    let settings = Settings::new().unwrap_or_else(|err| {
        eprintln!("Could not read settings: {err}");
        process::exit(1)
    });

    let mut radio_data_current = RadioData {
        key: settings.cloudlog.key,
        radio: settings.cloudlog.identifier,
        frequency: String::from(""),
        mode: String::from(""),
        power: String::from("0"),
    };

    let host = settings.flrig.host;
    let port = settings.flrig.port;
    let url = format!("{host}:{port}/");
    let url = Url::parse(&url)?;
    let maxpower: u32 = settings.flrig.maxpower.parse::<u32>().unwrap_or_else(|err| {
        eprintln!("maxpower must be a positive integer: {err}");
        process::exit(1)
    });

    let rig = Arc::new(flrig::FLRig::new(
        url,
        maxpower
    ));

    let rig_poll = rig.clone(); // clone Arc, not the underlying rig

    tokio::task::spawn(async move {
        loop {
            // MIGHT be able to call rig.get_update() here; it'll return NIL if nothing changed
            // We should also aim to reuse the single TCP connection for repeated requests, rather
            // than a new TCP socket request for every poll(!)
            //
            // If get_update() says somthing happened, try using system.multicall() to get multiple
            // fields from flrig in one go.
            //
            // NOTE that we might also need to do an initial start-of-day rig.get_info() to
            // establish initial data
            if let Ok(radio_data_new) = rig_poll.get_radio_data().await {
                if radio_data_current.frequency != radio_data_new.frequency
                    || radio_data_current.mode != radio_data_new.mode
                    || radio_data_current.power != radio_data_new.power
                {
                    radio_data_current.frequency = radio_data_new.frequency;
                    radio_data_current.mode = radio_data_new.mode;
                    radio_data_current.power = radio_data_new.power;

                    // if attempt to push VFO info to cloudlog fails this time,
                    // maybe the failure might be transient, and we should try next time
                    let _result = cloudlog::upload_live_radio_data(&settings.cloudlog.url,
                                                                   &radio_data_current)
                        .await;
                }
            } else if let Err(e) = rig_poll.get_radio_data().await {
                info!("Got err:{:#?}", e);
            };
            tokio::time::sleep(Duration::from_millis(settings.performance.interval)).await;
        }
    });

    // Separate thread for someone logging from WSJTX via UDP on port 2237
    // XXX: The address here needs to come from settings...
    tokio::task::spawn(async move {
        let socket = UdpSocket::bind("127.0.0.1:2237");
        match socket {
            Err(_) => println!("couldn't create socket"),
            Ok(socket) => wsjtx_rxloop(socket).await,
        }
    });

    // Listen on TCP socket for someone in Cloudlog/Wavelog clicking the bandmap
    let cat_ipv4: IpAddr = settings.CAT.host
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("Invalid IP address in settings.CAT.host: {}", settings.CAT.host));
    let cat_port: u16 = settings.CAT.port
        .parse()
        .expect("Invalid port number in settings.CAT.port");
    let addr = SocketAddr::from((cat_ipv4, cat_port));
    let listener = TcpListener::bind(addr).await?;

    loop {
        // accept a series of TCP connections arising from clicks on bandmap in Cloudlog/Wavelog
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let rig_for_qsy = rig.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .half_close(true)
                .serve_connection(io, service_fn( move |req| {
                    qsy(rig_for_qsy.clone(), req)
                }))
                .await
            {
                // This seems to happen if wavelog doesn't wait for the response to their second
                // attempt(!) to qsy, and drop the TCP connection early
                debug!("Error serving connection: {:?}", err);
            }
        });
    }
}
