//! Optional loopback HTTP host for OpenPaw client adapters.

#[cfg(feature = "engine-host")]
pub mod config;
#[cfg(feature = "engine-host")]
pub mod http;

#[cfg(feature = "engine-host")]
pub use config::HostConfig;
