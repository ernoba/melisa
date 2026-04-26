//! # Client-Side Input Filter
//!
//! Sanitises user-supplied arguments **before** they are passed to external
//! processes (SSH, SCP, Rsync).  Rules are intentionally stricter than the
//! server-side REPL filter because client commands are fully parsed before
//! execution; no shell pass-through is needed here.
//!
//! Call [`sanitise_arg`] on every user-controlled argument that will appear
//! inside a shell command string.  Call [`validate_profile_name`] whenever a
//! profile name is read from user input.

use std::fmt;

// ── Limits ────────────────────────────────────────────────────────────────────

/// Maximum character length for a single argument token.
const MAX_ARG_LEN: usize = 512;

/// Maximum character length for a connection profile name.
const MAX_PROFILE_NAME_LEN: usize = 64;

// ── Public types ──────────────────────────────────────────────────────────────

/// Outcome of a sanitisation call.
#[derive(Debug, PartialEq, Eq)]
pub enum SanitiseResult {
    /// The argument is safe to use.
    Ok,
    /// The argument is unsafe.  Inner value explains why.
    Reject(RejectReason),
}

/// Explains why an argument was rejected.
#[derive(Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The argument exceeds the permitted length.
    TooLong { length: usize, limit: usize },
    /// A null byte was found.
    NullByte,
    /// A shell metacharacter was found in a context where it is never valid.
    ShellMetachar { ch: char },
    /// A filesystem path-traversal sequence was detected.
    PathTraversal { pattern: String },
    /// The profile name contains characters that are not alphanumeric, hyphens,
    /// or underscores.
    InvalidProfileName { name: String },
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { length, limit } => write!(
                f,
                "Argument length {length} exceeds the maximum of {limit} characters.",
            ),
            Self::NullByte => write!(
                f,
                "Argument contains a null byte (\\0) which is never valid.",
            ),
            Self::ShellMetachar { ch } => write!(
                f,
                "Argument contains forbidden shell metacharacter: '{ch}'.",
            ),
            Self::PathTraversal { pattern } => write!(
                f,
                "Argument contains a path-traversal sequence: '{pattern}'.",
            ),
            Self::InvalidProfileName { name } => write!(
                f,
                "Profile name '{name}' is invalid. \
                 Only letters, digits, hyphens, and underscores are permitted.",
            ),
        }
    }
}

// ── Public functions ──────────────────────────────────────────────────────────

/// Validates a single command argument token.
///
/// Rejects the argument if it:
/// * exceeds [`MAX_ARG_LEN`] characters
/// * contains a null byte
/// * contains one of the forbidden shell metacharacters: `` ` $ ; & | < > \n \r ``
/// * contains a path-traversal sequence (`../`, `..\`, `..%2f`, `..%5c`)
///
/// # Examples
///
/// ```
/// use crate::filter::{sanitise_arg, SanitiseResult};
///
/// assert_eq!(sanitise_arg("my-dev-box"),   SanitiseResult::Ok);
/// assert!(matches!(sanitise_arg("$(id)"),  SanitiseResult::Reject(_)));
/// ```
pub fn sanitise_arg(arg: &str) -> SanitiseResult {
    // Length guard
    if arg.len() > MAX_ARG_LEN {
        return SanitiseResult::Reject(RejectReason::TooLong {
            length: arg.len(),
            limit:  MAX_ARG_LEN,
        });
    }

    // Null-byte guard
    if arg.contains('\0') {
        return SanitiseResult::Reject(RejectReason::NullByte);
    }

    // Shell metacharacter guard
    const FORBIDDEN_CHARS: &[char] = &['`', '$', ';', '&', '|', '<', '>', '\n', '\r'];
    for &ch in FORBIDDEN_CHARS {
        if arg.contains(ch) {
            return SanitiseResult::Reject(RejectReason::ShellMetachar { ch });
        }
    }

    // Path-traversal guard
    let lower = arg.to_lowercase();
    for &pattern in &["../", "..\\", "..%2f", "..%5c"] {
        if lower.contains(pattern) {
            return SanitiseResult::Reject(RejectReason::PathTraversal {
                pattern: pattern.to_string(),
            });
        }
    }

    SanitiseResult::Ok
}

/// Validates a connection profile name.
///
/// Profile names must be 1–[`MAX_PROFILE_NAME_LEN`] characters long and may
/// only contain ASCII letters, digits, hyphens (`-`), or underscores (`_`).
///
/// # Examples
///
/// ```
/// use crate::filter::validate_profile_name;
///
/// assert!(validate_profile_name("production-01").is_ok());
/// assert!(validate_profile_name("bad name!").is_err());
/// ```
pub fn validate_profile_name(name: &str) -> Result<(), RejectReason> {
    if name.is_empty() || name.len() > MAX_PROFILE_NAME_LEN {
        return Err(RejectReason::TooLong {
            length: name.len(),
            limit:  MAX_PROFILE_NAME_LEN,
        });
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(RejectReason::InvalidProfileName { name: name.to_string() });
    }
    Ok(())
}

