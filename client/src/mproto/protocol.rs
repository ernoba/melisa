/// ============================================================================
/// src/mproto/protocol.rs
///
/// Standard communication protocol between mshell (CLI) and mproto (execution)
/// Defines request/response types, status codes, and error handling.
/// ============================================================================

use std::fmt;

// ── Status Codes ────────────────────────────────────────────────────────────

/// Command execution status codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    /// Command executed successfully
    Success = 0,
    /// Command failed with error
    Error = 1,
    /// Command execution was rejected before running
    Rejected = 2,
    /// Connection to server failed
    ConnectionError = 3,
    /// Timeout during execution
    Timeout = 4,
    /// Invalid command format or arguments
    InvalidCommand = 5,
}

impl fmt::Display for CommandStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => write!(f, "Success"),
            Self::Error => write!(f, "Error"),
            Self::Rejected => write!(f, "Rejected"),
            Self::ConnectionError => write!(f, "Connection Error"),
            Self::Timeout => write!(f, "Timeout"),
            Self::InvalidCommand => write!(f, "Invalid Command"),
        }
    }
}

// ── Command Types ──────────────────────────────────────────────────────────

/// Command type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandType {
    /// Execute code/script in container
    Execute,
    /// File upload to container
    Upload,
    /// File download from container
    Download,
    /// Interactive terminal session
    Terminal,
    /// Project clone from server
    Clone,
    /// Project sync to server
    Sync,
    /// Create/manage tunnels
    Tunnel,
    /// Authentication operations
    Auth,
    /// Shell connection
    Shell,
    /// Query server information
    Query,
}

impl fmt::Display for CommandType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execute => write!(f, "Execute"),
            Self::Upload => write!(f, "Upload"),
            Self::Download => write!(f, "Download"),
            Self::Terminal => write!(f, "Terminal"),
            Self::Clone => write!(f, "Clone"),
            Self::Sync => write!(f, "Sync"),
            Self::Tunnel => write!(f, "Tunnel"),
            Self::Auth => write!(f, "Auth"),
            Self::Shell => write!(f, "Shell"),
            Self::Query => write!(f, "Query"),
        }
    }
}

// ── Command Request ────────────────────────────────────────────────────────

/// Request to execute a command through mproto
#[derive(Debug, Clone)]
pub struct CommandRequest {
    /// Type of command
    pub cmd_type: CommandType,
    /// Container/target identifier (if applicable)
    pub target: Option<String>,
    /// Primary argument(s)
    pub args: Vec<String>,
    /// Optional keyword arguments
    pub options: CommandOptions,
}

/// Command execution options
#[derive(Debug, Clone, Default)]
pub struct CommandOptions {
    /// Force execution (bypass safety checks)
    pub force: bool,
    /// Timeout in seconds
    pub timeout: Option<u64>,
    /// Use interactive TTY
    pub interactive: bool,
    /// Verbose output
    pub verbose: bool,
}

impl CommandRequest {
    /// Create a new command request
    pub fn new(cmd_type: CommandType) -> Self {
        Self {
            cmd_type,
            target: None,
            args: Vec::new(),
            options: CommandOptions::default(),
        }
    }

    /// Set the target container/resource
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Add argument
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments
    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args.extend(args);
        self
    }

    /// Set force option
    pub fn force(mut self, force: bool) -> Self {
        self.options.force = force;
        self
    }

    /// Set timeout
    pub fn timeout(mut self, seconds: u64) -> Self {
        self.options.timeout = Some(seconds);
        self
    }

    /// Set interactive mode
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.options.interactive = interactive;
        self
    }

    /// Set verbose mode
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.options.verbose = verbose;
        self
    }

    /// Validate command consistency
    pub fn validate(&self) -> Result<(), String> {
        match self.cmd_type {
            CommandType::Execute | CommandType::Terminal => {
                if self.target.is_none() {
                    return Err("Execute/Terminal requires target container".to_string());
                }
                if self.args.is_empty() {
                    return Err("Execute/Terminal requires at least one argument".to_string());
                }
            }
            CommandType::Upload | CommandType::Download => {
                if self.target.is_none() {
                    return Err("Upload/Download requires target container".to_string());
                }
                if self.args.len() < 2 {
                    return Err("Upload/Download requires source and destination paths".to_string());
                }
            }
            CommandType::Clone | CommandType::Sync => {
                if self.args.is_empty() {
                    return Err("Clone/Sync requires project name".to_string());
                }
            }
            CommandType::Tunnel => {
                if self.target.is_none() {
                    return Err("Tunnel requires target container".to_string());
                }
                if self.args.is_empty() {
                    return Err("Tunnel requires port number".to_string());
                }
            }
            CommandType::Auth | CommandType::Shell | CommandType::Query => {
                // These are generally permissive
            }
        }
        Ok(())
    }
}

// ── Command Response ───────────────────────────────────────────────────────

/// Response from command execution
#[derive(Debug, Clone)]
pub struct CommandResponse {
    /// Execution status
    pub status: CommandStatus,
    /// Human-readable message
    pub message: String,
    /// Standard output
    pub stdout: Option<String>,
    /// Standard error
    pub stderr: Option<String>,
    /// Exit code (if applicable)
    pub exit_code: Option<i32>,
}

impl CommandResponse {
    /// Create a successful response
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Success,
            message: message.into(),
            stdout: None,
            stderr: None,
            exit_code: Some(0),
        }
    }

    /// Create an error response
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Error,
            message: message.into(),
            stdout: None,
            stderr: None,
            exit_code: Some(1),
        }
    }

    /// Create a rejected response
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Rejected,
            message: reason.into(),
            stdout: None,
            stderr: None,
            exit_code: Some(2),
        }
    }

    /// Add stdout
    pub fn with_stdout(mut self, stdout: impl Into<String>) -> Self {
        self.stdout = Some(stdout.into());
        self
    }

    /// Add stderr
    pub fn with_stderr(mut self, stderr: impl Into<String>) -> Self {
        self.stderr = Some(stderr.into());
        self
    }

    /// Add exit code
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code = Some(code);
        self
    }

    /// Check if response indicates success
    pub fn is_success(&self) -> bool {
        self.status == CommandStatus::Success
    }

    /// Check if response indicates error
    pub fn is_error(&self) -> bool {
        self.status == CommandStatus::Error
    }
}

// ── Connection State ───────────────────────────────────────────────────────

/// Connection state to remote MELISA server
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected
    Disconnected,
    /// Connection established
    Connected,
    /// Connection error
    Failed(String),
}

// ── Error Types ────────────────────────────────────────────────────────────

/// Protocol-level errors
#[derive(Debug)]
pub enum ProtocolError {
    /// Invalid command format
    InvalidCommand(String),
    /// Validation failed
    ValidationFailed(String),
    /// Connection lost
    ConnectionLost,
    /// Timeout occurred
    Timeout,
    /// IO error
    IoError(String),
    /// Serialization error
    SerializationError(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommand(msg) => write!(f, "Invalid command: {}", msg),
            Self::ValidationFailed(msg) => write!(f, "Validation failed: {}", msg),
            Self::ConnectionLost => write!(f, "Connection to server lost"),
            Self::Timeout => write!(f, "Command execution timeout"),
            Self::IoError(msg) => write!(f, "IO error: {}", msg),
            Self::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
        }
    }
}

impl std::error::Error for ProtocolError {}
