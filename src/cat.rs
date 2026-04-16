use log::{debug, info};
use tokio_util::sync::CancellationToken;
use serde_json::json;

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
    /// FT8 dial frequencies in Hz. Overrides the built-in list when present.
    /// Example: ft8_frequencies = [1840000, 3575000, 7074000]
    pub ft8_frequencies: Option<Vec<u64>>,
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
    Am,
    Fm,
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
            "am" => Ok(WavelogMode::Am),
            "fm" => Ok(WavelogMode::Fm),
            _ => Err(()),
        }
    }
}

// Default FT8 dial frequencies (Hz).
// Overridable via ft8_frequencies in the [CAT] config section.
//
// 160m: 1.840 MHz
// 80m:  3.575 MHz
// 40m:  7.074 MHz
// 30m:  10.136 MHz
// 20m:  14.074 MHz
// 17m:  18.100 MHz
// 15m:  21.074 MHz
// 12m:  24.915 MHz
// 10m:  28.074 MHz
// 6m:   50.313 MHz
const DEFAULT_FT8_FREQS: [f64; 10] = [
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

//
// If dial frequency is within ±2–3 kHz of any entry in `freqs`, the mode should be FT8.
// See unit tests at end of file.
fn is_ft8(freq_hz: f64, freqs: &[f64]) -> bool {
    const LO_ALLOWANCE: f64 = 2_000.0;
    const HI_ALLOWANCE: f64 = 3_000.0;
    freqs.iter().any(|&f| freq_hz >= f - LO_ALLOWANCE && freq_hz < f + HI_ALLOWANCE)
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
fn wavelog_to_flrig_mode(freq: f64, mode: WavelogMode, ft8_freqs: &[f64]) -> Mode {
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
        WavelogMode::Digi => if is_ft8(freq, ft8_freqs) {
            Mode::D_USB
        } else {
            Mode::RTTY
        },
        WavelogMode::Rtty => if is_ft8(freq, ft8_freqs) {
            Mode::D_USB
        } else {
            Mode::RTTY
        },
        WavelogMode::Am => Mode::AM,
        WavelogMode::Fm => Mode::FM,
    }
}

// Yaesu version to handle explicit mode naming (-U vs -L)
fn wavelog_to_yaesu_flrig_mode(freq: f64, mode: WavelogMode, ft8_freqs: &[f64]) -> Mode {
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
        WavelogMode::Digi => if is_ft8(freq, ft8_freqs) {
            Mode::DATA_U
        } else {
            Mode::RTTY_U
        },
        WavelogMode::Rtty => if is_ft8(freq, ft8_freqs) {
            Mode::DATA_U
        } else {
            Mode::RTTY_U
        },
        WavelogMode::Am => Mode::AM,
        WavelogMode::Fm => Mode::FM,
    }
}

