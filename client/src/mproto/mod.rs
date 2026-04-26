pub mod exec;
pub mod filter;
pub mod protocol;
pub mod connection;
pub mod dispatcher;

// Re-export commonly used protocol types
pub use protocol::{CommandRequest, CommandResponse, CommandType, CommandStatus, CommandOptions};
pub use dispatcher::CommandDispatcher;
pub use connection::{set_connection, get_connection, require_connection, is_connected, get_state};