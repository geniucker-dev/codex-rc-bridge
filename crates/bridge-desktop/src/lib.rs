//! Desktop transport and lifecycle, sharing the same tenant coordinator as the server.
#[cfg(feature = "app")]
mod app;
#[cfg(feature = "app")]
mod credentials;
pub mod service;
#[cfg(feature = "app")]
pub use app::run;
