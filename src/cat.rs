use log::{debug, info};
use tokio_util::sync::CancellationToken;
use serde_json::json;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_derive::Deserialize;
use std::net::Ipv4Addr;
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

use crate::{flrig, flrig::Mode, flrig::ModeMap};

const CAT_BIND_HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;

pub fn generate_cat_token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("Failed to read from /dev/urandom — cannot generate CAT token");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// UK amateur frequency allocations permitted across all licence classes
// (Foundation as the common baseline), in Hz.
// Source: Ofcom Amateur Radio Licence Tables A–C, October 2025.
// Excluded: 472–479 kHz (Full licence only) and the 5 MHz channels
// (Full licence only, non-contiguous, specialist conditions).
// Microwave bands above 70cm omitted; add entries here if a supported
// rig needs them.
const AMATEUR_BANDS_HZ: &[(u32, u32)] = &[
    (135_700,     137_800),     // 136 kHz
    (1_810_000,   2_000_000),   // 160m
    (3_500_000,   3_800_000),   // 80m
    (7_000_000,   7_200_000),   // 40m
    (10_100_000,  10_150_000),  // 30m
    (14_000_000,  14_350_000),  // 20m
    (18_068_000,  18_168_000),  // 17m
    (21_000_000,  21_450_000),  // 15m
    (24_890_000,  24_990_000),  // 12m
    (28_000_000,  29_700_000),  // 10m
    (50_000_000,  52_000_000),  // 6m
    (70_000_000,  70_500_000),  // 4m
    (144_000_000, 146_000_000), // 2m
    (430_000_000, 440_000_000), // 70cm
];

fn is_amateur_frequency(freq_hz: u32) -> bool {
    AMATEUR_BANDS_HZ.iter().any(|&(lo, hi)| freq_hz >= lo && freq_hz <= hi)
}

