// ============================================================================
// src/cli/executor.rs
//
// MELISA command router and dispatcher.
//
// `execute_command` is the single entry-point called by the REPL loop.
// It parses raw input, strips the optional `--audit` flag, then delegates
// to the appropriate domain function.
//
// FIX #1: Added handlers for `exit`, `quit`, `cd`, and bash passthrough.
//          The old version (saferoom) supported all of these — the new version 
//          skipped them all so the shell could not exit normally.
//
// FIX #2: The `?` operator cannot be used directly in functions that return ExecResult
//         (instead of Result<_, _>). Solution: a `req!` macro whose behavior is identical to `?`
//         but is compatible with all return types.
// ============================================================================

use tokio::io::{self, AsyncBufReadExt};
use std::io::Write;
use std::env;

use crate::cli::color::{RED, GREEN, YELLOW, BOLD, RESET};
use crate::cli::loading::execute_with_spinner;
use crate::core::container::{
    create_container, delete_container, start_container, stop_container,
    attach_to_container, send_command, get_container_ip, list_containers,
    upload_to_container, add_shared_folder, remove_shared_folder,
};
use crate::core::metadata::{print_version, inspect_container_metadata, MelisaError};
use crate::core::root_check::admin_check;
use crate::core::setup::install_host_environment;
use crate::core::user::{
    add_melisa_user, set_user_password, delete_melisa_user,
    list_melisa_users, upgrade_user, clean_orphaned_sudoers,
};
use crate::core::project::{
    PROJECTS_MASTER_PATH, delete_project, invite_users_to_project, list_projects,
    create_new_project, remove_users_from_project, pull_user_workspace,
    update_project_for_user, distribute_master_to_all_members,
};
use crate::distros::lxc_distro::get_lxc_distro_list;
use crate::deployment::deployer::{cmd_up, cmd_down, cmd_mel_info};

