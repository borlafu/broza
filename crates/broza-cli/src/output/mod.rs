//! Presentation layer: format resolution, color policy, byte formatting,
//! the output sink and the envelope renderer.
//!
//! Nothing here reads the environment: it all comes from [`crate::env::RuntimeEnv`].

pub mod bar;
pub mod bytes;
pub mod color;
pub mod csv;
pub mod format;
pub mod human;
pub mod render;
pub mod sink;
pub mod style;
pub mod wrap;

pub use bar::{usage_bar, usage_percentage};
pub use bytes::format_bytes;
pub use color::ColorPolicy;
pub use format::OutputFormat;
pub use render::{Renderer, envelope_to_json};
pub use sink::Sink;
pub use style::{Style, paint};
pub use wrap::wrap;
