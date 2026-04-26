// ============================================================================
// src/mshell/mod.rs
//
// Public re-exports for the mshell (interactive shell) module.
// ============================================================================

pub mod color;
pub mod helper;
pub mod wellcome;
pub mod auth;
pub mod db;
pub mod handler;

// Re-export the command handler
pub use handler::handle_command;