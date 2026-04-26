//! Terminal colour constants and helpers.
//!
//! Mirrors the server-side `src/cli/color.rs` so that output formatting stays
//! consistent across both binaries.  The `colored` crate transparently enables
//! ANSI virtual-terminal processing on Windows 10+ via the Win32 API.

pub const GREEN:  &str = "\x1b[32m";
pub const RED:    &str = "\x1b[31m";
pub const CYAN:   &str = "\x1b[36m";
pub const BLUE:   &str = "\x1b[34;1m";
pub const YELLOW: &str = "\x1b[33m";
pub const BOLD:   &str = "\x1b[1m";
pub const RESET:  &str = "\x1b[0m";

pub fn log_info(msg: &str) {
    println!("{BOLD}{BLUE}[INFO]{RESET} {msg}");
}

pub fn log_success(msg: &str) {
    println!("{BOLD}{GREEN}[SUCCESS]{RESET} {msg}");
}

pub fn log_warning(msg: &str) {
    eprintln!("{BOLD}{YELLOW}[WARNING]{RESET} {msg}");
}

pub fn log_error(msg: &str) {
    eprintln!("{BOLD}{RED}[ERROR]{RESET} {msg}");
}

pub fn log_header(msg: &str) {
    println!("\n{BLUE}::{RESET} {BOLD}{msg}{RESET}");
}

pub fn log_stat(key: &str, val: &str) {
    println!(" {GREEN}=>{RESET} {key}: {BOLD}{val}{RESET}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_constants_are_valid_ansi_sequences() {
        for (name, code) in &[
            ("GREEN",  GREEN),
            ("RED",    RED),
            ("CYAN",   CYAN),
            ("BLUE",   BLUE),
            ("YELLOW", YELLOW),
            ("BOLD",   BOLD),
        ] {
            assert!(
                code.starts_with("\x1b["),
                "Color constant '{name}' must start with ESC sequence \\x1b["
            );
            assert!(
                code.ends_with('m'),
                "Color constant '{name}' must end with ANSI SGR terminator 'm'"
            );
        }
    }

    #[test]
    fn test_reset_constant_is_standard_ansi_reset() {
        assert_eq!(RESET, "\x1b[0m", "RESET must be the standard ANSI SGR reset sequence");
    }

    #[test]
    fn test_color_constants_are_distinct() {
        let codes = [GREEN, RED, CYAN, BLUE, YELLOW, BOLD];
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(
                    codes[i], codes[j],
                    "Color constants at index {i} and {j} must not be identical"
                );
            }
        }
    }
}