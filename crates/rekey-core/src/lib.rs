//! Rekey's platform-independent brain.
//!
//! Everything here is pure computation: no keyboard hooks, no OS calls, no
//! network. That keeps the interesting logic testable and lets the same crate
//! back the macOS and Windows apps.

pub mod buffer;
pub mod config;
pub mod detect;
pub mod engine;
pub mod input;
pub mod layout;
pub mod model;

pub use buffer::{Buffer, Input, LastCorrection, Word};
pub use config::{Config, Sensitivity};
pub use detect::{Candidate, Detector, Skip, Verdict};
pub use engine::{Action, Engine, Stats};
pub use input::{Context, Key, KeyEvent, Modifiers, TextWriter};
pub use layout::{convert, convert_by_id, layout, layout_ids, Layout, LayoutDef, Script};
pub use model::{Model, Models};