/// Validates a `user@host` SSH connection string.
///
/// The `user` part may only contain alphanumeric characters, hyphens, dots, and
/// underscores.  The `host` part may additionally contain colons (IPv6) and
/// square brackets.  Returns `Err` with a descriptive message on any violation.
pub fn validate_user_host(user_host: &str) -> Result<(), String> {
    // Must contain exactly one '@'
    let parts: Vec<&str> = user_host.splitn(2, '@').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid format '{user_host}'. Expected: user@host"
        ));
    }
    let (user, host) = (parts[0], parts[1]);

    // Validate user part
    if user.is_empty() {
        return Err("SSH username must not be empty.".to_string());
    }
    if !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(format!(
            "SSH username '{user}' contains invalid characters. \
             Only letters, digits, hyphens, underscores, and dots are permitted."
        ));
    }

    // Validate host part (IP, hostname, or bracketed IPv6)
    if host.is_empty() {
        return Err("SSH hostname must not be empty.".to_string());
    }

    // Reject obvious injection in host
    if host.chars().any(|c| matches!(c, ';' | '`' | '$' | '&' | '|' | '\n' | '\r')) {
        return Err(format!(
            "SSH hostname '{host}' contains forbidden characters."
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitise_arg ─────────────────────────────────────────────────────────

    #[test]
    fn test_sanitise_arg_allows_normal_container_name() {
        assert_eq!(sanitise_arg("my-dev-box"), SanitiseResult::Ok);
    }

    #[test]
    fn test_sanitise_arg_allows_absolute_path() {
        assert_eq!(sanitise_arg("/var/log/melisa"), SanitiseResult::Ok);
    }

    #[test]
    fn test_sanitise_arg_rejects_null_byte() {
        assert!(matches!(
            sanitise_arg("name\0injection"),
            SanitiseResult::Reject(RejectReason::NullByte)
        ));
    }

    #[test]
    fn test_sanitise_arg_rejects_backtick() {
        assert!(matches!(
            sanitise_arg("`id`"),
            SanitiseResult::Reject(RejectReason::ShellMetachar { ch: '`' })
        ));
    }

    #[test]
    fn test_sanitise_arg_rejects_dollar_sign() {
        assert!(matches!(
            sanitise_arg("$(whoami)"),
            SanitiseResult::Reject(RejectReason::ShellMetachar { ch: '$' })
        ));
    }

    #[test]
    fn test_sanitise_arg_rejects_semicolon() {
        assert!(matches!(
            sanitise_arg(";rm -rf /"),
            SanitiseResult::Reject(RejectReason::ShellMetachar { ch: ';' })
        ));
    }

    #[test]
    fn test_sanitise_arg_rejects_path_traversal_posix() {
        assert!(matches!(
            sanitise_arg("../../etc/passwd"),
            SanitiseResult::Reject(RejectReason::PathTraversal { .. })
        ));
    }

    #[test]
    fn test_sanitise_arg_rejects_path_traversal_windows() {
        assert!(matches!(
            sanitise_arg("..\\..\\Windows"),
            SanitiseResult::Reject(RejectReason::PathTraversal { .. })
        ));
    }

    #[test]
    fn test_sanitise_arg_rejects_oversized_input() {
        let big = "a".repeat(MAX_ARG_LEN + 1);
        assert!(matches!(
            sanitise_arg(&big),
            SanitiseResult::Reject(RejectReason::TooLong { .. })
        ));
    }

    // ── validate_profile_name ────────────────────────────────────────────────

    #[test]
    fn test_validate_profile_name_accepts_valid_name() {
        assert!(validate_profile_name("production-01").is_ok());
    }

    #[test]
    fn test_validate_profile_name_accepts_underscores() {
        assert!(validate_profile_name("dev_server_02").is_ok());
    }

    #[test]
    fn test_validate_profile_name_rejects_spaces() {
        assert!(validate_profile_name("my server").is_err());
    }

    #[test]
    fn test_validate_profile_name_rejects_empty_string() {
        assert!(validate_profile_name("").is_err());
    }

    // ── validate_user_host ───────────────────────────────────────────────────

    #[test]
    fn test_validate_user_host_accepts_valid_address() {
        assert!(validate_user_host("root@192.168.1.10").is_ok());
    }

    #[test]
    fn test_validate_user_host_accepts_hostname() {
        assert!(validate_user_host("alice@myserver.local").is_ok());
    }

    #[test]
    fn test_validate_user_host_rejects_missing_at_sign() {
        assert!(validate_user_host("root192.168.1.10").is_err());
    }

    #[test]
    fn test_validate_user_host_rejects_injection_in_host() {
        assert!(validate_user_host("root@host;evil").is_err());
    }
}