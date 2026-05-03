use crate::wavelog::RadioData;
use log::{debug, info, warn};
use serde_derive::Deserialize;
use std::fmt;
use std::result::Result;
use std::str::FromStr;

use dxr::TryFromValue;
use dxr_client::{Client, ClientBuilder, ClientError};
use url::Url;

// Settings from .toml file
#[derive(Debug, Deserialize)]
pub struct FlrigSettings {
    pub host: String,
    pub port: u16,
    pub maxpower: u32,
    /// Index into FLRig's bandwidth table to apply after every CW mode change.
    /// This is NOT a value in Hz.  See CLAUDE.md for the IC-703 FLRig bug that
    /// makes index 1 (labelled "MED") the correct choice for the narrow filter.
    pub cw_bw_index: Option<u32>,
}

// Internal state
#[allow(non_snake_case)]
pub struct FLRig {
    maxpower: u32, // Watts
    client: Client,
    identifier: String,
    cw_bw_index: Option<u32>,
}

#[derive(Debug)]
pub struct UnknownModeError {
    pub msg: String,
}

impl fmt::Display for UnknownModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UnknownModeError: {0}", self.msg)
    }
}

impl std::error::Error for UnknownModeError {}

#[derive(Debug)]
pub enum FlrigError {
    DxrClient(ClientError),
    UnknownMode(UnknownModeError),
}

impl fmt::Display for FlrigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FlrigError::DxrClient(err) => write!(f, "DxrClient error: {}", err),
            FlrigError::UnknownMode(err) => write!(f, "UnknownMode error: {}", err),
        }
    }
}

impl std::error::Error for FlrigError {}

impl From<ClientError> for FlrigError {
    fn from(error: ClientError) -> Self {
        FlrigError::DxrClient(error)
    }
}

#[allow(dead_code)]
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Mode {
    LSB,
    USB,
    AM,
    AM_N,
    CW, // or CW_U on Yaesu?
    CW_U,
    RTTY, // or RTTY_U on Yaesu?
    RTTY_U,
    FM,
    FM_N,
    CW_R, // or CW_L on Yaesu?
    CW_L,
    RTTY_R, // or RTTY_L on Yaesu?
    RTTY_L,
    D_LSB, // or DATA_L on Yaesu?
    DATA_L,
    D_USB, // or DATA_U on Yaesu?
    DATA_U,
    DATA_FM,
    DATA_FMN,
    PSK,
    FSK,   // Kenwood RTTY
    USB_D, // newer ICOM digital (e.g. IC-7300); cf. D-USB on IC-703
    DATA,  // Elecraft generic data
}

#[allow(dead_code)]
impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::LSB => write!(f, "LSB"),
            Mode::USB => write!(f, "USB"),
            Mode::AM => write!(f, "AM"),
            Mode::AM_N => write!(f, "AM-N"),
            Mode::CW => write!(f, "CW"),
            Mode::CW_U => write!(f, "CW-U"),
            Mode::RTTY => write!(f, "RTTY"),
            Mode::RTTY_U => write!(f, "RTTY-U"),
            Mode::FM => write!(f, "FM"),
            Mode::FM_N => write!(f, "FM-N"),
            Mode::CW_R => write!(f, "CW-R"),
            Mode::CW_L => write!(f, "CW-L"),
            Mode::RTTY_R => write!(f, "RTTY-R"),
            Mode::RTTY_L => write!(f, "RTTY-L"),
            Mode::D_LSB => write!(f, "D-LSB"),
            Mode::DATA_L => write!(f, "DATA-L"),
            Mode::D_USB => write!(f, "D-USB"),
            Mode::DATA_U => write!(f, "DATA-U"),
            Mode::DATA_FM => write!(f, "DATA-FM"),
            Mode::DATA_FMN => write!(f, "DATA-FMN"),
            Mode::PSK => write!(f, "PSK"),
            Mode::FSK => write!(f, "FSK"),
            Mode::USB_D => write!(f, "USB-D"),
            Mode::DATA => write!(f, "DATA"),
        }
    }
}

