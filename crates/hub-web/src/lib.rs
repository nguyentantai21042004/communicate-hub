//! Dashboard JSON for the local monitoring pages.
//!
//! This crate owns the response types and the axum routes under `/api`. It does no I/O and
//! does not talk to Telegram or Zalo: the application implements [`HubApi`] and supplies the
//! data. Mount [`routes`] inside the existing server so its access checks apply unchanged.

mod api;
mod error;
pub mod limits;
mod mask;
mod model;
mod query;
mod routes;

pub use api::HubApi;
pub use error::{Error, Result};
pub use hub_core::{ModuleHealth, ModuleState};
pub use mask::mask_external_id;
pub use model::*;
pub use routes::routes;

#[cfg(test)]
mod tests;
