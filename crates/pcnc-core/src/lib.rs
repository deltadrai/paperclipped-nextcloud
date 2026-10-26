//! Core of paperclipped-nextcloud: the identity model, the backend traits and the idempotent
//! engine that converges a digital worker's mail and Nextcloud identity.

pub mod backend;
pub mod engine;
pub mod error;
pub mod model;
pub mod name;
pub mod secret;

pub use engine::{Engine, EngineConfig, SharedMailbox};
pub use error::EngineError;
pub use model::*;
