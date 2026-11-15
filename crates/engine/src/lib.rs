//! The norn-ui engine (design §9.1, docs/ui-protocol.md in the Norn
//! repository): the retained node tree, styles and themes, layout,
//! hit-testing, engine-side states, damage tracking and counters. It knows
//! nothing of the Norn heap or of any platform; backends (headless, macOS)
//! drive it, and the extension (crates/norn-ui) connects it to Norn.

pub mod anim;
pub mod data;
pub mod engine;
pub mod editor;
pub mod field;
pub mod ime;
pub mod popup;
pub mod rich;
pub mod style;
pub mod text;

pub use data::{Data, Event, NodeId, Op, Props, Rect, WindowId};
pub use engine::{Engine, Measure, Node, RoughMeasure, Stats};
pub use text::{Text, TextStyle};