async fn qsy(
    rig: Arc<flrig::FLRig>,
    req: Request<hyper::body::Incoming>,
    yaesu: bool,
    ft8_freqs: Arc<[f64]>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    info!("qsy() called with: {}", &req.uri().path());

    let qsyinfo = match parse_qsy_path(&req) {
        Err(e) => return Ok(e), // Infallible
        Ok(q) => q,
    };

    info!("Got freq:{} mode:{:?}", qsyinfo.freq, qsyinfo.mode);
    let freq: f64 = qsyinfo.freq;

    let mode = match yaesu {
        true => wavelog_to_yaesu_flrig_mode(freq, qsyinfo.mode, &ft8_freqs),
        false => wavelog_to_flrig_mode(freq, qsyinfo.mode, &ft8_freqs),
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

    let body = json!({
        "status": "ok",
        "connected": true,
        "frequency": freq,
        "mode": mode.to_string(),
        "rig": rig.get_identifier(),
    })
    .to_string();

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
    token: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Listen on TCP socket for someone in Cloudlog/Wavelog clicking the bandmap
    let cat_ipv4: IpAddr =
        settings.host.trim().parse().unwrap_or_else(|_| {
            panic!("Invalid IP address in settings CAT.host: {}", settings.host)
        });
    let addr = SocketAddr::from((cat_ipv4, settings.port));

    let yaesu: bool = settings.yaesu;

    // Build the FT8 frequency list: use the config override if provided, otherwise defaults.
    let ft8_freqs: Arc<[f64]> = match settings.ft8_frequencies {
        Some(freqs) => freqs.iter().map(|&f| f as f64).collect::<Vec<f64>>().into(),
        None => Arc::from(DEFAULT_FT8_FREQS.as_slice()),
    };

    info!("Listening for CAT requests from Wavelog on: {:#?}", addr);
    info!("Yaesu mode is: {:#?}", yaesu);

    let listener = TcpListener::bind(addr).await?;

    loop {
        // accept a series of TCP connections arising from clicks on bandmap in Cloudlog/Wavelog
        let (stream, _) = tokio::select! {
            _ = token.cancelled() => {
                info!("CAT thread shutting down");
                return Ok(());
            }
            result = listener.accept() => result?,
        };
        let io = TokioIo::new(stream);
        let rig_for_qsy = rig.clone();
        let ft8_freqs_for_qsy = ft8_freqs.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .half_close(true)
                .serve_connection(
                    io,
                    service_fn(move |req| qsy(rig_for_qsy.clone(), req, yaesu, ft8_freqs_for_qsy.clone())),
                )
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
    //     * >= centre - 2kHz
    //     * <  centre + 3kHz
    //
    // Therefore we check either side of these boundaries
    //
    // Each band gets four boundary checks:
    //   - centre (must be in-window)
    //   - lower edge: centre - LO_ALLOWANCE (must be in-window)
    //   - just below lower edge (must NOT be in-window)
    //   - upper edge: centre + HI_ALLOWANCE (must NOT be in-window, it's exclusive)
    //
    // 160m: 1.840 MHz
    // 80m:  3.575 MHz
    // 40m:  7.074 MHz
    // 30m:  10.136 MHz
    // 20m:  14.074 MHz
    // 17m:  18.100 MHz
    // 15m:  21.074 MHz
    // 12m:  24.915 MHz
    // 10m:  28.074 MHz
    // 6m:   50.313 MHz

    // --- 160m ---
    #[test]
    fn ft8_160m() { assert!(is_ft8(1_840_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_160m_lower_edge() { assert!(is_ft8(1_838_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_160m_below() { assert!(!is_ft8(1_837_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_160m_above() { assert!(!is_ft8(1_843_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 80m ---
    #[test]
    fn ft8_80m() { assert!(is_ft8(3_575_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_80m_lower_edge() { assert!(is_ft8(3_573_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_80m_below() { assert!(!is_ft8(3_572_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_80m_above() { assert!(!is_ft8(3_578_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 40m ---
    #[test]
    fn ft8_40m() { assert!(is_ft8(7_074_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_40m_below() { assert!(!is_ft8(7_071_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_40m_lower() { assert!(is_ft8(7_072_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_40m_upper() { assert!(is_ft8(7_076_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_40m_above() { assert!(!is_ft8(7_077_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 30m ---
    #[test]
    fn ft8_30m() { assert!(is_ft8(10_136_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_30m_lower_edge() { assert!(is_ft8(10_134_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_30m_below() { assert!(!is_ft8(10_133_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_30m_above() { assert!(!is_ft8(10_139_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 20m ---
    #[test]
    fn ft8_20m() { assert!(is_ft8(14_074_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_20m_lower_edge() { assert!(is_ft8(14_072_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_20m_below() { assert!(!is_ft8(14_071_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_20m_above() { assert!(!is_ft8(14_077_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 17m ---
    #[test]
    fn ft8_17m() { assert!(is_ft8(18_100_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_17m_lower_edge() { assert!(is_ft8(18_098_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_17m_below() { assert!(!is_ft8(18_097_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_17m_above() { assert!(!is_ft8(18_103_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 15m ---
    #[test]
    fn ft8_15m() { assert!(is_ft8(21_074_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_15m_lower_edge() { assert!(is_ft8(21_072_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_15m_below() { assert!(!is_ft8(21_071_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_15m_above() { assert!(!is_ft8(21_077_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 12m ---
    #[test]
    fn ft8_12m() { assert!(is_ft8(24_915_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_12m_lower_edge() { assert!(is_ft8(24_913_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_12m_below() { assert!(!is_ft8(24_912_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_12m_above() { assert!(!is_ft8(24_918_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 10m ---
    #[test]
    fn ft8_10m() { assert!(is_ft8(28_074_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_10m_lower_edge() { assert!(is_ft8(28_072_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_10m_below() { assert!(!is_ft8(28_071_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_10m_above() { assert!(!is_ft8(28_077_000.0, &DEFAULT_FT8_FREQS)); }

    // --- 6m ---
    #[test]
    fn ft8_6m() { assert!(is_ft8(50_313_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_6m_lower_edge() { assert!(is_ft8(50_311_000.0, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_6m_below() { assert!(!is_ft8(50_310_999.9999, &DEFAULT_FT8_FREQS)); }
    #[test]
    fn ft8_6m_above() { assert!(!is_ft8(50_316_000.0, &DEFAULT_FT8_FREQS)); }

    //////////////////////////////////////////////////////////////
    // Tests for Bandlist/Cluster mode/frequency conversions to FLRig mode
    //////////////////////////////////////////////////////////////
    // FT8 detection only overrides Digi and Rtty; other modes pass through normally.
    #[test]
    fn flrig_40m_ft8_digi_rtty_become_d_usb() {
        const FT8_40M: f64 = 7_074_000.0;
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS), Mode::D_USB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS), Mode::D_USB);
    }

    #[test]
    fn flrig_40m_ft8_other_modes_unaffected() {
        const FT8_40M: f64 = 7_074_000.0;
        // CW stays CW, Phone→LSB (below 10 MHz), explicit LSB/USB pass through, AM/FM unchanged
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Cw,    &DEFAULT_FT8_FREQS), Mode::CW);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Phone,  &DEFAULT_FT8_FREQS), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::LSB,    &DEFAULT_FT8_FREQS), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::USB,    &DEFAULT_FT8_FREQS), Mode::USB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Am,     &DEFAULT_FT8_FREQS), Mode::AM);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Fm,     &DEFAULT_FT8_FREQS), Mode::FM);
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
                wavelog_to_flrig_mode(freq, WavelogMode::Cw, &DEFAULT_FT8_FREQS),
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
                wavelog_to_flrig_mode(freq, WavelogMode::Phone, &DEFAULT_FT8_FREQS),
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
                wavelog_to_flrig_mode(freq, WavelogMode::LSB, &DEFAULT_FT8_FREQS),
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
                wavelog_to_flrig_mode(freq, WavelogMode::USB, &DEFAULT_FT8_FREQS),
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
                wavelog_to_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS),
                Mode::RTTY
            );

            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS),
                Mode::RTTY
            );
        }
    }

    #[test]
    fn flrig_am_fm() {
        // AM and FM should map through regardless of frequency (no FT8 override applies)
        assert_eq!(wavelog_to_flrig_mode(7_200_000.0, WavelogMode::Am, &DEFAULT_FT8_FREQS), Mode::AM);
        assert_eq!(wavelog_to_flrig_mode(29_600_000.0, WavelogMode::Fm, &DEFAULT_FT8_FREQS), Mode::FM);
    }

    #[test]
    fn is_ft8_custom_freqs() {
        // With a custom single-entry list, only that frequency window should match.
        let custom: [f64; 1] = [14_074_000.0];
        assert!(is_ft8(14_074_000.0, &custom));
        assert!(!is_ft8(7_074_000.0, &custom));
    }

    // Also test 20m Phone → USB (above 10 MHz boundary) for the standard path.
    #[test]
    fn flrig_20m_phone_becomes_usb() {
        assert_eq!(wavelog_to_flrig_mode(14_225_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS), Mode::USB);
    }

    //////////////////////////////////////////////////////////////
    // Tests for Yaesu mode conversions (wavelog_to_yaesu_flrig_mode)
    // Key differences from the standard path:
    //   CW  → CW_U   (not CW)
    //   Digi/Rtty at FT8 freq → DATA_U  (not D_USB)
    //   Digi/Rtty elsewhere   → RTTY_U  (not RTTY)
    //////////////////////////////////////////////////////////////

    #[test]
    fn yaesu_ft8_digi_rtty_become_data_u() {
        const FT8_40M: f64 = 7_074_000.0;
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS), Mode::DATA_U);
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS), Mode::DATA_U);
    }

    #[test]
    fn yaesu_ft8_other_modes_unaffected() {
        const FT8_40M: f64 = 7_074_000.0;
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::Cw,   &DEFAULT_FT8_FREQS), Mode::CW_U);
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::Phone, &DEFAULT_FT8_FREQS), Mode::LSB);
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::LSB,   &DEFAULT_FT8_FREQS), Mode::LSB);
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::USB,   &DEFAULT_FT8_FREQS), Mode::USB);
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::Am,    &DEFAULT_FT8_FREQS), Mode::AM);
        assert_eq!(wavelog_to_yaesu_flrig_mode(FT8_40M, WavelogMode::Fm,    &DEFAULT_FT8_FREQS), Mode::FM);
    }

    #[test]
    fn yaesu_40m_cw_becomes_cw_u() {
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_yaesu_flrig_mode(freq, WavelogMode::Cw, &DEFAULT_FT8_FREQS), Mode::CW_U);
        }
    }

    #[test]
    fn yaesu_phone_lsb_usb_boundary() {
        // Below 10 MHz → LSB; at or above → USB (same split as non-Yaesu)
        assert_eq!(wavelog_to_yaesu_flrig_mode(7_150_000.0,  WavelogMode::Phone, &DEFAULT_FT8_FREQS), Mode::LSB);
        assert_eq!(wavelog_to_yaesu_flrig_mode(14_225_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS), Mode::USB);
        // Explicit LSB/USB always pass through
        assert_eq!(wavelog_to_yaesu_flrig_mode(7_150_000.0,  WavelogMode::LSB, &DEFAULT_FT8_FREQS), Mode::LSB);
        assert_eq!(wavelog_to_yaesu_flrig_mode(14_225_000.0, WavelogMode::USB, &DEFAULT_FT8_FREQS), Mode::USB);
    }

    #[test]
    fn yaesu_40m_digi_rtty_become_rtty_u() {
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_yaesu_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS), Mode::RTTY_U);
            assert_eq!(wavelog_to_yaesu_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS), Mode::RTTY_U);
        }
    }

    #[test]
    fn yaesu_am_fm() {
        assert_eq!(wavelog_to_yaesu_flrig_mode(7_200_000.0,  WavelogMode::Am, &DEFAULT_FT8_FREQS), Mode::AM);
        assert_eq!(wavelog_to_yaesu_flrig_mode(29_600_000.0, WavelogMode::Fm, &DEFAULT_FT8_FREQS), Mode::FM);
    }
}