impl Mode {
    /// Map a rig-specific FLRig mode to the mode string Wavelog expects.
    ///
    /// Wavelog accepts ADIF-standard mode names (USB, LSB, CW, RTTY, FM, AM).
    /// FLRig mirrors whatever the rig's panel displays — e.g. an IC-703 uses
    /// "D-USB" for digital USB, a Yaesu FTDX10 uses "DATA-U". Both mean the
    /// operator is on a USB carrier in data mode, so both map to "USB" here.
    pub fn to_wavelog_mode(self) -> &'static str {
        match self {
            Mode::LSB | Mode::D_LSB | Mode::DATA_L => "LSB",
            Mode::USB | Mode::D_USB | Mode::DATA_U => "USB",
            Mode::AM | Mode::AM_N => "AM",
            Mode::CW | Mode::CW_U | Mode::CW_R | Mode::CW_L => "CW",
            Mode::RTTY | Mode::RTTY_U | Mode::RTTY_R | Mode::RTTY_L => "RTTY",
            Mode::FM | Mode::FM_N | Mode::DATA_FM | Mode::DATA_FMN => "FM",
            Mode::PSK | Mode::USB_D | Mode::DATA => "USB",
            Mode::FSK => "RTTY",
        }
    }
}

impl FromStr for Mode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "LSB" => Ok(Mode::LSB),
            "USB" => Ok(Mode::USB),
            "AM" => Ok(Mode::AM),
            "AM-N" => Ok(Mode::AM_N),
            "CW" => Ok(Mode::CW),
            "CW-U" => Ok(Mode::CW_U),
            "RTTY" => Ok(Mode::RTTY),
            "RTTY-U" => Ok(Mode::RTTY_U),
            "FM" => Ok(Mode::FM),
            "FM-N" => Ok(Mode::FM_N),
            "CW-R" => Ok(Mode::CW_R),
            "CW-L" => Ok(Mode::CW_L),
            "RTTY-R" => Ok(Mode::RTTY_R),
            "RTTY-L" => Ok(Mode::RTTY_L),
            "D-LSB" => Ok(Mode::D_LSB),
            "DATA-L" => Ok(Mode::DATA_L),
            "D-USB" => Ok(Mode::D_USB),
            "DATA-U" => Ok(Mode::DATA_U),
            "DATA-FM" => Ok(Mode::DATA_FM),
            "DATA-FMN" => Ok(Mode::DATA_FMN),
            "PSK" => Ok(Mode::PSK),
            "FSK" => Ok(Mode::FSK),
            "USB-D" => Ok(Mode::USB_D),
            "DATA" => Ok(Mode::DATA),
            _ => Err(()),
        }
    }
}

/// The FLRig mode string to send for each logical concept when responding to a
/// Wavelog CAT QSY request.  Built once at CAT_thread startup from the [CAT]
/// config section.
#[derive(Debug, Clone)]
pub struct ModeMap {
    pub cw: Mode,
    pub rtty: Mode,
    pub digital: Mode,
}

/// Build a [`ModeMap`] from the optional mode strings supplied in the [CAT]
/// config section.  Each field defaults to the ICOM/generic name if absent or
/// unrecognised:
///
/// | concept | default |
/// |---------|---------|
/// | cw      | `CW`    |
/// | rtty    | `RTTY`  |
/// | digital | `D-USB` |
pub fn build_mode_map(cw: Option<&str>, rtty: Option<&str>, digital: Option<&str>) -> ModeMap {
    fn resolve(s: Option<&str>, default: Mode, field: &str) -> Mode {
        match s {
            None => default,
            Some(name) => match name.parse::<Mode>() {
                Ok(m) => m,
                Err(_) => {
                    warn!("Unrecognised mode '{name}' for {field}; using default '{default}'");
                    default
                }
            },
        }
    }
    let map = ModeMap {
        cw: resolve(cw, Mode::CW, "cw_mode"),
        rtty: resolve(rtty, Mode::RTTY, "rtty_mode"),
        digital: resolve(digital, Mode::D_USB, "digital_mode"),
    };
    info!(
        "Mode map: CW='{}' RTTY='{}' Digital='{}'",
        map.cw, map.rtty, map.digital
    );
    map
}

