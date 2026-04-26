/// ============================================================================
/// src/mproto/dispatcher.rs
///
/// Command dispatcher - translates protocol requests into SSH operations
/// This is the core execution bridge between mshell CLI and remote server
/// ============================================================================

use super::protocol::{CommandRequest, CommandResponse, CommandType};
use super::connection;

/// Command dispatcher - routes requests through appropriate handlers
pub struct CommandDispatcher;

impl CommandDispatcher {
    /// Execute a command request
    pub fn execute(request: &CommandRequest) -> CommandResponse {
        // Validate the request first
        if let Err(e) = request.validate() {
            return CommandResponse::rejected(e);
        }

        // Check connection requirement
        if request.cmd_type != CommandType::Auth && request.cmd_type != CommandType::Query {
            if let Err(e) = connection::require_connection() {
                return CommandResponse::error(e);
            }
        }

        // Dispatch to specific handler
        match request.cmd_type {
            CommandType::Execute => Self::handle_execute(request),
            CommandType::Upload => Self::handle_upload(request),
            CommandType::Download => Self::handle_download(request),
            CommandType::Terminal => Self::handle_terminal(request),
            CommandType::Clone => Self::handle_clone(request),
            CommandType::Sync => Self::handle_sync(request),
            CommandType::Tunnel => Self::handle_tunnel(request),
            CommandType::Auth => Self::handle_auth(request),
            CommandType::Shell => Self::handle_shell(request),
            CommandType::Query => Self::handle_query(request),
        }
    }

    /// Handle execute command
    fn handle_execute(request: &CommandRequest) -> CommandResponse {
        let target = request.target.as_ref().unwrap();
        
        if request.args.is_empty() {
            return CommandResponse::rejected("No command specified");
        }

        let cmd = &request.args[0];
        let cmd_args: Vec<&str> = request.args[1..].iter().map(|s| s.as_str()).collect();
        
        match super::exec::exec_command(
            target,
            cmd,
            &cmd_args,
            request.options.verbose,
        ) {
            Ok(output) => {
                CommandResponse::success("Command executed successfully")
                    .with_stdout(output)
                    .with_exit_code(0)
            }
            Err(e) => CommandResponse::error(format!("Execution failed: {}", e)),
        }
    }

    /// Handle upload command
    fn handle_upload(request: &CommandRequest) -> CommandResponse {
        let target = request.target.as_ref().unwrap();
        
        if request.args.len() < 2 {
            return CommandResponse::rejected("Upload requires source and destination");
        }

        let src = &request.args[0];
        let dst = &request.args[1];

        match super::exec::exec_upload(target, src, dst) {
            Ok(()) => CommandResponse::success(format!("Upload completed: {} → {}:{}", src, target, dst)),
            Err(e) => CommandResponse::error(format!("Upload failed: {}", e)),
        }
    }

    /// Handle download command
    fn handle_download(request: &CommandRequest) -> CommandResponse {
        let target = request.target.as_ref().unwrap();
        
        if request.args.len() < 2 {
            return CommandResponse::rejected("Download requires source and destination");
        }

        let src = &request.args[0];
        let dst = &request.args[1];

        match super::exec::exec_download(target, src, dst) {
            Ok(()) => CommandResponse::success(format!("Download completed: {}:{} → {}", target, src, dst)),
            Err(e) => CommandResponse::error(format!("Download failed: {}", e)),
        }
    }

    /// Handle terminal command
    fn handle_terminal(request: &CommandRequest) -> CommandResponse {
        let target = request.target.as_ref().unwrap();
        
        if request.args.is_empty() {
            return CommandResponse::rejected("Terminal requires a file to execute");
        }

        let file = &request.args[0];

        match super::exec::exec_run_tty(target, file) {
            Ok(()) => CommandResponse::success("Terminal session completed"),
            Err(e) => CommandResponse::error(format!("Terminal session failed: {}", e)),
        }
    }

    /// Handle clone command
    fn handle_clone(request: &CommandRequest) -> CommandResponse {
        if request.args.is_empty() {
            return CommandResponse::rejected("Clone requires project name");
        }

        let project = &request.args[0];
        let force = request.options.force;

        match super::exec::exec_clone(project, force) {
            Ok(()) => CommandResponse::success(format!("Project '{}' cloned successfully", project)),
            Err(e) => CommandResponse::error(format!("Clone failed: {}", e)),
        }
    }

    /// Handle sync command
    fn handle_sync(request: &CommandRequest) -> CommandResponse {
        if request.args.is_empty() {
            return CommandResponse::rejected("Sync requires project name");
        }

        let project = &request.args[0];

        match super::exec::exec_sync(project) {
            Ok(()) => CommandResponse::success(format!("Project '{}' synced successfully", project)),
            Err(e) => CommandResponse::error(format!("Sync failed: {}", e)),
        }
    }

    /// Handle tunnel command
    fn handle_tunnel(request: &CommandRequest) -> CommandResponse {
        let target = request.target.as_ref().unwrap();
        
        if request.args.is_empty() {
            return CommandResponse::rejected("Tunnel requires remote port");
        }

        let remote_port: u16 = match request.args[0].parse() {
            Ok(p) => p,
            Err(_) => return CommandResponse::rejected("Invalid port number"),
        };

        let local_port = if request.args.len() > 1 {
            request.args[1].parse().ok()
        } else {
            None
        };

        match super::exec::exec_tunnel(target, remote_port, local_port) {
            Ok(()) => CommandResponse::success("Tunnel established"),
            Err(e) => CommandResponse::error(format!("Tunnel failed: {}", e)),
        }
    }

    /// Handle auth command
    fn handle_auth(_request: &CommandRequest) -> CommandResponse {
        // Auth is handled separately in mshell, not through dispatcher
        CommandResponse::error("Auth commands should be handled by CLI directly")
    }

    /// Handle shell command
    fn handle_shell(_request: &CommandRequest) -> CommandResponse {
        match super::exec::exec_shell() {
            Ok(()) => CommandResponse::success("Shell session completed"),
            Err(e) => CommandResponse::error(format!("Shell failed: {}", e)),
        }
    }

    /// Handle query command
    fn handle_query(request: &CommandRequest) -> CommandResponse {
        if request.args.is_empty() {
            return CommandResponse::rejected("Query requires a query type");
        }

        let query_type = &request.args[0];
        
        match query_type.as_str() {
            "status" => {
                match connection::get_state() {
                    Ok(state) => CommandResponse::success(format!("Connection state: {:?}", state)),
                    Err(e) => CommandResponse::error(e),
                }
            }
            "connection" => {
                match connection::get_connection() {
                    Ok(Some(conn)) => CommandResponse::success(format!("Active connection: {}", conn)),
                    Ok(None) => CommandResponse::success("No active connection"),
                    Err(e) => CommandResponse::error(e),
                }
            }
            _ => CommandResponse::error(format!("Unknown query type: {}", query_type)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation() {
        let req = CommandRequest::new(CommandType::Execute);
        let resp = CommandDispatcher::execute(&req);
        assert_eq!(resp.status, crate::mproto::protocol::CommandStatus::Rejected);
    }

    #[test]
    fn test_connection_requirement() {
        let _ = connection::disconnect();
        let req = CommandRequest::new(CommandType::Execute)
            .target("container")
            .arg("whoami");
        
        let resp = CommandDispatcher::execute(&req);
        assert_eq!(resp.status, crate::mproto::protocol::CommandStatus::Error);
    }
}
