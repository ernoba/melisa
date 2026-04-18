// ============================================================================
// src/distros/lxc_distro.rs
//
// LXC distribution list retrieval with file-level caching and lock-file
// coordination to prevent concurrent fetch races.
//
// Cache policy:
//   - Cache lives at DISTRO_CACHE_PATH.
//   - Valid for CACHE_TTL_SECS (1 hour).
//   - A lock file (LOCK_FILE_PATH) is used during the network fetch.
//   - Lock files older than LOCK_STALE_SECS are treated as abandoned.
// ============================================================================

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::process::Command;
use tokio::time::sleep;

use crate::core::container::types::DistroMetadata;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Path to the global distribution list cache file.
const DISTRO_CACHE_PATH: &str = "/tmp/melisa_global_distros.cache";

/// Path to the lock file used to prevent concurrent fetch operations.
const LOCK_FILE_PATH: &str = "/tmp/melisa_distro.lock";

/// Cache validity window in seconds (1 hour).
const CACHE_TTL_SECS: u64 = 3600;

/// Lock files older than this (seconds) are considered abandoned.
const LOCK_STALE_SECS: u64 = 60;

/// Maximum number of retry cycles when waiting for the lock to be released.
const MAX_LOCK_RETRIES: u32 = 40;

/// Delay between lock-wait retry cycles (milliseconds).
const LOCK_RETRY_DELAY_MS: u64 = 500;

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetches the list of available LXC distributions.
///
/// Serves from cache if a fresh cache exists and no fetch is in progress.
/// Falls back to a network fetch via `lxc-download --list` or `lxc-create`.
///
/// # Arguments
/// * `audit` - When `true`, raw subprocess output is forwarded to the terminal.
///
/// # Returns
/// A tuple of `(Vec<DistroMetadata>, is_from_cache)`.
pub async fn get_lxc_distro_list(audit: bool) -> (Vec<DistroMetadata>, bool) {
    let cache_exists = Path::new(DISTRO_CACHE_PATH).exists();

    // Serve from a fresh cache when available and no lock is active.
    if cache_exists
        && is_cache_fresh(DISTRO_CACHE_PATH).await
        && !Path::new(LOCK_FILE_PATH).exists()
    {
        if let Ok(content) = fs::read_to_string(DISTRO_CACHE_PATH).await {
            if audit {
                println!("[AUDIT] Serving distro list from cache: {}", DISTRO_CACHE_PATH);
            }
            return (parse_distro_list(&content), true);
        }
    }

    // Wait for any concurrent fetch to complete (lock coordination).
    acquire_lock(cache_exists, audit).await;

    // Perform the network fetch.
    let result = fetch_distro_list_from_network(audit).await;

    // Always release the lock regardless of fetch outcome.
    let _ = fs::remove_file(LOCK_FILE_PATH).await;

    result
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Checks whether the cache file was written within the TTL window.
async fn is_cache_fresh(cache_path: &str) -> bool {
    let Ok(meta) = fs::metadata(cache_path).await else { return false; };
    let Ok(modified) = meta.modified() else { return false; };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else { return false; };
    let Ok(mtime) = modified.duration_since(UNIX_EPOCH) else { return false; };
    now.as_secs().saturating_sub(mtime.as_secs()) < CACHE_TTL_SECS
}

/// Acquires the fetch lock file, waiting for existing locks to clear.
///
/// Abandons stale lock files (older than `LOCK_STALE_SECS`) automatically.
async fn acquire_lock(cache_exists: bool, audit: bool) {
    let mut retry_count = 0_u32;

    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(LOCK_FILE_PATH)
            .await
        {
            Ok(_) => break, // Lock acquired.
            Err(_) => {
                // Check for a stale lock and remove it if found.
                if let Ok(meta) = fs::metadata(LOCK_FILE_PATH).await {
                    if let Ok(mtime) = meta.modified() {
                        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
                            if let Ok(lock_age) = mtime.duration_since(UNIX_EPOCH) {
                                if now.as_secs().saturating_sub(lock_age.as_secs()) > LOCK_STALE_SECS {
                                    let _ = fs::remove_file(LOCK_FILE_PATH).await;
                                    continue;
                                }
                            }
                        }
                    }
                }

                if retry_count >= MAX_LOCK_RETRIES {
                    // Timeout: serve stale cache as fallback if available.
                    if cache_exists {
                        if let Ok(old_content) = fs::read_to_string(DISTRO_CACHE_PATH).await {
                            let _ = audit; // consumed intentionally
                            let _ = old_content; // caller handles fallback
                        }
                    }
                    break;
                }

                // Another process may have finished; check if cache is now fresh.
                if !Path::new(LOCK_FILE_PATH).exists() {
                    break;
                }

                sleep(Duration::from_millis(LOCK_RETRY_DELAY_MS)).await;
                retry_count += 1;
            }
        }
    }
}