// Returns the bandwidth index to pass to set_narrow when entering CW mode,
// or None when no filter adjustment is needed.  Extracted as a pure function
// so the logic can be tested without touching the async XMLRPC path.
fn cw_narrow_index(mode: Mode, cw_bw_index: Option<u32>) -> Option<u32> {
    cw_bw_index.filter(|_| mode == Mode::CW)
}

impl FLRig {
    pub fn new(settings: FlrigSettings, identifier: String) -> FLRig {
        let url = format!("{0}:{1}/", settings.host, settings.port);
        let url = Url::parse(&url).unwrap_or_else(|_| panic!("{url} does not parse as a URL"));
        let client: Client = ClientBuilder::new(url).build();
        FLRig {
            maxpower: settings.maxpower,
            client,
            identifier,
            cw_bw_index: settings.cw_bw_index,
        }
    }

    pub async fn get_mode(&self) -> Result<String, ClientError> {
        let response: String = self.client.call("rig.get_mode", ()).await?;
        Ok(response)
    }

    pub async fn get_update(&self) -> Result<String, ClientError> {
        let response: String = self.client.call("rig.get_update", ()).await?;
        Ok(response)
    }

    /// Fetch current radio state. Returns `None` when FLRig reports nothing has changed
    /// since the last poll (fast path), saving the multicall round-trip.
    pub async fn get_radio_data(&self) -> Result<Option<RadioData>, ClientError> {
        // Fast path: FLRig returns "NIL" when nothing has changed since the last call.
        // Note: FLRig always includes vol/mic/rfg in the response; "NIL" is only returned
        // when those controls are unsupported by the connected rig and nothing else changed.
        if self.get_update().await? == "NIL" {
            return Ok(None);
        }

        // Fetch vfo, mode, maxpwr and power in a single XMLRPC round-trip.
        let calls: Vec<(String, ())> = vec![
            ("rig.get_vfo".to_string(), ()),
            ("rig.get_mode".to_string(), ()),
            ("rig.get_maxpwr".to_string(), ()),
            ("rig.get_power".to_string(), ()),
        ];
        let mut results = self.client.multicall(calls).await?;
        // Pop in reverse call order; the Vec always has exactly as many entries as calls sent.
        let power_r = results.pop().expect("multicall result count mismatch");
        let maxpwr_r = results.pop().expect("multicall result count mismatch");
        let mode_r = results.pop().expect("multicall result count mismatch");
        let vfo_r = results.pop().expect("multicall result count mismatch");

        let vfo = String::try_from_value(&vfo_r.map_err(ClientError::from)?)?;
        let mode_raw = String::try_from_value(&mode_r.map_err(ClientError::from)?)?;
        let maxpwr = i32::try_from_value(&maxpwr_r.map_err(ClientError::from)?)?;
        let power = i32::try_from_value(&power_r.map_err(ClientError::from)?)?;

        let maxpwr_u = if maxpwr < 0 { 0u32 } else { maxpwr as u32 };
        let power_u = if power < 0 { 0u32 } else { power as u32 };

        // Translate the rig-specific FLRig mode string to one Wavelog understands.
        // If the string isn't in our Mode enum (e.g. a new rig adds an unknown mode),
        // pass it through unchanged rather than dropping or erroring.
        let mode = match mode_raw.parse::<Mode>() {
            Ok(m) => m.to_wavelog_mode().to_string(),
            Err(_) => {
                debug!("Unknown FLRig mode '{mode_raw}', forwarding as-is");
                mode_raw
            }
        };

        debug!("freq:{vfo} mode:{mode} power:{power} max:{maxpwr}");

        Ok(Some(RadioData {
            key: String::new(),
            radio: String::new(),
            frequency: vfo,
            mode,
            power: rig_power_watts(power_u, maxpwr_u, self.maxpower),
            cat_url: None,
        }))
    }

