use log::{debug, info};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use hyper::body::Bytes;
use hyper::header::CONTENT_TYPE;
use hyper::{Request, Response, StatusCode};
use std::convert::Infallible;
use std::str::FromStr;

pub type HttpResponse = Response<Full<Bytes>>;

use http_body_util::Full;

use crate::{flrig, flrig::Mode, flrig::ModeMap};

const CAT_BIND_HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;

// UK amateur frequency allocations available to Foundation-class licensees
// (taken as the common baseline), in Hz.
// Source: Ofcom Amateur Radio Licence Tables A–C, October 2025.
// Excluded: 135.7–137.8 kHz and 472–479 kHz (both non-Foundation) and the
// 5 MHz channels (Full licence only, non-contiguous, specialist conditions).
// Microwave bands above 70cm omitted; add entries here if a supported
// rig needs them.
const AMATEUR_BANDS_HZ: &[(u32, u32)] = &[
    (1_810_000, 2_000_000),     // 160m
    (3_500_000, 3_800_000),     // 80m
    (7_000_000, 7_200_000),     // 40m
    (10_100_000, 10_150_000),   // 30m
    (14_000_000, 14_350_000),   // 20m
    (18_068_000, 18_168_000),   // 17m
    (21_000_000, 21_450_000),   // 15m
    (24_890_000, 24_990_000),   // 12m
    (28_000_000, 29_700_000),   // 10m
    (50_000_000, 52_000_000),   // 6m
    (70_000_000, 70_500_000),   // 4m
    (144_000_000, 146_000_000), // 2m
    (430_000_000, 440_000_000), // 70cm
];

// Hard-forbidden frequency ranges.  Any emission whose extended sideband
// range overlaps an entry here is refused unconditionally, regardless of
// mode or licence class.
const FORBIDDEN_RANGES_HZ: &[(u32, u32)] = &[(431_000_000, 432_000_000)];

// Soft sanity bounds for parse_qsy_path: anything outside this range is
// out of scope for wlrigctl and rejected before mode resolution.  The
// real band-and-mode check happens in is_emission_in_band.
// Lower: bottom of 160m (lowest Foundation allocation).
// Upper: top of 70cm (highest band wlrigctl supports).
const MIN_PLAUSIBLE_FREQ_HZ: u32 = 1_810_000;
const MAX_PLAUSIBLE_FREQ_HZ: u32 = 440_000_000;

fn is_plausible_radio_frequency(freq_hz: u32) -> bool {
    (MIN_PLAUSIBLE_FREQ_HZ..=MAX_PLAUSIBLE_FREQ_HZ).contains(&freq_hz)
}

// Per-mode emission bandwidth offsets from the dial frequency, in Hz.
// Returns (lower_offset, upper_offset) such that emitted energy occupies
// [dial - lower_offset, dial + upper_offset].  Conservative envelopes:
// SSB rounded up to 3 kHz, narrow FM to 6 kHz, RTTY two-sided (per-rig
// sideband convention varies), CW gets a 1 kHz buffer at each band edge
// — the operator must consciously nudge the dial if they want closer.
// The 1 kHz CW buffer also subsumes any CW pitch-offset on transmit
// (typically 600–800 Hz on rigs that apply it).
fn mode_emission_offsets(mode: Mode) -> (u32, u32) {
    use Mode::*;
    match mode {
        LSB | D_LSB | DATA_L => (3_000, 0),
        USB | D_USB | DATA_U | USB_D | DATA | PSK => (0, 3_000),
        AM | AM_N => (3_000, 3_000),
        FM | FM_N | DATA_FM | DATA_FMN => (6_000, 6_000),
        RTTY | RTTY_U | RTTY_L | RTTY_R | FSK => (3_000, 3_000),
        CW | CW_U | CW_L | CW_R => (1_000, 1_000),
    }
}

