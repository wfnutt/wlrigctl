use crate::wavelog::RadioData;
use log::{info};
use std::result::Result;
use std::fmt;

use dxr_client::{Client, ClientError, ClientBuilder};
use url::Url;

pub struct FLRig {
    maxwatts: u32,
    client: Client,
    identifier: String,
    cwbandwidth: Option<u32>,
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

impl FLRig {
    pub fn new(url: Url, maxwatts: u32, identifier: String, cwbandwidth: Option<u32>) -> FLRig {
        let client: Client = ClientBuilder::new(url).build();
        FLRig {
            maxwatts,
            client,
            identifier,
            cwbandwidth,
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
            power: rig_power_watts(power, maxpwr, self.maxwatts),
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
    ) -> Result<(), ClientError> {

        // rather than glitch the radio, if the required mode is already in effect, leave it alone!
        // This matters because if we're already in a mode with a reduced bandwidth or filter,
        // the rig is nice and quiet. If we peturb the mode, flrig will set a wider bandwidth
        // on IC-703, then a split-second later we apply our cwbandwidth option to put the filter
        // back in place. This causes a noticeable audio disturbance which is distracting.
        //
        // Maybe we could lose the cwbandwidth feature entirely, and just use this hysteresis
        // to not mess with a mode that was already correct?
        let existing_mode = self.get_mode().await?;
        if mode.to_string() == existing_mode { // XXX: This shouldn't be a string compare...
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
