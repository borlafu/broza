//! Presentation layer: format resolution, color policy, byte formatting,
//! the output sink and the envelope renderer.
//!
//! Nothing here reads the environment: it all comes from [`crate::env::RuntimeEnv`].

pub mod bytes;
pub mod color;
pub mod format;
pub mod render;
pub mod sink;

pub use bytes::format_bytes;
pub use color::ColorPolicy;
pub use format::OutputFormat;
pub use render::{Renderer, envelope_to_json};
pub use sink::Sink;