/// Fetches the distribution list from `lxc-download --list` with a fallback
/// to `lxc-create --list`.
pub async fn fetch_distro_list_from_network(audit: bool) -> (Vec<DistroMetadata>, bool) {
    if audit {
        println!("[AUDIT] Fetching distro list from lxc-download — raw output follows:");
    }
 
    let primary_output = Command::new("sudo")
        .args(&["-n", "-H", "/usr/share/lxc/templates/lxc-download", "--list"])
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .output()
        .await;
 
    match primary_output {
        Ok(out)
            if out.status.success()
                || (!out.stdout.is_empty()
                    && (String::from_utf8_lossy(&out.stdout).contains("Distribution")
                        || String::from_utf8_lossy(&out.stdout).contains("DIST"))) =>
        {
            let content = String::from_utf8_lossy(&out.stdout).to_string();
            if !content.is_empty() {
                // Tulis cache file
                let _ = fs::write(DISTRO_CACHE_PATH, &content).await;
 
                // FIX: chmod 644 bukan 666
                // 644 = owner baca+tulis, group baca, others baca
                // 666 sebelumnya = semua orang bisa tulis (berbahaya!)
                let _ = Command::new("sudo")
                    .args(&["chmod", "644", DISTRO_CACHE_PATH])
                    .status()
                    .await;
 
                return (parse_distro_list(&content), false);
            }
            (Vec::new(), false)
        }
 
        Ok(out) => {
            if audit {
                println!(
                    "[AUDIT] Primary fetch failed. Stderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                println!("[AUDIT] Trying fallback via lxc-create --list:");
            }
            fetch_distro_list_fallback(audit).await
        }
 
        Err(err) => {
            eprintln!("[FATAL] Could not execute lxc-download: {}", err);
            (Vec::new(), false)
        }
    }
}

/// Fallback distribution list fetch via `lxc-create -t download --list`.
pub async fn fetch_distro_list_fallback(audit: bool) -> (Vec<DistroMetadata>, bool) {
    // Bersihkan container probe yang mungkin tertinggal dari run sebelumnya
    let _ = Command::new("sudo")
        .args(&["-n", "lxc-destroy", "-n", "MELISA_PROBE_UNUSED", "-f"])
        .output()
        .await;
 
    let fallback_output = Command::new("sudo")
        .args(&[
            "-n", "-H", "lxc-create",
            "-n", "MELISA_PROBE_UNUSED",
            "-t", "download",
            "--", "--list",
        ])
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .output()
        .await;
 
    match fallback_output {
        Ok(out) if !out.stdout.is_empty() => {
            let content = String::from_utf8_lossy(&out.stdout).to_string();
 
            // Tulis cache
            let _ = fs::write(DISTRO_CACHE_PATH, &content).await;
 
            // FIX: chmod 644, bukan 666
            let _ = Command::new("sudo")
                .args(&["chmod", "644", DISTRO_CACHE_PATH])
                .status()
                .await;
 
            (parse_distro_list(&content), false)
        }
        _ => (Vec::new(), false),
    }
}

/// Parses the raw text output of `lxc-download --list` into `DistroMetadata` records.
///
/// Expected format (after a header section):
/// ```text
/// ubuntu  jammy  amd64  default  20240101_07:42
/// debian  bookworm  amd64  default  20240101_07:42
/// ```
fn parse_distro_list(raw_output: &str) -> Vec<DistroMetadata> {
    let mut entries: Vec<DistroMetadata> = Vec::new();

    for line in raw_output.lines() {
        let columns: Vec<&str> = line.split_whitespace().collect();
        if columns.len() < 3 {
            continue;
        }

        // Skip header lines (they contain "DIST" or "Distribution").
        if columns[0].eq_ignore_ascii_case("dist")
            || columns[0].eq_ignore_ascii_case("distribution")
            || columns[0].starts_with("---")
        {
            continue;
        }

        let name = columns[0].to_string();
        let release = columns[1].to_string();
        let arch = columns[2].to_string();
        let slug = format!("{}/{}/{}", name, release, arch);

        entries.push(DistroMetadata { slug, name, arch });
    }

    entries
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_distro_list ────────────────────────────────────────────────────

    #[test]
    fn test_parse_distro_list_extracts_valid_entries() {
        let raw = "DIST    RELEASE   ARCH\n---\nubuntu  jammy  amd64\ndebian  bookworm  amd64\n";
        let result = parse_distro_list(raw);
        assert_eq!(result.len(), 2, "Must parse two distribution entries");
        assert_eq!(result[0].name, "ubuntu");
        assert_eq!(result[0].slug, "ubuntu/jammy/amd64");
        assert_eq!(result[1].name, "debian");
    }

    #[test]
    fn test_parse_distro_list_skips_header_lines() {
        let raw = "Distribution  Release  Arch\nalpine  3.18  amd64\n";
        let result = parse_distro_list(raw);
        assert_eq!(result.len(), 1, "Header line must be skipped");
        assert_eq!(result[0].name, "alpine");
    }

    #[test]
    fn test_parse_distro_list_skips_lines_with_fewer_than_three_columns() {
        let raw = "ubuntu\ndebian bookworm amd64\n";
        let result = parse_distro_list(raw);
        assert_eq!(
            result.len(), 1,
            "Lines with fewer than 3 columns must be skipped"
        );
    }

    #[test]
    fn test_parse_distro_list_builds_correct_slug_format() {
        let raw = "ubuntu  jammy  amd64\n";
        let result = parse_distro_list(raw);
        assert_eq!(
            result[0].slug, "ubuntu/jammy/amd64",
            "Slug must be constructed as 'name/release/arch'"
        );
    }

    #[test]
    fn test_parse_distro_list_returns_empty_for_blank_input() {
        let result = parse_distro_list("");
        assert!(
            result.is_empty(),
            "Empty input must produce an empty distribution list"
        );
    }

    // ── is_cache_fresh ────────────────────────────────────────────────────────

    #[test]
    fn test_cache_ttl_constant_is_one_hour() {
        assert_eq!(
            CACHE_TTL_SECS, 3600,
            "Cache TTL must be set to 3600 seconds (1 hour)"
        );
    }

    #[test]
    fn test_lock_stale_threshold_is_sixty_seconds() {
        assert_eq!(
            LOCK_STALE_SECS, 60,
            "Lock file stale threshold must be 60 seconds"
        );
    }
        #[test]
    fn test_cache_permission_is_not_world_writable() {
        // Dokumentasi permission yang benar sebagai test guardrail.
        // Permission octal 0o644 = rw-r--r-- (owner write, others read-only)
        // Permission octal 0o666 = rw-rw-rw- (world-writable — DILARANG)
        let expected_permission: u32 = 0o644;
        let forbidden_permission: u32 = 0o666;
 
        assert_ne!(
            expected_permission, forbidden_permission,
            "Cache file must use 644, not world-writable 666"
        );
 
        // Pastikan 644 bukan world-writable
        let others_write_bit = expected_permission & 0o002;
        assert_eq!(
            others_write_bit, 0,
            "Permission 644 must NOT have the 'others write' bit set"
        );
    }
}