    pub async fn set_vfo(&self, freq_hz: f64) -> Result<(), ClientError> {
        let _response: String = self.client.call("rig.set_vfo", freq_hz).await?;

        Ok(())
    }

    pub async fn set_mode(&self, mode: Mode) -> Result<(), FlrigError> {
        // Avoid re-sending the mode command if the rig is already in the target mode.
        // rig.set_mode causes FLRig to momentarily apply a wide default bandwidth before the
        // mode settles; skipping the redundant call prevents an audible filter glitch.
        // Note: this guard only wraps rig.set_mode, not set_narrow — see below.
        let existing_mode_str: String = self.get_mode().await?;

        // Since we're converting the mode returned from FLRig's get_mode(), we have to handle the
        // prospect that a new mode is returned that is unknown to flrig::Mode
        let existing_mode: Mode = existing_mode_str.parse::<Mode>().map_err(|_| {
            FlrigError::UnknownMode(UnknownModeError {
                msg: format!("mode {existing_mode_str} is unknown"),
            })
        })?;

        if mode != existing_mode {
            info!("calling rig.set_mode with mode:{mode}");
            let _response: i32 = self.client.call("rig.set_mode", mode.to_string()).await?;
        }

        // Always restore narrow filter when targeting CW. Band memory may have
        // already switched the rig to CW (bypassing the change path above), so
        // set_narrow must not be inside the mode-change branch.
        // Always restore narrow filter when targeting CW. Band memory may have
        // already switched the rig to CW (bypassing the mode-change branch above),
        // so this must not be gated on whether the mode actually changed.
        if let Some(idx) = cw_narrow_index(mode, self.cw_bw_index) {
            info!("Bodging narrow filter on IC-703");
            self.set_narrow(idx as i32).await?;
        }

        Ok(())
    }

    pub async fn set_narrow(&self, bw_index: i32) -> Result<(), ClientError> {
        let _response: i32 = self.client.call("rig.set_bw", bw_index).await?;

        Ok(())
    }

    // Read back the string identifier, supplied in the .toml config file
    pub fn get_identifier(&self) -> String {
        self.identifier.clone()
    }
}