#[derive(Debug, Deserialize)]
pub struct CatSettings {
    pub port: u16,
    /// FLRig mode string to use for CW.  Defaults to "CW" (ICOM/Kenwood/Elecraft).
    /// Set to "CW-U" for Yaesu rigs that require an explicit sideband suffix.
    pub cw_mode: Option<String>,
    /// FLRig mode string to use for RTTY.  Defaults to "RTTY".
    /// Use "RTTY-U" for Yaesu, "FSK" for Kenwood rigs that name it differently.
    pub rtty_mode: Option<String>,
    /// FLRig mode string to use for digital modes (FT8, PSK31, etc.).
    /// Defaults to "D-USB" (IC-703).  Use "DATA-U" for Yaesu, "USB-D" for
    /// newer ICOM rigs (IC-7300 etc.), "DATA" for Elecraft.
    pub digital_mode: Option<String>,
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

// Parse '/<cat_token>/<freq>/<mode>' into a typed struct: Qsy
fn parse_qsy_path<B>(req: &Request<B>, cat_token: &str) -> Result<Qsy, Box<HttpResponse>> {
    let parts: Vec<&str> = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect();

    if parts.len() != 3 {
        return Err(Box::new(http_err_str(
            StatusCode::BAD_REQUEST,
            "Expected /<token>/<freq>/<mode>",
        )));
    }

    if parts[0] != cat_token {
        return Err(Box::new(http_err_str(
            StatusCode::UNAUTHORIZED,
            "Invalid token",
        )));
    }

    let freq: u32 = parts[1].parse::<u32>().map_err(|_| {
        Box::new(http_err_str(
            StatusCode::BAD_REQUEST,
            "Frequency must be a positive integer",
        ))
    })?;

    if !is_amateur_frequency(freq) {
        return Err(Box::new(http_err_str(
            StatusCode::BAD_REQUEST,
            format!("{freq} Hz is outside permitted UK amateur allocations"),
        )));
    }

    let mode = parts[2]
        .parse::<WavelogMode>()
        .map_err(|_| Box::new(http_err_str(StatusCode::BAD_REQUEST, "Invalid mode")))?;
    Ok(Qsy {
        freq: freq as f64,
        mode,
    })
}

// Map a Wavelog bandmap mode + frequency to the FLRig mode string for the
// connected rig.  The rig-specific mode names (e.g. "CW-U" vs "CW") come
// from the ModeMap built at startup from the [CAT] config section.
//
// Heuristics applied:
// * Digi/RTTY at a known FT8 frequency → mode_map.digital (the rig's data mode)
// * Digi/RTTY elsewhere               → mode_map.rtty
// * Phone below 10 MHz                → LSB (convention)
// * Phone at or above 10 MHz          → USB (convention)
// * Explicit LSB/USB/AM/FM/CW         → pass straight through via the mode map
fn wavelog_to_flrig_mode(freq: f64, mode: WavelogMode, ft8_freqs: &[f64], mode_map: &ModeMap) -> Mode {
    match mode {
        WavelogMode::Cw => mode_map.cw,
        WavelogMode::Phone => if freq < 10_000_000.0 { Mode::LSB } else { Mode::USB },
        WavelogMode::LSB => Mode::LSB,
        WavelogMode::USB => Mode::USB,
        WavelogMode::Digi | WavelogMode::Rtty => {
            if is_ft8(freq, ft8_freqs) { mode_map.digital } else { mode_map.rtty }
        },
        WavelogMode::Am => Mode::AM,
        WavelogMode::Fm => Mode::FM,
    }
}

async fn qsy(
    rig: Arc<flrig::FLRig>,
    req: Request<hyper::body::Incoming>,
    mode_map: Arc<ModeMap>,
    ft8_freqs: Arc<[f64]>,
    cat_token: Arc<String>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    info!("qsy() called");

    let qsyinfo = match parse_qsy_path(&req, &cat_token) {
        Err(e) => return Ok(*e), // Infallible
        Ok(q) => q,
    };

    info!("Got freq:{} mode:{:?}", qsyinfo.freq, qsyinfo.mode);
    let freq: f64 = qsyinfo.freq;

    let mode = wavelog_to_flrig_mode(freq, qsyinfo.mode, &ft8_freqs, &mode_map);

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
    cat_token: Arc<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Listen on TCP socket for someone in Cloudlog/Wavelog clicking the bandmap
    let addr = SocketAddr::from((CAT_BIND_HOST, settings.port));

    // Build the mode map from config; defaults to ICOM/generic names if fields are absent.
    let mode_map: Arc<ModeMap> = Arc::new(flrig::build_mode_map(
        settings.cw_mode.as_deref(),
        settings.rtty_mode.as_deref(),
        settings.digital_mode.as_deref(),
    ));

    // Build the FT8 frequency list: use the config override if provided, otherwise defaults.
    let ft8_freqs: Arc<[f64]> = match settings.ft8_frequencies {
        Some(freqs) => freqs.iter().map(|&f| f as f64).collect::<Vec<f64>>().into(),
        None => Arc::from(DEFAULT_FT8_FREQS.as_slice()),
    };

    info!("Listening for CAT requests from Wavelog on: {:#?}", addr);

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
        let mode_map_for_qsy = mode_map.clone();
        let ft8_freqs_for_qsy = ft8_freqs.clone();
        let cat_token_for_qsy = cat_token.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .half_close(true)
                .serve_connection(
                    io,
                    service_fn(move |req| qsy(rig_for_qsy.clone(), req, mode_map_for_qsy.clone(), ft8_freqs_for_qsy.clone(), cat_token_for_qsy.clone())),
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

    // Helpers: construct a ModeMap for testing without config or a live FLRig.
    fn icom_mode_map() -> ModeMap { flrig::build_mode_map(None, None, None) }
    fn yaesu_mode_map() -> ModeMap { flrig::build_mode_map(Some("CW-U"), Some("RTTY-U"), Some("DATA-U")) }

    //////////////////////////////////////////////////////////////
    // Tests for Bandlist/Cluster mode/frequency conversions (ICOM/generic map)
    // FT8 detection only overrides Digi and Rtty; other modes pass through.
    //////////////////////////////////////////////////////////////

    #[test]
    fn flrig_40m_ft8_digi_rtty_become_d_usb() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = icom_mode_map();
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m), Mode::D_USB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m), Mode::D_USB);
    }

    #[test]
    fn flrig_40m_ft8_other_modes_unaffected() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = icom_mode_map();
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Cw,    &DEFAULT_FT8_FREQS, &m), Mode::CW);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Phone,  &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::LSB,    &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::USB,    &DEFAULT_FT8_FREQS, &m), Mode::USB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Am,     &DEFAULT_FT8_FREQS, &m), Mode::AM);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Fm,     &DEFAULT_FT8_FREQS, &m), Mode::FM);
    }

    #[test]
    fn flrig_40m_cw() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [7_000_000.0, 7_030_000.0, 7_100_000.0, 7_185_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Cw, &DEFAULT_FT8_FREQS, &m), Mode::CW);
        }
    }

    #[test]
    fn flrig_40m_phone() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [7_000_000.0, 7_030_000.0, 7_100_000.0, 7_185_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        }
    }

    #[test]
    fn flrig_40m_lsb() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [7_000_000.0, 7_030_000.0, 7_100_000.0, 7_185_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::LSB, &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        }
    }

    #[test]
    fn flrig_40m_usb() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [7_000_000.0, 7_030_000.0, 7_100_000.0, 7_185_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::USB, &DEFAULT_FT8_FREQS, &m), Mode::USB);
        }
    }

    #[test]
    fn flrig_40m_digi_rtty() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [7_000_000.0, 7_030_000.0, 7_100_000.0, 7_185_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m), Mode::RTTY);
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m), Mode::RTTY);
        }
    }

    #[test]
    fn flrig_am_fm() {
        let m = icom_mode_map();
        assert_eq!(wavelog_to_flrig_mode(7_200_000.0,  WavelogMode::Am, &DEFAULT_FT8_FREQS, &m), Mode::AM);
        assert_eq!(wavelog_to_flrig_mode(29_600_000.0, WavelogMode::Fm, &DEFAULT_FT8_FREQS, &m), Mode::FM);
    }

    #[test]
    fn flrig_20m_phone_becomes_usb() {
        let m = icom_mode_map();
        assert_eq!(wavelog_to_flrig_mode(14_225_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m), Mode::USB);
    }

    #[test]
    fn is_ft8_custom_freqs() {
        let custom: [f64; 1] = [14_074_000.0];
        assert!(is_ft8(14_074_000.0, &custom));
        assert!(!is_ft8(7_074_000.0, &custom));
    }

    //////////////////////////////////////////////////////////////
    // Tests for Yaesu mode map (CW→CW_U, Digi/Rtty→DATA_U or RTTY_U)
    //////////////////////////////////////////////////////////////

    #[test]
    fn yaesu_ft8_digi_rtty_become_data_u() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = yaesu_mode_map();
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m), Mode::DATA_U);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m), Mode::DATA_U);
    }

    #[test]
    fn yaesu_ft8_other_modes_unaffected() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = yaesu_mode_map();
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Cw,    &DEFAULT_FT8_FREQS, &m), Mode::CW_U);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Phone,  &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::LSB,    &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::USB,    &DEFAULT_FT8_FREQS, &m), Mode::USB);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Am,     &DEFAULT_FT8_FREQS, &m), Mode::AM);
        assert_eq!(wavelog_to_flrig_mode(FT8_40M, WavelogMode::Fm,     &DEFAULT_FT8_FREQS, &m), Mode::FM);
    }

    #[test]
    fn yaesu_40m_cw_becomes_cw_u() {
        let m = yaesu_mode_map();
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Cw, &DEFAULT_FT8_FREQS, &m), Mode::CW_U);
        }
    }

    #[test]
    fn yaesu_phone_lsb_usb_boundary() {
        let m = yaesu_mode_map();
        assert_eq!(wavelog_to_flrig_mode(7_150_000.0,  WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(14_225_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m), Mode::USB);
        assert_eq!(wavelog_to_flrig_mode(7_150_000.0,  WavelogMode::LSB,   &DEFAULT_FT8_FREQS, &m), Mode::LSB);
        assert_eq!(wavelog_to_flrig_mode(14_225_000.0, WavelogMode::USB,   &DEFAULT_FT8_FREQS, &m), Mode::USB);
    }

    #[test]
    fn yaesu_40m_digi_rtty_become_rtty_u() {
        let m = yaesu_mode_map();
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m), Mode::RTTY_U);
            assert_eq!(wavelog_to_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m), Mode::RTTY_U);
        }
    }

    #[test]
    fn yaesu_am_fm() {
        let m = yaesu_mode_map();
        assert_eq!(wavelog_to_flrig_mode(7_200_000.0,  WavelogMode::Am, &DEFAULT_FT8_FREQS, &m), Mode::AM);
        assert_eq!(wavelog_to_flrig_mode(29_600_000.0, WavelogMode::Fm, &DEFAULT_FT8_FREQS, &m), Mode::FM);
    }

    //////////////////////////////////////////////////////////////
    // Tests for parse_qsy_path input validation
    //////////////////////////////////////////////////////////////

    const TEST_TOKEN: &str = "testtoken";

    fn make_get(path: &str) -> Request<()> {
        Request::builder()
            .uri(format!("/{TEST_TOKEN}{path}"))
            .body(())
            .unwrap()
    }

    // --- Malformed paths ---

    #[test]
    fn qsy_path_single_segment_rejected() {
        // No token or freq/mode at all.
        let req = Request::builder().uri("/14030000").body(()).unwrap();
        assert!(parse_qsy_path(&req, TEST_TOKEN).is_err());
    }

    #[test]
    fn qsy_path_empty_rejected() {
        let req = Request::builder().uri("/").body(()).unwrap();
        assert!(parse_qsy_path(&req, TEST_TOKEN).is_err());
    }

    #[test]
    fn qsy_path_four_segments_rejected() {
        assert!(parse_qsy_path(&make_get("/14030000/cw/extra"), TEST_TOKEN).is_err());
    }

    // --- Token check ---

    #[test]
    fn qsy_wrong_token_rejected() {
        let req = Request::builder()
            .uri("/wrongtoken/14074000/usb")
            .body(())
            .unwrap();
        assert!(parse_qsy_path(&req, TEST_TOKEN).is_err());
    }

    // --- Frequency allowlist: out-of-band inputs rejected ---

    #[test]
    fn qsy_rejects_zero_frequency() {
        assert!(parse_qsy_path(&make_get("/0/usb"), TEST_TOKEN).is_err(),
            "frequency 0 Hz must be rejected");
    }

    #[test]
    fn qsy_rejects_broadcast_band_frequency() {
        // 909 kHz is an AM broadcast frequency, not an amateur allocation.
        assert!(parse_qsy_path(&make_get("/909000/usb"), TEST_TOKEN).is_err(),
            "broadcast-band frequency 909 kHz must be rejected");
    }

    #[test]
    fn qsy_rejects_max_u32_frequency() {
        // 4,294,967,295 Hz (~4.3 GHz) is not an amateur allocation.
        assert!(parse_qsy_path(&make_get("/4294967295/usb"), TEST_TOKEN).is_err(),
            "out-of-range frequency 4294967295 Hz must be rejected");
    }

    #[test]
    fn qsy_rejects_between_bands() {
        // 11 MHz falls between 30m (10.15 MHz) and 20m (14.0 MHz).
        assert!(parse_qsy_path(&make_get("/11000000/usb"), TEST_TOKEN).is_err(),
            "inter-band frequency 11 MHz must be rejected");
    }

    // --- Frequency allowlist: valid in-band inputs accepted ---

    #[test]
    fn qsy_accepts_valid_hf_frequencies() {
        let valid = [
            "/1840000/usb",   // 160m
            "/3573000/usb",   // 80m FT8
            "/7074000/usb",   // 40m FT8
            "/10136000/usb",  // 30m FT8
            "/14074000/usb",  // 20m FT8
            "/18100000/usb",  // 17m FT8
            "/21074000/usb",  // 15m FT8
            "/24915000/usb",  // 12m FT8
            "/28074000/usb",  // 10m FT8
            "/50313000/usb",  // 6m FT8
        ];
        for path in valid {
            assert!(parse_qsy_path(&make_get(path), TEST_TOKEN).is_ok(),
                "expected Ok for {path}");
        }
    }

    // --- is_amateur_frequency: band edge boundary checks ---

    #[test]
    fn amateur_frequency_band_edges() {
        // Lower and upper edges of each band must be accepted (inclusive).
        for &(lo, hi) in AMATEUR_BANDS_HZ {
            assert!(is_amateur_frequency(lo), "{lo} Hz (band lower edge) should be accepted");
            assert!(is_amateur_frequency(hi), "{hi} Hz (band upper edge) should be accepted");
        }
    }

    #[test]
    fn amateur_frequency_outside_all_bands_rejected() {
        let out_of_band = [
            0,           // zero
            100_000,     // below 136 kHz band
            500_000,     // 500 kHz (between 136 kHz and 160m)
            472_000,     // 472 kHz (Full-only, excluded by policy)
            5_000_000,   // 5 MHz (Full-only specialist channels, excluded by policy)
            11_000_000,  // between 30m and 20m
            200_000_000, // between 70cm and anything above
            4_294_967_295, // max u32
        ];
        for freq in out_of_band {
            assert!(!is_amateur_frequency(freq), "{freq} Hz should be rejected");
        }
    }
}