// ── FIX: Replacement macro for the `?` operator for the ExecResult return type ────────────
macro_rules! req {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

// ── Result type for REPL loop control ───────────────────────────────────────

/// Signals returned to the REPL loop after each command execution.
#[derive(Debug)]
pub enum ExecResult {
    /// Continue the REPL loop normally.
    Continue,
    /// Exit the REPL loop cleanly.
    Break,
    /// Purge command history and restart the REPL loop.
    ResetHistory,
    /// An unrecoverable error occurred; carry the message for logging.
    Error(String),
}

// ── Input parsing ────────────────────────────────────────────────────────────

/// Parses a raw input line into `(tokens, is_audit_mode)`.
///
/// The `--audit` flag may appear anywhere in the input.  It is stripped from
/// the token list before the caller processes arguments.
pub fn parse_command(input: &str) -> (Vec<String>, bool) {
    let raw_tokens: Vec<&str> = input.split_whitespace().collect();
    let is_audit_mode = raw_tokens.contains(&"--audit");
    let tokens: Vec<String> = raw_tokens
        .into_iter()
        .filter(|&token| token != "--audit")
        .map(String::from)
        .collect();
    (tokens, is_audit_mode)
}

// ── Main dispatcher ──────────────────────────────────────────────────────────

/// Routes a single REPL input line to the correct handler.
///
/// # Arguments
/// * `input` - Raw input string from the user.
/// * `user`  - Current login username.
/// * `home`  - Current user's home directory.
///
/// # Returns
/// An [`ExecResult`] that tells the REPL loop what to do next.
pub async fn execute_command(input: &str, user: &str, home: &str) -> ExecResult {
    let (tokens, is_audit_mode) = parse_command(input);

    if tokens.is_empty() {
        return ExecResult::Continue;
    }

    match tokens[0].as_str() {
        // ── Session / navigation ─────────────────────────────────────────────────
        //
        // FIX: The new version previously skipped all of this, resulting in:
        //   - `exit` / `quit` not being recognized → the shell couldn't be closed
        //   - `cd` not working → the directory couldn't be changed
        //   - Arbitrary shell commands couldn't be executed
        //
        "exit" | "quit" => {
            println!(
                "{}[SYSTEM]{} Terminating secure session... Goodbye.{}",
                BOLD, YELLOW, RESET
            );
            ExecResult::Break
        }

        "cd" => {
            // Change the working directory of the REPL session in-process.
            // If there are no arguments, move to the home directory.
            let target = tokens.get(1).map(|s| s.as_str()).unwrap_or(home);
            let target = if target == "~" { home } else { target };

            match env::set_current_dir(target) {
                Ok(_) => ExecResult::Continue,
                Err(e) => ExecResult::Error(format!("{}cd: {}{}", RED, e, RESET)),
            }
        }

        // ── melisa subcommand ────────────────────────────────────────────────
        "melisa" => dispatch_melisa_subcommand(&tokens, is_audit_mode, user, home).await,

        // ── Bash passthrough ─────────────────────────────────────────────────
        //
        // FIX: Any command not recognized by the REPL is passed through to
        // bash, exactly like the old version. This allows the user to run
        // system tools (git, vim, python3, etc.) directly from within the MELISA shell.
        //
        _ => {
            // Add ~/.cargo/bin to PATH so that the user's Rust binaries are available.
            let cargo_bin = format!("{}/.cargo/bin", home);
            let path_env = format!(
                "{}:{}",
                cargo_bin,
                env::var("PATH").unwrap_or_default()
            );

            let status = tokio::process::Command::new("bash")
                .env("PATH", path_env)
                .env("HOME", home)
                .env("USER", user)
                .envs([
                    ("RUSTUP_HOME",    format!("{}/.rustup", home)),
                    ("CARGO_HOME",     format!("{}/.cargo", home)),
                    ("RUSTUP_TOOLCHAIN", "stable".into()),
                ])
                .args(["-c", input])
                .status()
                .await;

            match status {
                Ok(_) => ExecResult::Continue,
                Err(e) => ExecResult::Error(format!("Bash Execution Error: {}", e)),
            }
        }
    }
}

// ── Sub-command dispatcher ───────────────────────────────────────────────────

/// Handles all `melisa <subcommand>` forms.
async fn dispatch_melisa_subcommand(
    tokens: &[String],
    is_audit_mode: bool,
    user: &str,
    home: &str,
) -> ExecResult {
    let sub_command = tokens.get(1).map(|s| s.as_str()).unwrap_or("");

    match sub_command {
        // ── Help & info ──────────────────────────────────────────────────────
        "--help" | "-h" => {
            print_help(is_audit_mode).await;
            ExecResult::Continue
        }

        "--version" => {
            print_version().await;
            ExecResult::Continue
        }

        // ── Deployment engine ────────────────────────────────────────────────
        "--up" => {
            let mel_path = req!(require_arg(tokens, 2, "melisa --up <file.mel>"));
            cmd_up(mel_path, is_audit_mode).await;
            ExecResult::Continue
        }

        "--down" => {
            let mel_path = req!(require_arg(tokens, 2, "melisa --down <file.mel>"));
            cmd_down(mel_path, is_audit_mode).await;
            ExecResult::Continue
        }

        "--mel-info" => {
            let mel_path = req!(require_arg(tokens, 2, "melisa --mel-info <file.mel>"));
            cmd_mel_info(mel_path).await;
            ExecResult::Continue
        }

        // ── Host setup ───────────────────────────────────────────────────────
        "--setup" => {
            install_host_environment().await;
            ExecResult::Continue
        }

        "--clear" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} You do not have sufficient privileges to clear system history.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            ExecResult::ResetHistory
        }

        // ── Distribution search ──────────────────────────────────────────────
        "--search" => {
            let keyword = tokens
                .get(2)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            handle_search_distro(keyword, is_audit_mode).await;
            ExecResult::Continue
        }

        // ── Container lifecycle ──────────────────────────────────────────────
        "--create" => {
            let name = req!(require_arg(tokens, 2, "melisa --create <n> <distro_code>"));
            let distro_code = req!(require_arg(tokens, 3, "melisa --create <n> <distro_code>"));
            handle_create_container(name, distro_code, is_audit_mode).await;
            ExecResult::Continue
        }

        "--delete" => {
            let name = req!(require_arg(tokens, 2, "melisa --delete <n>"));
            if confirm_destructive_action(&format!("permanently delete container '{}'", name)).await {
                execute_with_spinner(
                    &format!("Destroying container '{}'...", name),
                    |pb| delete_container(name, pb, is_audit_mode),
                    is_audit_mode,
                )
                .await;
            }
            ExecResult::Continue
        }

        "--run" => {
            let name = req!(require_arg(tokens, 2, "melisa --run <n>"));
            start_container(name, is_audit_mode).await;
            ExecResult::Continue
        }

        "--stop" => {
            let name = req!(require_arg(tokens, 2, "melisa --stop <n>"));
            stop_container(name, is_audit_mode).await;
            ExecResult::Continue
        }

        "--use" => {
            let name = req!(require_arg(tokens, 2, "melisa --use <n>"));
            attach_to_container(name).await;
            ExecResult::Continue
        }

        "--send" => {
            let name = req!(require_arg(tokens, 2, "melisa --send <n> <command>"));
            let cmd_args: Vec<&str> = tokens[3..].iter().map(|s| s.as_str()).collect();
            if cmd_args.is_empty() {
                println!(
                    "{}[ERROR]{} Command payload required. Usage: melisa --send <n> <command>",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            send_command(name, &cmd_args).await;
            ExecResult::Continue
        }

        "--info" => {
            let name = req!(require_arg(tokens, 2, "melisa --info <n>"));
            handle_container_info(name).await;
            ExecResult::Continue
        }

        "--ip" => {
            let name = req!(require_arg(tokens, 2, "melisa --ip <n>"));
            match get_container_ip(name).await {
                Some(ip) => println!("{}", ip),
                None => eprintln!(
                    "{}[ERROR]{} Cannot get IP for '{}'. Container may be stopped or lack DHCP.",
                    RED, RESET, name
                ),
            }
            ExecResult::Continue
        }

        "--upload" => {
            let name = req!(require_arg(tokens, 2, "melisa --upload <n> <dest_path>"));
            let dest_path = req!(require_arg(tokens, 3, "melisa --upload <n> <dest_path>"));
            upload_to_container(name, dest_path).await;
            ExecResult::Continue
        }

        "--list" => {
            list_containers(false).await;
            ExecResult::Continue
        }

        "--active" => {
            list_containers(true).await;
            ExecResult::Continue
        }

        "--share" => {
            match (tokens.get(2), tokens.get(3), tokens.get(4)) {
                (Some(name), Some(host_path), Some(container_path)) => {
                    add_shared_folder(name, host_path, container_path).await;
                }
                _ => println!(
                    "{}[ERROR]{} Usage: melisa --share <n> <host_path> <container_path>",
                    RED, RESET
                ),
            }
            ExecResult::Continue
        }

        "--reshare" => {
            match (tokens.get(2), tokens.get(3), tokens.get(4)) {
                (Some(name), Some(host_path), Some(container_path)) => {
                    remove_shared_folder(name, host_path, container_path).await;
                }
                _ => println!(
                    "{}[ERROR]{} Usage: melisa --reshare <n> <host_path> <container_path>",
                    RED, RESET
                ),
            }
            ExecResult::Continue
        }

        // ── User management ──────────────────────────────────────────────────
        "--add" => {
            let username = req!(require_arg(tokens, 2, "melisa --add <username>"));
            add_melisa_user(username, is_audit_mode).await;
            ExecResult::Continue
        }

        "--remove" => {
            let username = req!(require_arg(tokens, 2, "melisa --remove <username>"));
            if confirm_destructive_action(&format!("permanently delete user '{}'", username)).await {
                delete_melisa_user(username, is_audit_mode).await;
            }
            ExecResult::Continue
        }

        "--user" => {
            list_melisa_users().await;
            ExecResult::Continue
        }

        "--upgrade" => {
            let username = req!(require_arg(tokens, 2, "melisa --upgrade <username>"));
            upgrade_user(username, is_audit_mode).await;
            ExecResult::Continue
        }

        "--passwd" => {
            let username = req!(require_arg(tokens, 2, "melisa --passwd <username>"));
            set_user_password(username).await;
            ExecResult::Continue
        }

        "--clean" => {
            clean_orphaned_sudoers().await;
            ExecResult::Continue
        }

        // ── Project orchestration ────────────────────────────────────────────
        "--new_project" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} Only administrators can provision new master projects.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            let project_name = req!(require_arg(tokens, 2, "melisa --new_project <project_name>"));
            create_new_project(project_name, is_audit_mode).await;
            ExecResult::Continue
        }

        "--delete_project" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} Only administrators can delete master projects.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            let project_name = req!(require_arg(tokens, 2, "melisa --delete_project <project_name>"));
            let master_path = format!("{}/{}", PROJECTS_MASTER_PATH, project_name);
            if !std::path::Path::new(&master_path).exists() {
                println!(
                    "{}[ERROR]{} Master project '{}' does not exist.",
                    RED, RESET, project_name
                );
                return ExecResult::Continue;
            }
            delete_project(&master_path, project_name).await;
            ExecResult::Continue
        }

        "--invite" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} Only administrators can assign users to projects.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            if tokens.len() < 4 {
                println!(
                    "{}[ERROR]{} Usage: melisa --invite <project_name> <user1> [user2 …]",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            let project_name = &tokens[2];
            let master_path = format!("{}/{}", PROJECTS_MASTER_PATH, project_name);
            if !std::path::Path::new(&master_path).exists() {
                println!(
                    "{}[ERROR]{} Master project '{}' does not exist.",
                    RED, RESET, project_name
                );
                return ExecResult::Continue;
            }
            let invited_users: Vec<&str> = tokens[3..].iter().map(|s| s.as_str()).collect();
            invite_users_to_project(project_name, &invited_users, is_audit_mode).await;
            ExecResult::Continue
        }

        "--out" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} Only administrators can revoke user project access.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            if tokens.len() < 4 {
                println!(
                    "{}[ERROR]{} Usage: melisa --out <project_name> <user1> [user2 …]",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            let project_name = &tokens[2];
            let target_users: Vec<&str> = tokens[3..].iter().map(|s| s.as_str()).collect();
            remove_users_from_project(project_name, &target_users, is_audit_mode).await;
            ExecResult::Continue
        }

        "--pull" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} Only administrators can pull user workspaces.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            if tokens.len() < 4 {
                println!(
                    "{}[ERROR]{} Usage: melisa --pull <from_user> <project_name>",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            let from_user = &tokens[2];
            let project_name = &tokens[3];
            pull_user_workspace(from_user, project_name, is_audit_mode).await;
            ExecResult::Continue
        }

        "--projects" => {
            list_projects(home).await;
            ExecResult::Continue
        }

        "--update" => {
            let project_name = req!(require_arg(tokens, 2, "melisa --update <project_name>"));
            update_project_for_user(project_name, user, is_audit_mode).await;
            ExecResult::Continue
        }

        "--update-all" => {
            if !admin_check().await {
                println!(
                    "{}[ERROR]{} Only administrators can distribute master updates.",
                    RED, RESET
                );
                return ExecResult::Continue;
            }
            let project_name = req!(require_arg(tokens, 2, "melisa --update-all <project_name>"));
            distribute_master_to_all_members(project_name, is_audit_mode).await;
            ExecResult::Continue
        }

        // ── Empty subcommand ────────────────────────────────────────────────
        "" => {
            println!(
                "{}[ERROR]{} Incomplete command. Execute 'melisa --help' for usage.",
                RED, RESET
            );
            ExecResult::Continue
        }

        // ── Unknown sub-command ──────────────────────────────────────────────
        other => {
            println!(
                "{}[ERROR]{} Unknown option '{}'. Run 'melisa --help' for available commands.",
                RED, RESET, other
            );
            ExecResult::Continue
        }
    }
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Returns `Ok(&tokens[index])` or prints a usage hint and returns a
/// `Continue` `ExecResult` wrapped in `Err`.
fn require_arg<'a>(
    tokens: &'a [String],
    index: usize,
    usage: &str,
) -> Result<&'a str, ExecResult> {
    match tokens.get(index) {
        Some(val) if !val.is_empty() => Ok(val.as_str()),
        _ => {
            println!("{}[ERROR]{} Missing argument. Usage: {}", RED, RESET, usage);
            Err(ExecResult::Continue)
        }
    }
}

