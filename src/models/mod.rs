//! Serde-deserializable data models for `runners.yaml`.
//!
//! Keeping the schema types in one place makes it easier to iterate on the
//! YAML surface — edit here, then the loader in [`crate::config`] picks up
//! any changes automatically.

pub mod fleet;
pub mod runner;

pub use fleet::{FleetConfig, Reserve};
pub use runner::{runner_names, Kind, RunnerGroup};