// Returns true iff the full mode-aware occupied-emission range fits inside
// a single amateur-band allocation AND does not overlap any forbidden range.
fn is_emission_in_band(freq_hz: u32, mode: Mode) -> bool {
    let (lo_off, hi_off) = mode_emission_offsets(mode);
    let lower_emission = freq_hz.saturating_sub(lo_off);
    let upper_emission = freq_hz.saturating_add(hi_off);

    let in_amateur_band = AMATEUR_BANDS_HZ
        .iter()
        .any(|&(lo, hi)| lower_emission >= lo && upper_emission <= hi);

    let touches_forbidden = FORBIDDEN_RANGES_HZ
        .iter()
        .any(|&(lo, hi)| lower_emission <= hi && lo <= upper_emission);

    in_amateur_band && !touches_forbidden
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
    /// Expected value of the HTTP `Origin` header on incoming QSY requests.
    /// When set, requests whose `Origin` does not match are rejected with 403.
    /// Protects against browser-based CSRF from pages not served by Wavelog.
    /// Example: wavelog_origin = "https://wavelog.example.org"
    pub wavelog_origin: Option<String>,
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
    freqs
        .iter()
        .any(|&f| freq_hz >= f - LO_ALLOWANCE && freq_hz < f + HI_ALLOWANCE)
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

// Returns true if the request's Origin header matches `expected` exactly.
// browsers set Origin automatically and JS cannot override it, so this
// reliably blocks cross-origin browser CSRF.  Local non-browser processes
// can forge any header, so this is not a defence against them.
fn check_origin<B>(req: &Request<B>, expected: &str) -> bool {
    req.headers().get("origin").and_then(|v| v.to_str().ok()) == Some(expected)
}

// Parse '/<freq>/<mode>' into a typed struct: Qsy
fn parse_qsy_path<B>(req: &Request<B>) -> Result<Qsy, Box<HttpResponse>> {
    let parts: Vec<&str> = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect();

    let &[freq_str, mode_str] = parts.as_slice() else {
        debug!("parse_qsy_path: wrong segment count ({})", parts.len());
        return Err(Box::new(http_err_str(
            StatusCode::BAD_REQUEST,
            "Expected /<freq>/<mode>",
        )));
    };

    let freq: u32 = freq_str.parse::<u32>().map_err(|_| {
        Box::new(http_err_str(
            StatusCode::BAD_REQUEST,
            "Frequency must be a positive integer",
        ))
    })?;

    if !is_plausible_radio_frequency(freq) {
        return Err(Box::new(http_err_str(
            StatusCode::BAD_REQUEST,
            format!("{freq} Hz is outside the supported amateur frequency range"),
        )));
    }

    let mode = mode_str.parse::<WavelogMode>().map_err(|_| {
        debug!("parse_qsy_path: unrecognised mode {:?}", mode_str);
        Box::new(http_err_str(StatusCode::BAD_REQUEST, "Invalid mode"))
    })?;
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
fn wavelog_to_flrig_mode(
    freq: f64,
    mode: WavelogMode,
    ft8_freqs: &[f64],
    mode_map: &ModeMap,
) -> Mode {
    match mode {
        WavelogMode::Cw => mode_map.cw,
        WavelogMode::Phone => {
            if freq < 10_000_000.0 {
                Mode::LSB
            } else {
                Mode::USB
            }
        }
        WavelogMode::LSB => Mode::LSB,
        WavelogMode::USB => Mode::USB,
        WavelogMode::Digi | WavelogMode::Rtty => {
            if is_ft8(freq, ft8_freqs) {
                mode_map.digital
            } else {
                mode_map.rtty
            }
        }
        WavelogMode::Am => Mode::AM,
        WavelogMode::Fm => Mode::FM,
    }
}

async fn qsy(
    rig: Arc<flrig::FLRig>,
    req: Request<hyper::body::Incoming>,
    mode_map: Arc<ModeMap>,
    ft8_freqs: Arc<[f64]>,
    wavelog_origin: Option<Arc<String>>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    info!("qsy() called");

    if let Some(expected) = &wavelog_origin {
        if !check_origin(&req, expected) {
            debug!("qsy: Origin header missing or does not match configured wavelog_origin");
            return Ok(http_err_str(StatusCode::FORBIDDEN, "Forbidden"));
        }
    }

    let qsyinfo = match parse_qsy_path(&req) {
        Err(e) => return Ok(*e), // Infallible
        Ok(q) => q,
    };

    info!("Got freq:{} mode:{:?}", qsyinfo.freq, qsyinfo.mode);
    let freq: f64 = qsyinfo.freq;

    let mode = wavelog_to_flrig_mode(freq, qsyinfo.mode, &ft8_freqs, &mode_map);

    // Mode-aware band-edge check: reject if any part of the occupied
    // sideband bandwidth would fall outside UK amateur allocations or
    // touch a forbidden range.
    let freq_u32 = freq as u32;
    if !is_emission_in_band(freq_u32, mode) {
        let (lo_off, hi_off) = mode_emission_offsets(mode);
        let lower = freq_u32.saturating_sub(lo_off);
        let upper = freq_u32.saturating_add(hi_off);
        return Ok(http_err_str(
            StatusCode::BAD_REQUEST,
            format!(
                "{mode} at {freq_u32} Hz emits {lower}-{upper} Hz, outside permitted UK amateur allocations"
            ),
        ));
    }

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
) -> Result<(), std::io::Error> {
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

    let wavelog_origin: Option<Arc<String>> = settings.wavelog_origin.map(Arc::new);

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
        let wavelog_origin_for_qsy = wavelog_origin.clone();
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .half_close(true)
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        qsy(
                            rig_for_qsy.clone(),
                            req,
                            mode_map_for_qsy.clone(),
                            ft8_freqs_for_qsy.clone(),
                            wavelog_origin_for_qsy.clone(),
                        )
                    }),
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
    fn ft8_160m() {
        assert!(is_ft8(1_840_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_160m_lower_edge() {
        assert!(is_ft8(1_838_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_160m_below() {
        assert!(!is_ft8(1_837_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_160m_above() {
        assert!(!is_ft8(1_843_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 80m ---
    #[test]
    fn ft8_80m() {
        assert!(is_ft8(3_575_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_80m_lower_edge() {
        assert!(is_ft8(3_573_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_80m_below() {
        assert!(!is_ft8(3_572_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_80m_above() {
        assert!(!is_ft8(3_578_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 40m ---
    #[test]
    fn ft8_40m() {
        assert!(is_ft8(7_074_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_40m_below() {
        assert!(!is_ft8(7_071_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_40m_lower() {
        assert!(is_ft8(7_072_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_40m_upper() {
        assert!(is_ft8(7_076_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_40m_above() {
        assert!(!is_ft8(7_077_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 30m ---
    #[test]
    fn ft8_30m() {
        assert!(is_ft8(10_136_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_30m_lower_edge() {
        assert!(is_ft8(10_134_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_30m_below() {
        assert!(!is_ft8(10_133_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_30m_above() {
        assert!(!is_ft8(10_139_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 20m ---
    #[test]
    fn ft8_20m() {
        assert!(is_ft8(14_074_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_20m_lower_edge() {
        assert!(is_ft8(14_072_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_20m_below() {
        assert!(!is_ft8(14_071_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_20m_above() {
        assert!(!is_ft8(14_077_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 17m ---
    #[test]
    fn ft8_17m() {
        assert!(is_ft8(18_100_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_17m_lower_edge() {
        assert!(is_ft8(18_098_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_17m_below() {
        assert!(!is_ft8(18_097_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_17m_above() {
        assert!(!is_ft8(18_103_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 15m ---
    #[test]
    fn ft8_15m() {
        assert!(is_ft8(21_074_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_15m_lower_edge() {
        assert!(is_ft8(21_072_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_15m_below() {
        assert!(!is_ft8(21_071_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_15m_above() {
        assert!(!is_ft8(21_077_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 12m ---
    #[test]
    fn ft8_12m() {
        assert!(is_ft8(24_915_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_12m_lower_edge() {
        assert!(is_ft8(24_913_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_12m_below() {
        assert!(!is_ft8(24_912_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_12m_above() {
        assert!(!is_ft8(24_918_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 10m ---
    #[test]
    fn ft8_10m() {
        assert!(is_ft8(28_074_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_10m_lower_edge() {
        assert!(is_ft8(28_072_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_10m_below() {
        assert!(!is_ft8(28_071_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_10m_above() {
        assert!(!is_ft8(28_077_000.0, &DEFAULT_FT8_FREQS));
    }

    // --- 6m ---
    #[test]
    fn ft8_6m() {
        assert!(is_ft8(50_313_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_6m_lower_edge() {
        assert!(is_ft8(50_311_000.0, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_6m_below() {
        assert!(!is_ft8(50_310_999.9999, &DEFAULT_FT8_FREQS));
    }
    #[test]
    fn ft8_6m_above() {
        assert!(!is_ft8(50_316_000.0, &DEFAULT_FT8_FREQS));
    }

    // Helpers: construct a ModeMap for testing without config or a live FLRig.
    fn icom_mode_map() -> ModeMap {
        flrig::build_mode_map(None, None, None)
    }
    fn yaesu_mode_map() -> ModeMap {
        flrig::build_mode_map(Some("CW-U"), Some("RTTY-U"), Some("DATA-U"))
    }
    fn kenwood_mode_map() -> ModeMap {
        flrig::build_mode_map(Some("CW"), Some("FSK"), Some("USB-D"))
    }

    //////////////////////////////////////////////////////////////
    // Tests for Bandlist/Cluster mode/frequency conversions (ICOM/generic map)
    // FT8 detection only overrides Digi and Rtty; other modes pass through.
    //////////////////////////////////////////////////////////////

    #[test]
    fn flrig_40m_ft8_digi_rtty_become_d_usb() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = icom_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m),
            Mode::D_USB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m),
            Mode::D_USB
        );
    }

    #[test]
    fn flrig_40m_ft8_other_modes_unaffected() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = icom_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Cw, &DEFAULT_FT8_FREQS, &m),
            Mode::CW
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m),
            Mode::LSB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::LSB, &DEFAULT_FT8_FREQS, &m),
            Mode::LSB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::USB, &DEFAULT_FT8_FREQS, &m),
            Mode::USB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Am, &DEFAULT_FT8_FREQS, &m),
            Mode::AM
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Fm, &DEFAULT_FT8_FREQS, &m),
            Mode::FM
        );
    }

    #[test]
    fn flrig_40m_cw() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Cw, &DEFAULT_FT8_FREQS, &m),
                Mode::CW
            );
        }
    }

    #[test]
    fn flrig_40m_phone() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m),
                Mode::LSB
            );
        }
    }

    #[test]
    fn flrig_40m_lsb() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::LSB, &DEFAULT_FT8_FREQS, &m),
                Mode::LSB
            );
        }
    }

    #[test]
    fn flrig_40m_usb() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::USB, &DEFAULT_FT8_FREQS, &m),
                Mode::USB
            );
        }
    }

    #[test]
    fn flrig_40m_digi_rtty() {
        let m = icom_mode_map();
        const BAND_40M: [f64; 5] = [
            7_000_000.0,
            7_030_000.0,
            7_100_000.0,
            7_185_000.0,
            7_200_000.0,
        ];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m),
                Mode::RTTY
            );
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m),
                Mode::RTTY
            );
        }
    }

    #[test]
    fn flrig_am_fm() {
        let m = icom_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(7_200_000.0, WavelogMode::Am, &DEFAULT_FT8_FREQS, &m),
            Mode::AM
        );
        assert_eq!(
            wavelog_to_flrig_mode(29_600_000.0, WavelogMode::Fm, &DEFAULT_FT8_FREQS, &m),
            Mode::FM
        );
    }

    #[test]
    fn flrig_20m_phone_becomes_usb() {
        let m = icom_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(14_225_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m),
            Mode::USB
        );
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
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m),
            Mode::DATA_U
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m),
            Mode::DATA_U
        );
    }

    #[test]
    fn yaesu_ft8_other_modes_unaffected() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = yaesu_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Cw, &DEFAULT_FT8_FREQS, &m),
            Mode::CW_U
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m),
            Mode::LSB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::LSB, &DEFAULT_FT8_FREQS, &m),
            Mode::LSB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::USB, &DEFAULT_FT8_FREQS, &m),
            Mode::USB
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Am, &DEFAULT_FT8_FREQS, &m),
            Mode::AM
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Fm, &DEFAULT_FT8_FREQS, &m),
            Mode::FM
        );
    }

    #[test]
    fn yaesu_40m_cw_becomes_cw_u() {
        let m = yaesu_mode_map();
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Cw, &DEFAULT_FT8_FREQS, &m),
                Mode::CW_U
            );
        }
    }

    #[test]
    fn yaesu_phone_lsb_usb_boundary() {
        let m = yaesu_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(7_150_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m),
            Mode::LSB
        );
        assert_eq!(
            wavelog_to_flrig_mode(14_225_000.0, WavelogMode::Phone, &DEFAULT_FT8_FREQS, &m),
            Mode::USB
        );
        assert_eq!(
            wavelog_to_flrig_mode(7_150_000.0, WavelogMode::LSB, &DEFAULT_FT8_FREQS, &m),
            Mode::LSB
        );
        assert_eq!(
            wavelog_to_flrig_mode(14_225_000.0, WavelogMode::USB, &DEFAULT_FT8_FREQS, &m),
            Mode::USB
        );
    }

    #[test]
    fn yaesu_40m_digi_rtty_become_rtty_u() {
        let m = yaesu_mode_map();
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m),
                Mode::RTTY_U
            );
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m),
                Mode::RTTY_U
            );
        }
    }

    #[test]
    fn yaesu_am_fm() {
        let m = yaesu_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(7_200_000.0, WavelogMode::Am, &DEFAULT_FT8_FREQS, &m),
            Mode::AM
        );
        assert_eq!(
            wavelog_to_flrig_mode(29_600_000.0, WavelogMode::Fm, &DEFAULT_FT8_FREQS, &m),
            Mode::FM
        );
    }

    //////////////////////////////////////////////////////////////
    // Tests for Kenwood mode map (CW→CW, RTTY→FSK, Digi/FT8→USB_D)
    //////////////////////////////////////////////////////////////

    #[test]
    fn kenwood_ft8_digi_rtty_become_usb_d() {
        const FT8_40M: f64 = 7_074_000.0;
        let m = kenwood_mode_map();
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m),
            Mode::USB_D
        );
        assert_eq!(
            wavelog_to_flrig_mode(FT8_40M, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m),
            Mode::USB_D
        );
    }

    #[test]
    fn kenwood_40m_digi_rtty_become_fsk() {
        let m = kenwood_mode_map();
        const BAND_40M: [f64; 3] = [7_000_000.0, 7_030_000.0, 7_200_000.0];
        for freq in BAND_40M {
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Digi, &DEFAULT_FT8_FREQS, &m),
                Mode::FSK
            );
            assert_eq!(
                wavelog_to_flrig_mode(freq, WavelogMode::Rtty, &DEFAULT_FT8_FREQS, &m),
                Mode::FSK
            );
        }
    }

    //////////////////////////////////////////////////////////////
    // Tests for parse_qsy_path input validation
    //////////////////////////////////////////////////////////////

    fn make_get(path: &str) -> Request<()> {
        Request::builder().uri(path).body(()).unwrap()
    }

    // --- Baseline: exact wire format Wavelog sends ---

    #[test]
    fn qsy_path_wavelog_wire_format_accepted() {
        // Wavelog constructs the QSY URL as <cat_url>/<freq>/<mode>.
        // This test documents and locks that format.  If parse_qsy_path ever
        // stops accepting it, something has broken the Wavelog integration.
        assert!(parse_qsy_path(&make_get("/14074000/usb")).is_ok());
        assert!(parse_qsy_path(&make_get("/7074000/digi")).is_ok());
        assert!(parse_qsy_path(&make_get("/3573000/cw")).is_ok());
    }

    // --- Malformed paths ---

    #[test]
    fn qsy_path_single_segment_rejected() {
        let req = Request::builder().uri("/14030000").body(()).unwrap();
        assert!(parse_qsy_path(&req).is_err());
    }

    #[test]
    fn qsy_path_empty_rejected() {
        let req = Request::builder().uri("/").body(()).unwrap();
        assert!(parse_qsy_path(&req).is_err());
    }

    #[test]
    fn qsy_path_three_segments_rejected() {
        assert!(parse_qsy_path(&make_get("/14030000/cw/extra")).is_err());
    }

    // --- parse_qsy_path: soft sanity range rejects clearly out-of-scope inputs ---

    #[test]
    fn qsy_rejects_zero_frequency() {
        assert!(
            parse_qsy_path(&make_get("/0/usb")).is_err(),
            "frequency 0 Hz must be rejected"
        );
    }

    #[test]
    fn qsy_rejects_broadcast_band_frequency() {
        // 909 kHz is an AM broadcast frequency, well below the 160m lower edge.
        assert!(
            parse_qsy_path(&make_get("/909000/usb")).is_err(),
            "broadcast-band frequency 909 kHz must be rejected"
        );
    }

    #[test]
    fn qsy_rejects_max_u32_frequency() {
        // 4,294,967,295 Hz (~4.3 GHz) is far above the 70cm upper edge.
        assert!(
            parse_qsy_path(&make_get("/4294967295/usb")).is_err(),
            "out-of-range frequency 4294967295 Hz must be rejected"
        );
    }

    // --- parse_qsy_path: valid wire-format inputs accepted ---
    //
    // Note: parse_qsy_path is now a pure parser plus a soft sanity range.
    // Between-band frequencies (e.g. 11 MHz) and band-edge sideband issues
    // are caught later by is_emission_in_band; see the emission tests below.

    #[test]
    fn qsy_accepts_valid_hf_frequencies() {
        let valid = [
            "/1840000/usb",  // 160m
            "/3573000/usb",  // 80m FT8
            "/7074000/usb",  // 40m FT8
            "/10136000/usb", // 30m FT8
            "/14074000/usb", // 20m FT8
            "/18100000/usb", // 17m FT8
            "/21074000/usb", // 15m FT8
            "/24915000/usb", // 12m FT8
            "/28074000/usb", // 10m FT8
            "/50313000/usb", // 6m FT8
        ];
        for path in valid {
            assert!(
                parse_qsy_path(&make_get(path)).is_ok(),
                "expected Ok for {path}"
            );
        }
    }

    //////////////////////////////////////////////////////////////
    // Tests for is_plausible_radio_frequency (sanity range)
    //////////////////////////////////////////////////////////////

    #[test]
    fn sanity_rejects_zero() {
        assert!(!is_plausible_radio_frequency(0));
    }

    #[test]
    fn sanity_rejects_just_below_160m() {
        // Foundation cannot access anything below the 160m band lower edge.
        assert!(!is_plausible_radio_frequency(1_809_999));
    }

    #[test]
    fn sanity_accepts_160m_lower_edge() {
        assert!(is_plausible_radio_frequency(1_810_000));
    }

    #[test]
    fn sanity_accepts_70cm_upper_edge() {
        assert!(is_plausible_radio_frequency(440_000_000));
    }

    #[test]
    fn sanity_rejects_above_70cm() {
        assert!(!is_plausible_radio_frequency(440_000_001));
        assert!(!is_plausible_radio_frequency(1_000_000_000));
        assert!(!is_plausible_radio_frequency(u32::MAX));
    }

    //////////////////////////////////////////////////////////////
    // Tests for mode_emission_offsets
    //////////////////////////////////////////////////////////////

    #[test]
    fn offsets_lsb_family_one_sided_below() {
        assert_eq!(mode_emission_offsets(Mode::LSB), (3_000, 0));
        assert_eq!(mode_emission_offsets(Mode::D_LSB), (3_000, 0));
        assert_eq!(mode_emission_offsets(Mode::DATA_L), (3_000, 0));
    }

    #[test]
    fn offsets_usb_family_one_sided_above() {
        assert_eq!(mode_emission_offsets(Mode::USB), (0, 3_000));
        assert_eq!(mode_emission_offsets(Mode::D_USB), (0, 3_000));
        assert_eq!(mode_emission_offsets(Mode::DATA_U), (0, 3_000));
        assert_eq!(mode_emission_offsets(Mode::USB_D), (0, 3_000));
        assert_eq!(mode_emission_offsets(Mode::DATA), (0, 3_000));
        assert_eq!(mode_emission_offsets(Mode::PSK), (0, 3_000));
    }

    #[test]
    fn offsets_am_symmetric_3k() {
        assert_eq!(mode_emission_offsets(Mode::AM), (3_000, 3_000));
        assert_eq!(mode_emission_offsets(Mode::AM_N), (3_000, 3_000));
    }

    #[test]
    fn offsets_fm_symmetric_6k() {
        assert_eq!(mode_emission_offsets(Mode::FM), (6_000, 6_000));
        assert_eq!(mode_emission_offsets(Mode::FM_N), (6_000, 6_000));
        assert_eq!(mode_emission_offsets(Mode::DATA_FM), (6_000, 6_000));
        assert_eq!(mode_emission_offsets(Mode::DATA_FMN), (6_000, 6_000));
    }

    #[test]
    fn offsets_rtty_symmetric_3k() {
        assert_eq!(mode_emission_offsets(Mode::RTTY), (3_000, 3_000));
        assert_eq!(mode_emission_offsets(Mode::RTTY_U), (3_000, 3_000));
        assert_eq!(mode_emission_offsets(Mode::RTTY_L), (3_000, 3_000));
        assert_eq!(mode_emission_offsets(Mode::RTTY_R), (3_000, 3_000));
        assert_eq!(mode_emission_offsets(Mode::FSK), (3_000, 3_000));
    }

    #[test]
    fn offsets_cw_family_one_kilohertz_buffer() {
        assert_eq!(mode_emission_offsets(Mode::CW), (1_000, 1_000));
        assert_eq!(mode_emission_offsets(Mode::CW_U), (1_000, 1_000));
        assert_eq!(mode_emission_offsets(Mode::CW_L), (1_000, 1_000));
        assert_eq!(mode_emission_offsets(Mode::CW_R), (1_000, 1_000));
    }

    //////////////////////////////////////////////////////////////
    // Tests for is_emission_in_band
    //////////////////////////////////////////////////////////////

    // LSB: emission below the dial; sensitive to band lower edge.

    #[test]
    fn emission_lsb_at_lower_edge_rejected() {
        for &(lo, _hi) in AMATEUR_BANDS_HZ {
            assert!(
                !is_emission_in_band(lo, Mode::LSB),
                "LSB at {lo} (band lower edge) should be rejected — emission falls below"
            );
        }
    }

    #[test]
    fn emission_lsb_at_lower_edge_plus_3khz_accepted() {
        for &(lo, _hi) in AMATEUR_BANDS_HZ {
            let dial = lo + 3_000;
            assert!(
                is_emission_in_band(dial, Mode::LSB),
                "LSB at {dial} (= lower edge + 3 kHz) should be accepted"
            );
        }
    }

    #[test]
    fn emission_lsb_at_upper_edge_accepted() {
        for &(_lo, hi) in AMATEUR_BANDS_HZ {
            assert!(
                is_emission_in_band(hi, Mode::LSB),
                "LSB at {hi} (band upper edge) should be accepted — emission stays below"
            );
        }
    }

    // USB: emission above the dial; sensitive to band upper edge.

    #[test]
    fn emission_usb_at_upper_edge_rejected() {
        for &(_lo, hi) in AMATEUR_BANDS_HZ {
            assert!(
                !is_emission_in_band(hi, Mode::USB),
                "USB at {hi} (band upper edge) should be rejected — emission falls above"
            );
        }
    }

    #[test]
    fn emission_usb_at_upper_edge_minus_3khz_accepted() {
        for &(_lo, hi) in AMATEUR_BANDS_HZ {
            let dial = hi - 3_000;
            assert!(
                is_emission_in_band(dial, Mode::USB),
                "USB at {dial} (= upper edge - 3 kHz) should be accepted"
            );
        }
    }

    #[test]
    fn emission_usb_at_lower_edge_accepted() {
        for &(lo, _hi) in AMATEUR_BANDS_HZ {
            assert!(
                is_emission_in_band(lo, Mode::USB),
                "USB at {lo} (band lower edge) should be accepted — emission stays above"
            );
        }
    }

    // AM and RTTY: two-sided 3 kHz; both edges rejected.

    #[test]
    fn emission_am_at_band_edges_rejected() {
        for &(lo, hi) in AMATEUR_BANDS_HZ {
            assert!(
                !is_emission_in_band(lo, Mode::AM),
                "AM at {lo} (band lower edge) should be rejected"
            );
            assert!(
                !is_emission_in_band(hi, Mode::AM),
                "AM at {hi} (band upper edge) should be rejected"
            );
        }
    }

    #[test]
    fn emission_rtty_at_band_edges_rejected() {
        for &(lo, hi) in AMATEUR_BANDS_HZ {
            assert!(
                !is_emission_in_band(lo, Mode::RTTY),
                "RTTY at {lo} (band lower edge) should be rejected"
            );
            assert!(
                !is_emission_in_band(hi, Mode::RTTY),
                "RTTY at {hi} (band upper edge) should be rejected"
            );
        }
    }

    // FM: two-sided 6 kHz.  10m FM upper edge is the canonical example.

    #[test]
    fn emission_fm_29700khz_rejected_29694khz_accepted() {
        assert!(!is_emission_in_band(29_700_000, Mode::FM));
        assert!(is_emission_in_band(29_694_000, Mode::FM));
    }

    // CW: 1 kHz buffer at each band edge.

    #[test]
    fn emission_cw_at_band_edges_rejected() {
        for &(lo, hi) in AMATEUR_BANDS_HZ {
            assert!(
                !is_emission_in_band(lo, Mode::CW),
                "CW at {lo} (band lower edge) should be rejected (1 kHz margin)"
            );
            assert!(
                !is_emission_in_band(hi, Mode::CW),
                "CW at {hi} (band upper edge) should be rejected (1 kHz margin)"
            );
        }
    }

    #[test]
    fn emission_cw_one_kilohertz_inside_band_accepted() {
        for &(lo, hi) in AMATEUR_BANDS_HZ {
            assert!(
                is_emission_in_band(lo + 1_000, Mode::CW),
                "CW at lower+1 kHz of {lo} should be accepted"
            );
            assert!(
                is_emission_in_band(hi - 1_000, Mode::CW),
                "CW at upper-1 kHz of {hi} should be accepted"
            );
        }
    }

    // FT8 dial frequencies are well inside their bands for every digital mode.

    #[test]
    fn emission_ft8_dials_accepted() {
        let dials = [
            1_840_000, 3_575_000, 7_074_000, 10_136_000, 14_074_000, 18_100_000, 21_074_000,
            24_915_000, 28_074_000, 50_313_000,
        ];
        for dial in dials {
            for mode in [Mode::D_USB, Mode::DATA_U, Mode::USB_D, Mode::DATA] {
                assert!(
                    is_emission_in_band(dial, mode),
                    "{mode:?} at {dial} (FT8 dial) should be accepted"
                );
            }
        }
    }

    // Inter-band frequencies — caught here now, not by parse_qsy_path.

    #[test]
    fn emission_between_bands_rejected() {
        // 11 MHz is between 30m (10.15 MHz) and 20m (14.0 MHz).
        for mode in [Mode::USB, Mode::LSB, Mode::CW, Mode::AM, Mode::FM] {
            assert!(
                !is_emission_in_band(11_000_000, mode),
                "{mode:?} at 11 MHz (between 30m and 20m) should be rejected"
            );
        }
    }

    //////////////////////////////////////////////////////////////
    // Tests for the forbidden range
    //////////////////////////////////////////////////////////////

    #[test]
    fn emission_inside_forbidden_range_rejected() {
        for freq in [431_000_000, 431_500_000, 432_000_000] {
            for mode in [Mode::USB, Mode::LSB, Mode::CW, Mode::FM] {
                assert!(
                    !is_emission_in_band(freq, mode),
                    "{mode:?} at {freq} (inside forbidden range) should be rejected"
                );
            }
        }
    }

    #[test]
    fn emission_sideband_touching_forbidden_range_rejected() {
        // USB at 430_998_000: emission upper = 431_001_000 → overlaps lower edge
        assert!(!is_emission_in_band(430_998_000, Mode::USB));
        // LSB at 432_001_000: emission lower = 431_998_000 → overlaps upper edge
        assert!(!is_emission_in_band(432_001_000, Mode::LSB));
        // CW at 430_999_999: emission upper = 431_000_999 → overlaps lower edge
        assert!(!is_emission_in_band(430_999_999, Mode::CW));
    }

    #[test]
    fn emission_clear_of_forbidden_range_accepted() {
        // USB at 430_995_000: emission 430_995_000-430_998_000 — clear below
        assert!(is_emission_in_band(430_995_000, Mode::USB));
        // LSB at 432_004_000: emission 432_001_000-432_004_000 — clear above
        assert!(is_emission_in_band(432_004_000, Mode::LSB));
    }

    //////////////////////////////////////////////////////////////
    // End-to-end path checks: parse + mode-map + emission
    //////////////////////////////////////////////////////////////

    // Compose the chain the real qsy() handler walks: parse the path,
    // resolve the FLRig mode, then check emission against the band plan.
    // Returns false if any step would refuse the request.
    fn would_emit_in_band(path: &str, mode_map: &ModeMap) -> bool {
        let req = make_get(path);
        let Ok(qsy) = parse_qsy_path(&req) else {
            return false;
        };
        let mode = wavelog_to_flrig_mode(qsy.freq, qsy.mode, &DEFAULT_FT8_FREQS, mode_map);
        is_emission_in_band(qsy.freq as u32, mode)
    }

    #[test]
    fn path_phone_at_80m_lower_edge_rejected() {
        // /3500000/phone resolves to LSB on 80m; emission below the lower edge.
        assert!(!would_emit_in_band("/3500000/phone", &icom_mode_map()));
    }

    #[test]
    fn path_lsb_at_80m_lower_edge_rejected() {
        assert!(!would_emit_in_band("/3500000/lsb", &icom_mode_map()));
    }

    #[test]
    fn path_usb_at_80m_lower_edge_accepted() {
        // USB at the lower edge: emission stays above the dial, safely in band.
        assert!(would_emit_in_band("/3500000/usb", &icom_mode_map()));
    }

    #[test]
    fn path_phone_at_20m_upper_edge_rejected() {
        // /14350000/phone resolves to USB on 20m; emission above the upper edge.
        assert!(!would_emit_in_band("/14350000/phone", &icom_mode_map()));
    }

    #[test]
    fn path_usb_at_20m_upper_edge_rejected() {
        assert!(!would_emit_in_band("/14350000/usb", &icom_mode_map()));
    }

    #[test]
    fn path_lsb_at_20m_upper_edge_accepted() {
        // Operationally unusual but technically safe.
        assert!(would_emit_in_band("/14350000/lsb", &icom_mode_map()));
    }

    #[test]
    fn path_ft8_dial_accepted() {
        assert!(would_emit_in_band("/7074000/digi", &icom_mode_map()));
    }

    #[test]
    fn path_fm_at_10m_upper_edge_rejected() {
        assert!(!would_emit_in_band("/29700000/fm", &icom_mode_map()));
    }

    #[test]
    fn path_cw_at_80m_lower_edge_rejected() {
        // CW has a 1 kHz margin from band edges.
        assert!(!would_emit_in_band("/3500000/cw", &icom_mode_map()));
    }

    #[test]
    fn path_cw_one_kilohertz_inside_80m_accepted() {
        assert!(would_emit_in_band("/3501000/cw", &icom_mode_map()));
    }

    #[test]
    fn path_inside_forbidden_range_rejected() {
        // 431.5 MHz is inside the unconditional forbidden range.
        assert!(!would_emit_in_band("/431500000/usb", &icom_mode_map()));
    }

    //////////////////////////////////////////////////////////////
    // Tests for check_origin
    //////////////////////////////////////////////////////////////

    #[test]
    fn origin_correct_accepted() {
        let req = Request::builder()
            .header("origin", "https://wavelog.example.org")
            .uri("/14074000/usb")
            .body(())
            .unwrap();
        assert!(check_origin(&req, "https://wavelog.example.org"));
    }

    #[test]
    fn origin_wrong_rejected() {
        let req = Request::builder()
            .header("origin", "https://evil.example.com")
            .uri("/14074000/usb")
            .body(())
            .unwrap();
        assert!(!check_origin(&req, "https://wavelog.example.org"));
    }

    #[test]
    fn origin_missing_rejected() {
        let req = Request::builder().uri("/14074000/usb").body(()).unwrap();
        assert!(!check_origin(&req, "https://wavelog.example.org"));
    }
}