/// Prompts the user to confirm a destructive action.
///
/// Returns `true` only when the user explicitly types `y` or `yes`.
async fn confirm_destructive_action(action_description: &str) -> bool {
    print!(
        "{}Are you sure you want to {}? (y/N): {}",
        RED, action_description, RESET
    );
    std::io::stdout().flush().expect("Failed to flush stdout");

    let mut response = String::new();
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin);

    if reader.read_line(&mut response).await.is_ok() {
        let trimmed = response.trim().to_lowercase();
        if trimmed.is_empty() {
            println!("{}[CANCEL]{} No input detected. Operation aborted.", YELLOW, RESET);
            return false;
        }
        if trimmed == "y" || trimmed == "yes" {
            return true;
        }
        println!("{}[CANCEL]{} Operation aborted.", YELLOW, RESET);
    }
    false
}

/// Searches the distribution list and prints matching entries.
async fn handle_search_distro(keyword: String, is_audit_mode: bool) {
    let (distro_list, is_cache) = execute_with_spinner(
        "Synchronizing distribution list...",
        |_pb| get_lxc_distro_list(is_audit_mode),
        is_audit_mode,
    )
    .await;

    if distro_list.is_empty() {
        println!(
            "{}[ERROR]{} Failed to retrieve the distribution list from LXC.",
            RED, RESET
        );
        println!(
            "{}Tip:{} Ensure LXC is properly configured and the network is reachable.",
            YELLOW, RESET
        );
        return;
    }

    if is_cache {
        println!(
            "{}[CACHE]{} Displaying local data (offline / cached mode).",
            YELLOW, RESET
        );
    } else {
        println!(
            "{}[FRESH]{} Successfully synchronized the latest distribution index.",
            GREEN, RESET
        );
    }

    println!("\n{:<20} | {:<10} | {:<10}", "UNIQUE CODE", "DISTRO", "ARCH");
    println!("{}", "-".repeat(45));

    for distro in distro_list {
        if distro.slug.contains(&keyword) || distro.name.contains(&keyword) {
            println!("{:<20} | {:<10} | {:<10}", distro.slug, distro.name, distro.arch);
        }
    }
}

