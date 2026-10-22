//! Code copied from swayward-config at the sha in `COPIED.md`. Edit only to keep it compiling;
//! behaviour changes go in `crate::config`.

#![allow(dead_code, unused_macros)]

#[macro_use]
pub mod macros;
pub mod color;
pub mod effects;
pub mod error;
pub mod merge_with;
pub mod utils;
