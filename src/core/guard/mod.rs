//! # Core Guard Subsystem — MELISA Security Layer
//!
//! Provides input validation and sanitisation for all user-supplied text
//! entering the MELISA command executor.  Every rule is explicit, independently
//! testable, and surfaces a machine-readable [`BlockReason`] on violation.

pub mod filter;

pub use filter::{filter_input, BlockReason, FilterResult};