/// Validates the distribution code and creates a new LXC container.
async fn handle_create_container(name: &str, distro_code: &str, is_audit_mode: bool) {
    let (distro_list, is_cache) = execute_with_spinner(
        "Validating distribution metadata...",
        |_pb| get_lxc_distro_list(is_audit_mode),
        is_audit_mode,
    )
    .await;

    if distro_list.is_empty() {
        println!(
            "{}[ERROR]{} Failed to retrieve the distribution list. Cannot validate the code.",
            RED, RESET
        );
        return;
    }

    if is_cache {
        println!(
            "{}[INFO]{} Validating distribution code '{}' against local cache.",
            YELLOW, RESET, distro_code
        );
    }

    match distro_list.into_iter().find(|d| d.slug == distro_code) {
        Some(meta) => {
            execute_with_spinner(
                &format!("Provisioning container '{}'...", name),
                |pb| create_container(name, meta, pb, is_audit_mode),
                is_audit_mode,
            )
            .await;
        }
        None => {
            println!(
                "{}[ERROR]{} Distro code '{}' was not found in the distribution registry.",
                RED, RESET, distro_code
            );
            println!(
                "{}Tip:{} Run 'melisa --search' to view available distribution codes.",
                YELLOW, RESET
            );
        }
    }
}

/// Fetches and prints container metadata.
async fn handle_container_info(name: &str) {
    println!("{}Searching metadata for container: {}...{}", BOLD, name, RESET);
    match inspect_container_metadata(name).await {
        Ok(data) => {
            println!("\n--- [ MELISA CONTAINER INFO ] ---");
            println!("{}", data.trim());
            println!("---------------------------------");
        }
        Err(MelisaError::MetadataNotFound(_)) => {
            println!(
                "{}[ERROR]{} Container '{}' lacks MELISA metadata. \
                It may not have been provisioned via the MELISA engine.",
                RED, RESET, name
            );
        }
        Err(err) => {
            println!("{}[ERROR]{} An unexpected error occurred: {}", RED, RESET, err);
        }
    }
}

