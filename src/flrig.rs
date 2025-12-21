use crate::wavelog::RadioData;
use log::{info};
use std::fmt;
use std::result::Result;
use std::str::FromStr;
use serde_derive::Deserialize;

use dxr_client::{Client, ClientError, ClientBuilder};
use url::Url;

// Settings from .toml file
#[derive(Debug, Deserialize)]
pub struct FlrigSettings {
    pub host:        String,
    pub port:        u16,
    pub maxpower:    u32,
    pub cwbandwidth: Option<u32>,
}

// Internal state
#[allow(non_snake_case)]
pub struct FLRig {
    maxpower:    u32, // Watts
    client:      Client,
    identifier:  String,
    cwbandwidth: Option<u32>,
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
            FlrigError::DxrClient(err)   => write!(f, "DxrClient error: {}", err),
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
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Mode {
    LSB,
    USB,
    AM,
    CW,
    RTTY,
    FM,
    CW_R,
    RTTY_R,
    D_LSB,
    D_USB,
}

#[allow(dead_code)]
impl fmt::Display for Mode {

    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::LSB    => write!(f, "LSB"),
            Mode::USB    => write!(f, "USB"),
            Mode::AM     => write!(f, "AM"),
            Mode::CW     => write!(f, "CW"),
            Mode::RTTY   => write!(f, "RTTY"),
            Mode::FM     => write!(f, "FM"),
            Mode::CW_R   => write!(f, "CW-R"),
            Mode::RTTY_R => write!(f, "RTTY-R"),
            Mode::D_LSB  => write!(f, "D-LSB"),
            Mode::D_USB  => write!(f, "D-USB"),
        }
    }
}

impl FromStr for Mode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "LSB"   => Ok(Mode::LSB),
            "USB"   => Ok(Mode::USB),
            "AM"    => Ok(Mode::AM),
            "CW"    => Ok(Mode::CW),
            "RTTY"  => Ok(Mode::RTTY),
            "FM"    => Ok(Mode::FM),
            "CW-R"    => Ok(Mode::CW_R),
            "RTTY-R"  => Ok(Mode::RTTY_R),
            "D-LSB"   => Ok(Mode::D_LSB),
            "D-USB"   => Ok(Mode::D_USB),
            _       => Err(()),
        }
    }
}

impl FLRig {
    pub fn new(settings: FlrigSettings, identifier: String) -> FLRig {

        let url = format!("{0}:{1}/", settings.host, settings.port);
        let url = Url::parse(&url).expect("\"{url}\" does not parse as a url.");
        let client: Client = ClientBuilder::new(url).build();
        FLRig {
            maxpower: settings.maxpower,
            client,
            identifier,
            cwbandwidth: settings.cwbandwidth,
        }
    }

    pub async fn get_vfo(&self,
    ) -> Result<String, ClientError> {

        let response: String = self.client.call("rig.get_vfo", ()).await?;
        Ok(response)
    }

    pub async fn get_mode(&self,
    ) -> Result<String, ClientError> {

        let response: String = self.client.call("rig.get_mode", ()).await?;
        Ok(response)
    }

    pub async fn get_maxpwr(&self,
    ) -> Result<i32, ClientError> {

        let response: i32 = self.client.call("rig.get_maxpwr", ()).await?;
        Ok(response)
    }

    pub async fn get_power(&self,
    ) -> Result<i32, ClientError> {

        let response: i32 = self.client.call("rig.get_power", ()).await?;
        Ok(response)
    }

    pub async fn get_radio_data(&self,
    ) -> Result<RadioData, ClientError> {

        let vfo = self.get_vfo().await?;
        let mode = self.get_mode().await?;
        let maxpwr: u32 = match self.get_maxpwr().await? {
            val if val < 0 => 0,
            val => val as u32,
        };
        let power: u32 = match self.get_power().await? {
            val if val < 0 => 0,
            val => val as u32,
        };

        info!("freq:{vfo} mode:{mode} power:{power} max:{maxpwr}");

        let radio_data = RadioData {
            key: String::new(),
            radio: String::new(),
            frequency: vfo,
            mode,
            power: rig_power_watts(power, maxpwr, self.maxpower),
        };

        Ok(radio_data)
    }

    pub async fn set_vfo(
        &self,
        freq_hz: f64
    ) -> Result<(), ClientError> {

        let _response: String = self.client.call("rig.set_vfo", freq_hz).await?;

        Ok(())
    }

    pub async fn set_mode(
        &self,
        mode: Mode
    ) -> Result<(), FlrigError> {

        // rather than glitch the radio, if the required mode is already in effect, leave it alone!
        // This matters because if we're already in a mode with a reduced bandwidth or filter,
        // the rig is nice and quiet. If we perturb the mode, flrig will set a wider bandwidth
        // on IC-703, then a split-second later we apply our cwbandwidth option to put the filter
        // back in place. This causes a noticeable audio disturbance which is distracting.
        //
        // Maybe we could lose the cwbandwidth feature entirely, and just use this hysteresis
        // to not mess with a mode that was already correct?
        let existing_mode_str: String = self.get_mode().await?;

        // Since we're converting the mode returned from FLRig's get_mode(), we have to handle the
        // prospect that a new mode is returned that is unknown to flrig::Mode
        let existing_mode: Mode = existing_mode_str.parse::<Mode>()
            .map_err(|_| FlrigError::UnknownMode(UnknownModeError {
                msg: "mode {existing_mode_str} is unknown".to_string()
            }))?;

        if mode == existing_mode {
            // we're done
            return Ok(())
        }

        let _response: i32 = self.client.call("rig.set_mode", mode.to_string()).await?;
        if let Some(cwbandwidth) = self.cwbandwidth {
            if mode == Mode::CW {
                info!("Bodging narrow filter on IC-703");
                self.set_narrow(cwbandwidth as i32).await?;
            }
        }

        Ok(())
    }

    pub async fn set_narrow(
        &self,
        cwbandwidth: i32
    ) -> Result<(), ClientError> {

        let _response: i32 = self.client.call("rig.set_bw", cwbandwidth).await?;

        Ok(())
    }

    // Read back the string identifier, supplied in the .toml config file
    pub fn get_identifier(
        &self,
    ) -> String {

        self.identifier.clone()
    }
}

fn rig_power_watts(power: u32, max_power: u32, max_watts: u32) -> String {
    let watts: f32 = power as f32 * max_watts as f32 / max_power as f32;

    watts.to_string()
}
