use log::{debug, info};

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_derive::Deserialize;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use hyper::body::{Bytes, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::{Request, Response, StatusCode};
use std::convert::Infallible;
use std::str::FromStr;

pub type HttpResponse = Response<Full<Bytes>>;

use http_body_util::Full;

use crate::{flrig, flrig::Mode};

#[derive(Debug, Deserialize)]
pub struct CatSettings {
    pub host: String,
    pub port: u16,
    pub yaesu: bool,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Copy, Clone, Debug)]
enum WavelogMode {
    Cw,
    Phone,
    LSB,
    USB,
    Digi,
    Rtty,
}

impl FromStr for WavelogMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cw" => Ok(WavelogMode::Cw),
            "phone" => Ok(WavelogMode::Phone),
            "lsb" => Ok(WavelogMode::LSB),
            "usb" => Ok(WavelogMode::USB),
            "digi" => Ok(WavelogMode::Digi),
            "rtty" => Ok(WavelogMode::Rtty),
            _ => Err(()),
        }
    }
}

//
// If dial frequency is between any of these and +3kHz, then mode should probably be set for FT8
// See simple unit tests at end of file.
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

fn http_err_str(status: StatusCode, msg: impl Into<String>) -> HttpResponse {
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

// Parse '/14030000/cw' into a typed struct: Qsy
fn parse_qsy_path(req: &Request<Incoming>) -> Result<Qsy, HttpResponse> {
    let parts: Vec<&str> = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect();

    if parts.len() != 2 {
        return Err(http_err_str(
            StatusCode::BAD_REQUEST,
            "Expected /<freq>/<mode>",
        ));
    }

    let freq: u32 = parts[0].parse::<u32>().map_err(|_| {
        http_err_str(
            StatusCode::BAD_REQUEST,
            "Frequency must be a positive integer",
        )
    })?;

    let mode = parts[1]
        .parse::<WavelogMode>()
        .map_err(|_| http_err_str(StatusCode::BAD_REQUEST, "Invalid mode"))?;
    Ok(Qsy {
        freq: freq as f64,
        mode,
    })
}

// When Wavelog requests CAT control to change transceiver frequency we try to provide some
// assistance because the modes emanating from the Wavelog Bandmap haven't always been great.
// Use some really simple heuristics to try to get things broadly correct:
//
// * If the frequency appears to be a known FT8 frequency, jump to the required mode
//   - The IC-703 has a D-USB mode
//
// * Otherwise if we're dealing with a phone mode, force that to LSB if the frequency is below 10MHz
//
// * But if someone is explicitly selecting USB, assume they know best, because LSB/USB is after all
//   only a convention.
//
// * If it's some other digital mode, then use RTTY
//   (perhaps I'll do more digi modes one day, and realise this behaviour is too naive...!)
//
// See unit tests at bottom of file
//
// Oh, but life is never simple, is it? Turns out FLRig replicates the modes displayed on a rig's
// panel rather than provide a single, brand-agnostic interface for transceiver mode.
// This is great for the GUI, but rubbish for XMLRPC.
// So on a Yaesu FTDX10 for example, there is no "CW" mode at all; one must explicitly select
// either CW-U or CW-L. Similarly, there's RTTY-U or RTTY-L as well...
fn wavelog_to_flrig_mode(freq: f64, mode: WavelogMode) -> Mode {
    if is_ft8(freq) {
        Mode::D_USB
    } else {
        match mode {
            WavelogMode::Cw => Mode::CW,
            WavelogMode::Phone => {
                if freq < 10_000_000.0 {
                    Mode::LSB
                } else {
                    Mode::USB
                }
            },
            WavelogMode::LSB => Mode::LSB,
            WavelogMode::USB => Mode::USB,
            WavelogMode::Digi => Mode::RTTY,
            WavelogMode::Rtty => Mode::RTTY,
        }
    }
}

// Yaesu version to handle explicit mode naming (-U vs -L)
fn wavelog_to_yaesu_flrig_mode(freq: f64, mode: WavelogMode) -> Mode {
    if is_ft8(freq) {
        Mode::DATA_U
    } else {
        match mode {
            WavelogMode::Cw => Mode::CW_U,
            WavelogMode::Phone => {
                if freq < 10_000_000.0 {
                    Mode::LSB
                } else {
                    Mode::USB
                }
            },
            WavelogMode::LSB => Mode::LSB,
            WavelogMode::USB => Mode::USB,
            WavelogMode::Digi => Mode::RTTY_U,
            WavelogMode::Rtty => Mode::RTTY_U,
        }
    }
}

async fn qsy(
    rig: Arc<flrig::FLRig>,
    req: Request<hyper::body::Incoming>,
    yaesu: bool,
) -> Result<Response<Full<Bytes>>, Infallible> {
    info!("qsy() called with: {}", &req.uri().path());

    let qsyinfo = match parse_qsy_path(&req) {
        Err(e) => return Ok(e), // Infallible
        Ok(q) => q,
    };

    info!("Got freq:{} mode:{:?}", qsyinfo.freq, qsyinfo.mode);
    let freq: f64 = qsyinfo.freq;

    let mode = match yaesu {
        true  => wavelog_to_yaesu_flrig_mode(freq, qsyinfo.mode),
        false => wavelog_to_flrig_mode(freq, qsyinfo.mode),
    };

    if let Err(e) = rig.set_vfo(freq).await {
        return Ok(http_err_str(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to set frequency: {e}"),
        ));
    };

    if let Err(e) = rig.set_mode(mode).await {
        return Ok(http_err_str(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to set mode: {e}"),
        ));
    }

    let body = format!(
        r#"{{
    "status": "ok",
    "connected": true,
    "frequency": {},
    "mode": "{}",
    "rig": "{}"
}}
"#,
        freq,
        mode,
        rig.get_identifier(),
    );

    Ok(Response::builder()
        .status(200)
        .header(CONTENT_TYPE, "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "Content-Type")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

#[allow(non_snake_case)]
pub async fn CAT_thread(
    settings: CatSettings,
    rig: &Arc<flrig::FLRig>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Listen on TCP socket for someone in Cloudlog/Wavelog clicking the bandmap
    let cat_ipv4: IpAddr =
        settings.host.trim().parse().unwrap_or_else(|_| {
            panic!("Invalid IP address in settings CAT.host: {}", settings.host)
        });
    let addr = SocketAddr::from((cat_ipv4, settings.port));

    let yaesu: bool = settings.yaesu;

    info!("Listening for CAT requests from Wavelog on: {:#?}", addr);
    info!("Yaesu mode is: {:#?}", yaesu);

    let listener = TcpListener::bind(addr).await?;

    loop {
        // accept a series of TCP connections arising from clicks on bandmap in Cloudlog/Wavelog
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let rig_for_qsy = rig.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .half_close(true)
                .serve_connection(io, service_fn(move |req| qsy(rig_for_qsy.clone(), req, yaesu)))
                .await
            {
                // This seems to happen if wavelog doesn't wait for the response to their second
                // attempt(!) to qsy, and drop the TCP connection early
                debug!("Error serving connection: {:?}", err);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    //////////////////////////////////////////////////////////////
    // Tests for FT8 frequency identification
    //////////////////////////////////////////////////////////////
    // This file assumes the following centres of activity for FT8
    // Further more, the is_ft8() function checks for:
    //     * >= centre + 2kHz
    //     * <  centre + 3kHz
    //
    // Therefore we check either side of these boundaries
    //
    // XXX: Finish these off:
    // 160m: 1.840 MHz
    // 80m: 3.575 MHz
    // 40m: 7.074 MHz -- implemented
    // 30m: 10.136 MHz
    // 20m: 14.074 MHz
    // 17m: 18.100 MHz
    // 15m: 21.074 MHz
    // 12m: 24.915 MHz
    // 10m: 28.074 MHz
    // 6m: 50.313 MHz
    #[test]
    fn ft8_40m() {
        const FT8_40M: f64 = 7_074_000.0;
        assert!(is_ft8(FT8_40M));
    }

    #[test]
    fn ft8_40m_below() {
        const FT8_40M_TOO_LOW: f64 = 7_071_999.9999;
        assert!(!is_ft8(FT8_40M_TOO_LOW));
    }

    #[test]
    fn ft8_40m_lower() {
        const FT8_40M_LOWER: f64 = 7_072_000.0;
        assert!(is_ft8(FT8_40M_LOWER));
    }

    #[test]
    fn ft8_40m_upper() {
        const FT8_40M_UPPER: f64 = 7_076_999.9999;
        assert!(is_ft8(FT8_40M_UPPER));
    }

    #[test]
    fn ft8_40m_above() {
        const FT8_40M_TOO_HIGH: f64 = 7_077_000.0;
        assert!(!is_ft8(FT8_40M_TOO_HIGH));
    }

    //////////////////////////////////////////////////////////////
    // Tests for Bandlist/Cluster mode/frequency conversions to FLRig mode
    //////////////////////////////////////////////////////////////
    #[test]
    fn flrig_40m_ft8() {
        const FT8_40M: f64 = 7_074_000.0;

        const ALL_WL_MODES: [WavelogMode; 6] = [
            WavelogMode::Cw,
            WavelogMode::Phone,
            WavelogMode::LSB,
            WavelogMode::USB,
            WavelogMode::Digi,
            WavelogMode::Rtty,
        ];

        for wl_mode in ALL_WL_MODES {
            assert_eq!(
                wavelog_bandlist_to_flrig_mode(FT8_40M, wl_mode),
                Mode::D_USB
            );
        }
    }

    #[test]
    fn flrig_40m_cw() {
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];

        for freq in BAND_40M {
            assert_eq!(
                wavelog_bandlist_to_flrig_mode(freq, WavelogMode::Cw),
                Mode::CW
            );
        }
    }

    #[test]
    fn flrig_40m_phone() {
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];

        for freq in BAND_40M {
            assert_eq!(
                wavelog_bandlist_to_flrig_mode(freq, WavelogMode::Phone),
                Mode::LSB
            );
        }
    }

    #[test]
    fn flrig_40m_lsb() {
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];

        for freq in BAND_40M {
            assert_eq!(
                wavelog_bandlist_to_flrig_mode(freq, WavelogMode::LSB),
                Mode::LSB
            );
        }
    }

    #[test]
    fn flrig_40m_usb() {
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];

        for freq in BAND_40M {
            assert_eq!(
                wavelog_bandlist_to_flrig_mode(freq, WavelogMode::USB),
                Mode::USB
            );
        }
    }

    #[test]
    fn flrig_40m_digi_rtty() {
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];

        for freq in BAND_40M {
            assert_eq!(
                wavelog_bandlist_to_flrig_mode(freq, WavelogMode::Digi),
                Mode::RTTY
            );

            assert_eq!(
                wavelog_bandlist_to_flrig_mode(freq, WavelogMode::Rtty),
                Mode::RTTY
            );
        }
    }
}
