// ============================================================================
// src/cli/melisa_cli.rs
//
// Primary entry point for the MELISA interactive shell (REPL).
//
// Initializes the Rustyline editor, injects the MelisaHelper, loads session
// history, and drives the main input loop.  Each command is dispatched to
// `executor::execute_command` which returns an [`ExecResult`] that controls
// loop continuation.
//

use tokio::fs;
use rustyline::{Editor, Config};
use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::completion::FilenameCompleter;
use rustyline::highlight::MatchingBracketHighlighter;
use rustyline::hint::HistoryHinter;
use rustyline::validate::MatchingBracketValidator;

use crate::cli::color::{RED, YELLOW, GREEN, RESET, BOLD};
use crate::cli::executor::{execute_command, ExecResult};
use crate::cli::helper::MelisaHelper;
use crate::cli::prompt::{Prompt, reset_history};
// FIX-01: This import was entirely absent in the original file.
use crate::cli::wellcome::display_melisa_banner;

// ── REPL entry point ──────────────────────────────────────────────────────────

/// Launches the MELISA interactive shell.
///
/// Execution order:
/// 1. Display the animated welcome banner (was missing — see FIX-01).
/// 2. Resolve user identity and home directory via [`Prompt::new`], with
///    sudo-safe home correction (see FIX-03).
/// 3. Configure the Rustyline editor with history and autocompletion.
/// 4. Drive the main read-eval-print loop until the user exits.
pub async fn melisa() {
    // ── Step 1: Welcome banner ────────────────────────────────────────────────
    //
    // FIX-01 + FIX-02: Call the banner FIRST, before any other output.
    //
    // `display_melisa_banner()` performs:
    //   - clear_screen()
    //   - system_boot_sequence()  (animated boot log lines)
    //   - decrypt_core_animation()  (glitch-decrypt text animation)
    //   - display_system_dashboard()  (sysinfo telemetry table)
    //   - enforce_isolation_directives()  (final status line)
    //
    // After this call stdout ends with a printed line but the cursor is at
    // column 0 of the NEXT line (enforce_isolation_directives was fixed in
    // wellcome.rs to end with println! instead of print!).
    display_melisa_banner();

    // FIX-04: Ensure stdout is on a fresh line before Rustyline takes over
    // raw-mode control.  Without this, the first Rustyline prompt is drawn
    // on the same terminal line as the last banner output, producing visual
    // corruption.
    println!();

    // ── Step 2: Resolve user identity & home directory ────────────────────────
    let mut p_info = Prompt::new();

    // FIX-03: Correct the home directory when running as root via sudo.
    //
    // `main.rs` re-execs via `sudo -E`.  Depending on the host sudoers config,
    // HOME may still be set to /root even though SUDO_USER is alice.
    // `Prompt::new()` falls back to $HOME, which would be /root.
    //
    // We correct this by checking SUDO_USER and deriving the home path from it.
    // The path /home/<sudo_user> is used when it exists; otherwise we fall back
    // to whatever $HOME reports (handles non-standard home dirs like /opt/<u>).
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        // Only override if p_info.user resolved to SUDO_USER (i.e., not "root")
        if p_info.user == sudo_user && p_info.home == "/root" {
            let candidate = format!("/home/{}", sudo_user);
            if std::path::Path::new(&candidate).exists() {
                p_info.home = candidate;
            }
            // If the candidate does not exist, keep whatever HOME reports.
            // This preserves compatibility with non-standard home directories.
        }
    }

    // FIX-02: Print the authenticated-as line AFTER the banner, not before.
    println!(
        "{}Authenticated as {}. Secure session granted.{}",
        BOLD, p_info.user, RESET
    );

    // ── Step 3: History storage path ─────────────────────────────────────────
    //
    // Align storage with the directories provisioned by the bash installer:
    //   ~/.local/share/melisa/history.txt
    //
    // FIX-03 (continued): Because p_info.home is now correct, the history
    // file lands in the invoking user's home, not in /root.
    let melisa_dir   = format!("{}/.local/share/melisa", p_info.home);
    let history_path = format!("{}/history.txt", melisa_dir);

    // FIX-06: Surface directory-creation errors rather than silently ignoring.
    if let Err(e) = fs::create_dir_all(&melisa_dir).await {
        eprintln!(
            "{}[WARNING]{} Could not create MELISA state directory '{}': {}",
            YELLOW, RESET, melisa_dir, e
        );
        eprintln!(
            "{}[WARNING]{} Session history will NOT be persisted this session.",
            YELLOW, RESET
        );
    }

    // ── Step 4: Configure the Rustyline editor ────────────────────────────────
    let config = Config::builder()
        .history_ignore_dups(true)
        .unwrap_or_default()
        // History is managed manually for security: commands are added only
        // after successful execution, and the disk write is incremental.
        .auto_add_history(false)
        .build();

    let mut rl: Editor<MelisaHelper, FileHistory> = Editor::with_config(config)
        .expect("FATAL: Failed to allocate terminal interface for Rustyline Editor");

    // Inject the custom MELISA helper:
    //   - HistoryHinter      → ghost-text history suggestions
    //   - MatchingBracket*   → bracket matching highlight + validation
    //   - FilenameCompleter  → file-path completion fallback
    rl.set_helper(Some(MelisaHelper {
        hinter:         HistoryHinter {},
        highlighter:    MatchingBracketHighlighter::new(),
        validator:      MatchingBracketValidator::new(),
        file_completer: FilenameCompleter::new(),
    }));

    // Load previous session history (soft fail — first run has no file).
    let _ = rl.load_history(&history_path);

    // ── Step 5: Core REPL loop ────────────────────────────────────────────────
    loop {
        // Build a fresh prompt string on every iteration so that directory
        // changes made via `cd` are reflected immediately.
        let prompt_str = p_info.build();

        // `rustyline::readline` blocks the current OS thread in raw-mode while
        // waiting for user input.  This is unavoidable for terminal handling.
        match rl.readline(&prompt_str) {
            Ok(line) => {
                let input = line.trim();

                // Skip blank lines without adding them to history.
                if input.is_empty() {
                    continue;
                }

                match execute_command(input, &p_info.user, &p_info.home).await {
                    // ── History management ────────────────────────────────────
                    //
                    // History is managed per-result:
                    //   Continue     → persist the command
                    //   Break        → persist the exit command, then quit
                    //   ResetHistory → purge history, do NOT add this command
                    //   Error        → persist even failed commands (enables
                    //                  Up-arrow correction of typos)
                    //
                    ExecResult::ResetHistory => {
                        // Delegate the purge + re-init to the dedicated helper.
                        // The "--clear" command itself is intentionally NOT added
                        // to history after a reset.
                        reset_history(&mut rl, &history_path).await;
                    }

                    ExecResult::Continue => {
                        // Register the command in the RAM buffer, then flush
                        // only the new delta to disk (incremental append).
                        let _ = rl.add_history_entry(input);
                        let _ = rl.append_history(&history_path);
                    }

                    ExecResult::Break => {
                        // FIX-05: Persist the exit/quit command before breaking
                        // so the user can see it in the next session's history.
                        let _ = rl.add_history_entry(input);
                        let _ = rl.append_history(&history_path);
                        println!(
                            "{}[SESSION]{} Secure session terminated. Goodbye.{}",
                            GREEN, RESET, RESET
                        );
                        break;
                    }

                    ExecResult::Error(e) => {
                        // Persist errored commands: the Up-arrow then lets the
                        // user correct typos without retyping the full command.
                        let _ = rl.add_history_entry(input);
                        let _ = rl.append_history(&history_path);
                        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
                    }
                }
            }

            // ── Terminal signal handling ──────────────────────────────────────

            Err(ReadlineError::Interrupted) => {
                // Ctrl+C: cancel the current input line but keep the session
                // open.  This mirrors standard bash/zsh behaviour.
                println!("{}[CTRL+C]{} Input cancelled. Session continues.{}", YELLOW, RESET, RESET);
                continue;
            }

            Err(ReadlineError::Eof) => {
                // Ctrl+D: user closed the input stream — terminate gracefully.
                println!("{}[EOF]{} Session terminated via EOF signal.{}", GREEN, RESET, RESET);
                break;
            }

            Err(err) => {
                // Any other Rustyline error is unrecoverable.  Log it and exit
                // to prevent a silent infinite loop in a broken terminal state.
                eprintln!(
                    "{}[FATAL]{} Readline encountered an unrecoverable error: {:?}",
                    RED, RESET, err
                );
                break;
            }
        }
    }

    // Persist any remaining in-memory history entries on clean exit.
    // This is a safety net in case the loop exits without the Break arm.
    let _ = rl.save_history(&history_path);
}