fn rig_power_watts(power: u32, max_power: u32, max_watts: u32) -> String {
    if max_power == 0 {
        return "0".to_string();
    }
    let watts = (power as f32 * max_watts as f32 / max_power as f32).round() as u32;
    watts.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> FlrigSettings {
        FlrigSettings {
            host: "http://127.0.0.1".to_string(),
            port: 19999,
            maxpower: 100,
            cw_bw_index: None,
        }
    }

    #[test]
    fn flrig_new_stores_identifier() {
        let rig = FLRig::new(test_settings(), "IC-703".to_string());
        assert_eq!(rig.get_identifier(), "IC-703");
    }

    #[tokio::test]
    async fn flrig_bad_url_returns_error() {
        // Port 19999 has nothing listening; the connection should be refused.
        let rig = FLRig::new(test_settings(), "IC-703".to_string());
        assert!(rig.get_mode().await.is_err());
    }

    // Tests for cw_narrow_index.
    // Three cases characterise the function completely.
    // The third case (non-CW mode with cw_bw_index set) is the one the old
    // code got wrong: set_narrow was inside the mode-change branch so it would
    // never fire when the rig was already in CW (band memory QSY path).
    //
    // Index 1 is used throughout because it is the correct IC-703 value: FLRig
    // labels it "MED" but it is the entry that activates the hardware narrow
    // filter (N indicator on).  See CLAUDE.md for the FLRig bug explanation.

    #[test]
    fn cw_narrow_index_returns_index_for_cw_with_index_configured() {
        assert_eq!(cw_narrow_index(Mode::CW, Some(1)), Some(1));
    }

    #[test]
    fn cw_narrow_index_returns_none_for_cw_without_index_configured() {
        assert_eq!(cw_narrow_index(Mode::CW, None), None);
    }

    #[test]
    fn cw_narrow_index_returns_none_for_non_cw_modes() {
        for mode in [Mode::USB, Mode::LSB, Mode::RTTY, Mode::FM, Mode::AM, Mode::D_USB] {
            assert_eq!(
                cw_narrow_index(mode, Some(1)),
                None,
                "cw_narrow_index returned Some for {mode:?} — only CW should yield an index"
            );
        }
    }

    #[test]
    fn mode_map_defaults_to_icom() {
        let m = build_mode_map(None, None, None);
        assert_eq!(m.cw, Mode::CW);
        assert_eq!(m.rtty, Mode::RTTY);
        assert_eq!(m.digital, Mode::D_USB);
    }

    #[test]
    fn mode_map_yaesu_config() {
        let m = build_mode_map(Some("CW-U"), Some("RTTY-U"), Some("DATA-U"));
        assert_eq!(m.cw, Mode::CW_U);
        assert_eq!(m.rtty, Mode::RTTY_U);
        assert_eq!(m.digital, Mode::DATA_U);
    }

    #[test]
    fn mode_map_kenwood_config() {
        let m = build_mode_map(Some("CW"), Some("FSK"), Some("USB-D"));
        assert_eq!(m.cw, Mode::CW);
        assert_eq!(m.rtty, Mode::FSK);
        assert_eq!(m.digital, Mode::USB_D);
    }

    #[test]
    fn mode_map_elecraft_config() {
        let m = build_mode_map(Some("CW"), Some("RTTY"), Some("DATA"));
        assert_eq!(m.digital, Mode::DATA);
    }

    #[test]
    fn mode_map_unknown_mode_falls_back_to_default() {
        let m = build_mode_map(Some("CW-UNKNOWN"), Some("FSK"), Some("D-USB"));
        assert_eq!(m.cw, Mode::CW); // fell back to default
    }

    #[test]
    fn wavelog_mode_standard_passthrough() {
        // Standard modes should come through unchanged.
        assert_eq!(Mode::LSB.to_wavelog_mode(), "LSB");
        assert_eq!(Mode::USB.to_wavelog_mode(), "USB");
        assert_eq!(Mode::CW.to_wavelog_mode(), "CW");
        assert_eq!(Mode::RTTY.to_wavelog_mode(), "RTTY");
        assert_eq!(Mode::FM.to_wavelog_mode(), "FM");
        assert_eq!(Mode::AM.to_wavelog_mode(), "AM");
    }

    #[test]
    fn wavelog_mode_icom_digital() {
        // IC-703 D-USB / D-LSB → standard carrier modes.
        assert_eq!(Mode::D_USB.to_wavelog_mode(), "USB");
        assert_eq!(Mode::D_LSB.to_wavelog_mode(), "LSB");
    }

    #[test]
    fn wavelog_mode_yaesu_variants() {
        // Yaesu CW/RTTY/DATA variants → standard equivalents.
        assert_eq!(Mode::CW_U.to_wavelog_mode(), "CW");
        assert_eq!(Mode::CW_L.to_wavelog_mode(), "CW");
        assert_eq!(Mode::CW_R.to_wavelog_mode(), "CW");
        assert_eq!(Mode::RTTY_U.to_wavelog_mode(), "RTTY");
        assert_eq!(Mode::RTTY_L.to_wavelog_mode(), "RTTY");
        assert_eq!(Mode::RTTY_R.to_wavelog_mode(), "RTTY");
        assert_eq!(Mode::DATA_U.to_wavelog_mode(), "USB");
        assert_eq!(Mode::DATA_L.to_wavelog_mode(), "LSB");
    }

    #[test]
    fn wavelog_mode_fm_variants() {
        assert_eq!(Mode::FM_N.to_wavelog_mode(), "FM");
        assert_eq!(Mode::DATA_FM.to_wavelog_mode(), "FM");
        assert_eq!(Mode::DATA_FMN.to_wavelog_mode(), "FM");
    }

    #[test]
    fn wavelog_mode_am_narrow() {
        assert_eq!(Mode::AM_N.to_wavelog_mode(), "AM");
    }

    #[test]
    fn wavelog_mode_new_variants() {
        assert_eq!(Mode::FSK.to_wavelog_mode(), "RTTY"); // Kenwood FSK = RTTY
        assert_eq!(Mode::USB_D.to_wavelog_mode(), "USB"); // newer ICOM digital
        assert_eq!(Mode::DATA.to_wavelog_mode(), "USB"); // Elecraft data
    }

    #[test]
    fn rig_power_zero_max_returns_zero() {
        // Must return "0" rather than panicking with divide-by-zero.
        assert_eq!(rig_power_watts(50, 0, 100), "0");
    }

    #[test]
    fn rig_power_full_scale() {
        let watts: f32 = rig_power_watts(100, 100, 100).parse().unwrap();
        assert_eq!(watts, 100.0);
    }

    #[test]
    fn rig_power_half_scale() {
        let watts: f32 = rig_power_watts(50, 100, 100).parse().unwrap();
        assert_eq!(watts, 50.0);
    }

    #[test]
    fn rig_power_zero_power_returns_zero() {
        assert_eq!(rig_power_watts(0, 100, 100), "0");
    }

    #[test]
    fn rig_power_result_is_always_integer() {
        // Wavelog requires integer Watts; the string must never contain a decimal point.
        let cases = [(1, 3, 100), (2, 3, 100), (1, 7, 100), (33, 64, 100)];
        for (power, max_power, max_watts) in cases {
            let result = rig_power_watts(power, max_power, max_watts);
            assert!(
                !result.contains('.'),
                "expected integer string, got '{result}' for ({power}, {max_power}, {max_watts})"
            );
        }
    }

    #[test]
    fn rig_power_fractional_watts_are_rounded() {
        // 1/3 * 100 = 33.333... → 33
        assert_eq!(rig_power_watts(1, 3, 100), "33");
        // 2/3 * 100 = 66.666... → 67
        assert_eq!(rig_power_watts(2, 3, 100), "67");
    }

    #[test]
    fn rig_power_half_watt_rounds_up() {
        // f32::round() is half-away-from-zero: 0.5 → 1
        assert_eq!(rig_power_watts(1, 2, 1), "1");
    }

    #[test]
    fn rig_power_scaled_max_watts() {
        // 200 W rig at 50% → 100 W
        assert_eq!(rig_power_watts(50, 100, 200), "100");
        // 1500 W rig at 75% → 1125 W
        assert_eq!(rig_power_watts(75, 100, 1500), "1125");
    }

    #[test]
    fn rig_power_max_power_not_100() {
        // FLRig reports maxpwr=80 (some rigs use a different scale).
        // 50 * 100 / 80 = 62.5 → rounds to 63 W.
        assert_eq!(rig_power_watts(50, 80, 100), "63");
    }

    #[test]
    fn rig_power_full_power_equals_max_watts() {
        // When power == max_power the result must equal max_watts, regardless of scale.
        assert_eq!(rig_power_watts(100, 100, 100), "100");
        assert_eq!(rig_power_watts(80, 80, 100), "100");
        assert_eq!(rig_power_watts(255, 255, 50), "50");
    }
}
