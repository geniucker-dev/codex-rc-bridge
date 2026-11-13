//! Shared domain and protocol foundation. No transport starts implicitly.
pub mod auth;
pub mod chains;
pub mod configuration;
pub mod delivery;
pub mod deployment;
pub mod device;
pub mod enrollment;
pub mod identity;
pub mod input_pairing;
mod input_sync;
pub mod lifecycle;
pub mod management;
pub mod model;
pub mod operation_error;
pub mod output_pairing;
mod output_sync;
pub mod presence;
pub mod routing;
pub mod runtime;
pub mod storage;
pub mod transport;
pub mod upstream;
mod user_agent;
pub mod vault;
pub mod wire;

pub fn fill_random(bytes: &mut [u8]) -> Result<(), identity::IdentityError> {
    aws_lc_rs::rand::fill(bytes).map_err(|_| identity::IdentityError::Crypto)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub version: &'static str,
    pub remote_control: bool,
}
pub fn capabilities() -> Capabilities {
    Capabilities {
        version: env!("CARGO_PKG_VERSION"),
        remote_control: false,
    }
}

#[cfg(test)]
mod auth_regressions;
