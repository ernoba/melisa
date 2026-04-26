/// ============================================================================
/// src/mshell/handler.rs
///
/// Command handler for mshell - parses user input and dispatches to mproto
/// Bridges the interactive shell with the protocol execution layer
/// ============================================================================

use crate::mproto::{CommandDispatcher, CommandRequest, CommandType, CommandResponse};
use crate::mproto::protocol::CommandStatus;

/// Parse and execute a command from user input
pub fn handle_command(input: &str) -> CommandResponse {
    let parts: Vec<&str> = input.trim().split_whitespace().collect();
    
    if parts.is_empty() {
        return CommandResponse::rejected("Empty command");
    }

    let cmd = parts[0];
    let args: Vec<&str> = if parts.len() > 1 { parts[1..].to_vec() } else { vec![] };

    match cmd {
        // ── Execution Commands ────────────────────────────────────────────────
        "exec" => handle_exec(&args),
        "run" => handle_run(&args),
        "shell" => handle_shell(&args),
        "tty" => handle_tty(&args),

        // ── File Operations ───────────────────────────────────────────────────
        "upload" => handle_upload(&args),
        "download" => handle_download(&args),

        // ── Project Management ────────────────────────────────────────────────
        "clone" => handle_clone(&args),
        "sync" => handle_sync(&args),

        // ── Network ───────────────────────────────────────────────────────────
        "tunnel" => handle_tunnel(&args),

        // ── Query Commands ────────────────────────────────────────────────────
        "status" => handle_status(&args),
        "info" => handle_info(&args),

        // ── Unknown Command ───────────────────────────────────────────────────
        _ => CommandResponse::rejected(format!("Unknown command: '{}'", cmd)),
    }
}

/// Execute command in container
/// Usage: exec <container> <command> [args...]
fn handle_exec(args: &[&str]) -> CommandResponse {
    if args.len() < 2 {
        return CommandResponse::rejected("exec requires: <container> <command> [args...]");
    }

    let container = args[0].to_string();
    let command = args[1].to_string();
    let cmd_args: Vec<String> = args[2..].iter().map(|s| s.to_string()).collect();

    let req = CommandRequest::new(CommandType::Execute)
        .target(container)
        .arg(command)
        .args(cmd_args);

    CommandDispatcher::execute(&req)
}

/// Run file in container
/// Usage: run <container> <file>
fn handle_run(args: &[&str]) -> CommandResponse {
    if args.len() < 2 {
        return CommandResponse::rejected("run requires: <container> <file>");
    }

    let container = args[0].to_string();
    let file = args[1].to_string();

    let req = CommandRequest::new(CommandType::Execute)
        .target(container)
        .arg("run")
        .arg(file);

    CommandDispatcher::execute(&req)
}

/// Interactive terminal in container
/// Usage: tty <container> <file>
fn handle_tty(args: &[&str]) -> CommandResponse {
    if args.len() < 2 {
        return CommandResponse::rejected("tty requires: <container> <file>");
    }

    let container = args[0].to_string();
    let file = args[1].to_string();

    let req = CommandRequest::new(CommandType::Terminal)
        .target(container)
        .arg(file)
        .interactive(true);

    CommandDispatcher::execute(&req)
}

/// Open shell connection
/// Usage: shell
fn handle_shell(_args: &[&str]) -> CommandResponse {
    let req = CommandRequest::new(CommandType::Shell);
    CommandDispatcher::execute(&req)
}

/// Upload files to container
/// Usage: upload <container> <local_path> <remote_path>
fn handle_upload(args: &[&str]) -> CommandResponse {
    if args.len() < 3 {
        return CommandResponse::rejected("upload requires: <container> <local_path> <remote_path>");
    }

    let container = args[0].to_string();
    let local = args[1].to_string();
    let remote = args[2].to_string();

    let req = CommandRequest::new(CommandType::Upload)
        .target(container)
        .arg(local)
        .arg(remote);

    CommandDispatcher::execute(&req)
}

/// Download files from container
/// Usage: download <container> <remote_path> <local_path>
fn handle_download(args: &[&str]) -> CommandResponse {
    if args.len() < 3 {
        return CommandResponse::rejected("download requires: <container> <remote_path> <local_path>");
    }

    let container = args[0].to_string();
    let remote = args[1].to_string();
    let local = args[2].to_string();

    let req = CommandRequest::new(CommandType::Download)
        .target(container)
        .arg(remote)
        .arg(local);

    CommandDispatcher::execute(&req)
}

/// Clone project from server
/// Usage: clone <project_name> [--force]
fn handle_clone(args: &[&str]) -> CommandResponse {
    if args.is_empty() {
        return CommandResponse::rejected("clone requires: <project_name> [--force]");
    }

    let project = args[0].to_string();
    let force = args.iter().any(|a| *a == "--force");

    let req = CommandRequest::new(CommandType::Clone)
        .arg(project)
        .force(force);

    CommandDispatcher::execute(&req)
}

/// Sync project to server
/// Usage: sync <project_name>
fn handle_sync(args: &[&str]) -> CommandResponse {
    if args.is_empty() {
        return CommandResponse::rejected("sync requires: <project_name>");
    }

    let project = args[0].to_string();

    let req = CommandRequest::new(CommandType::Sync)
        .arg(project);

    CommandDispatcher::execute(&req)
}

/// Create tunnel
/// Usage: tunnel <container> <remote_port> [local_port]
fn handle_tunnel(args: &[&str]) -> CommandResponse {
    if args.len() < 2 {
        return CommandResponse::rejected("tunnel requires: <container> <remote_port> [local_port]");
    }

    let container = args[0].to_string();
    let remote_port = args[1].to_string();
    let local_port = if args.len() > 2 {
        Some(args[2].to_string())
    } else {
        None
    };

    let mut req = CommandRequest::new(CommandType::Tunnel)
        .target(container)
        .arg(remote_port);

    if let Some(port) = local_port {
        req = req.arg(port);
    }

    CommandDispatcher::execute(&req)
}

/// Show connection status
/// Usage: status
fn handle_status(_args: &[&str]) -> CommandResponse {
    let req = CommandRequest::new(CommandType::Query)
        .arg("status");

    CommandDispatcher::execute(&req)
}

/// Show connection info
/// Usage: info
fn handle_info(_args: &[&str]) -> CommandResponse {
    let req = CommandRequest::new(CommandType::Query)
        .arg("connection");

    CommandDispatcher::execute(&req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_command_parsing() {
        let resp = handle_exec(&["container", "whoami"]);
        // Should create request but fail due to no connection
        assert_eq!(resp.status, CommandStatus::Error);
    }

    #[test]
    fn test_clone_command_parsing() {
        let resp = handle_clone(&[]);
        assert_eq!(resp.status, CommandStatus::Rejected);
    }
}