/// Prints the full help menu. Admin-only sections are gated on privilege check.
async fn print_help(is_audit_mode: bool) {
    let is_admin = admin_check().await;

    println!("\n{}MELISA CONTROL INTERFACE — VERSION 0.1.3{}", BOLD, RESET);
    println!("Usage: melisa [options] [--audit]\n");
    println!(
        "{}[--audit]{} can be added to any command to display hidden logs\n\
        and show subprocess output directly in the terminal.\n",
        YELLOW, RESET
    );

    println!("{}GENERAL COMMANDS{}", BOLD, RESET);
    println!("  --help, -h             Show this help guide");
    println!("  --version              Show system version");
    println!("  --ip <n>               Get internal IP of the container");
    println!("  --projects             List all workspace projects");
    println!("  --update <project>     Synchronize project workdir via force-reset");
    println!("  --list                 Show all LXC containers");
    println!("  --active               Show only running containers");
    println!("  --run <n>              Start container");
    println!("  --stop <n>             Stop container");
    println!("  --use <n>              Enter interactive container shell");
    println!("  --send <n> <cmd>       Send command to container");
    println!("  --info <n>             Show container metadata");
    println!("  --upload <n> <dest>    Upload file to container");

    println!("\n{}DEPLOYMENT ENGINE (.mel){}", BOLD, RESET);
    println!("  --up <file.mel>        Deploy project from .mel manifest");
    println!("  --down <file.mel>      Stop deployment from .mel manifest");
    println!("  --mel-info <file.mel>  Show .mel manifest info");

    if is_admin {
        println!("\n{}ADMINISTRATION & INFRASTRUCTURE{}", BOLD, RESET);
        println!("  --setup                Initialize host environment");
        println!("  --clear                Clear command history");
        println!("  --clean                Clean orphaned sudoers configuration");
        println!("  --search <keyword>     Search available LXC distributions");
        println!("  --create <n> <code>    Create new container from distribution code");
        println!("  --delete <n>           Delete container (requires confirmation)");
        println!("  --share <n> <h> <c>    Mount host directory to container");
        println!("  --reshare <n> <h> <c>  Unmount directory from container");

        println!("\n{}IDENTITY & ACCESS MANAGEMENT{}", BOLD, RESET);
        println!("  --user                 List all MELISA identities");
        println!("  --add <user>           Add new user");
        println!("  --remove <user>        Remove user (requires confirmation)");
        println!("  --upgrade <user>       Modify user access level");
        println!("  --passwd <user>        Change user credentials");

        println!("\n{}PROJECT ORCHESTRATION{}", BOLD, RESET);
        println!("  --new_project <n>      Initialize new project repository");
        println!("  --delete_project <n>   Delete project and all workdirs");
        println!("  --invite <p> <u…>      Grant project access to user(s)");
        println!("  --out <p> <u…>         Revoke project access from user(s)");
        println!("  --pull <user> <proj>   Merge code from user workdir to master");
        println!("  --update-all <proj>    Distribute master updates to all members");
    }

    let _ = is_audit_mode;
    println!("\n{}Note: System modifications require SUID elevation.{}", BOLD, RESET);
    println!("\n{}SHELL COMMANDS{}", BOLD, RESET);
    println!("  exit / quit            Terminate the MELISA session");
    println!("  cd <path>              Change working directory");
    println!("  <any shell command>    Execute directly via bash");
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_command ────────────────────────────────────────────────────────

    #[test]
    fn test_parse_command_no_audit_flag_returns_false() {
        let (tokens, is_audit) = parse_command("melisa --list");
        assert!(!is_audit, "audit flag must be false when '--audit' is absent");
        assert_eq!(tokens, vec!["melisa", "--list"]);
    }

    #[test]
    fn test_parse_command_with_audit_flag_sets_true_and_strips_flag() {
        let (tokens, is_audit) = parse_command("melisa --list --audit");
        assert!(is_audit, "audit flag must be true when '--audit' is present");
        assert!(
            !tokens.contains(&"--audit".to_string()),
            "--audit must be stripped from the token list"
        );
        assert_eq!(tokens, vec!["melisa", "--list"]);
    }

    #[test]
    fn test_parse_command_audit_flag_in_middle_is_stripped() {
        let (tokens, is_audit) = parse_command("melisa --audit --stop mybox");
        assert!(is_audit, "audit flag must be detected regardless of position");
        assert_eq!(tokens, vec!["melisa", "--stop", "mybox"]);
    }

    #[test]
    fn test_parse_command_empty_input_produces_empty_tokens() {
        let (tokens, is_audit) = parse_command("");
        assert!(tokens.is_empty(), "Empty input must produce an empty token list");
        assert!(!is_audit, "Empty input must not set the audit flag");
    }

    #[test]
    fn test_parse_command_whitespace_only_produces_empty_tokens() {
        let (tokens, _) = parse_command("   ");
        assert!(tokens.is_empty(), "Whitespace-only input must produce an empty token list");
    }

    #[test]
    fn test_parse_command_preserves_argument_order() {
        let (tokens, _) = parse_command("melisa --send mybox apt update -y");
        assert_eq!(
            tokens,
            vec!["melisa", "--send", "mybox", "apt", "update", "-y"],
            "Token order must be preserved after stripping --audit"
        );
    }

    // ── require_arg ──────────────────────────────────────────────────────────

    #[test]
    fn test_require_arg_returns_value_when_present() {
        let tokens: Vec<String> = vec!["melisa".into(), "--run".into(), "mybox".into()];
        let result = require_arg(&tokens, 2, "melisa --run <n>");
        assert!(result.is_ok(), "require_arg must succeed when the token exists at the given index");
        assert_eq!(result.unwrap(), "mybox");
    }

    #[test]
    fn test_require_arg_returns_err_when_index_out_of_bounds() {
        let tokens: Vec<String> = vec!["melisa".into(), "--run".into()];
        let result = require_arg(&tokens, 2, "melisa --run <n>");
        assert!(
            result.is_err(),
            "require_arg must return Err when the required argument is missing"
        );
    }

    // ── exit/quit/cd/bash routing ────────────────────────────────────────────

    #[test]
    fn test_exit_token_is_first_class_command() {
        // Verify parse_command correctly tokenizes exit
        let (tokens, _) = parse_command("exit");
        assert_eq!(tokens[0], "exit");
    }

    #[test]
    fn test_cd_token_is_first_class_command() {
        let (tokens, _) = parse_command("cd /home/user");
        assert_eq!(tokens[0], "cd");
        assert_eq!(tokens[1], "/home/user");
    }

    #[test]
    fn test_arbitrary_command_is_not_melisa() {
        let (tokens, _) = parse_command("git status");
        assert_ne!(tokens[0], "melisa");
        assert_ne!(tokens[0], "exit");
        assert_ne!(tokens[0], "cd");
        // Verify it would fall into the bash passthrough branch
        assert_eq!(tokens[0], "git");
    }
}