/// ============================================================================
/// src/mproto/connection.rs
///
/// Manages connection state and active server connection for remote execution
/// ============================================================================

use std::sync::{Arc, Mutex, OnceLock};
use super::protocol::ConnectionState;

static CONNECTION_STATE: OnceLock<Arc<Mutex<ConnectionState>>> = OnceLock::new();
static ACTIVE_CONNECTION: OnceLock<Arc<Mutex<Option<String>>>> = OnceLock::new();
static ACTIVE_USER: OnceLock<Arc<Mutex<Option<String>>>> = OnceLock::new();

/// Get or initialize the global connection state
fn get_connection_state() -> Arc<Mutex<ConnectionState>> {
    CONNECTION_STATE
        .get_or_init(|| Arc::new(Mutex::new(ConnectionState::Disconnected)))
        .clone()
}

/// Get or initialize the global active connection string
fn get_active_connection() -> Arc<Mutex<Option<String>>> {
    ACTIVE_CONNECTION
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Get or initialize the global active MELISA user
fn get_active_user() -> Arc<Mutex<Option<String>>> {
    ACTIVE_USER
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Set the active server connection (format: "user@host")
pub fn set_connection(conn: String) -> Result<(), String> {
    if conn.is_empty() {
        return Err("Connection string cannot be empty".to_string());
    }
    
    let active_conn = get_active_connection();
    let mut guard = active_conn.lock().map_err(|e| format!("Lock error: {}", e))?;
    *guard = Some(conn.clone());
    
    let state = get_connection_state();
    let mut state_guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
    *state_guard = ConnectionState::Connected;
    
    Ok(())
}

/// Set the active MELISA user on the server
pub fn set_active_user(user: String) -> Result<(), String> {
    if user.is_empty() {
        return Err("User name cannot be empty".to_string());
    }
    
    let active_user = get_active_user();
    let mut guard = active_user.lock().map_err(|e| format!("Lock error: {}", e))?;
    *guard = Some(user);
    
    Ok(())
}

/// Get the current active connection string
pub fn get_connection() -> Result<Option<String>, String> {
    let active_conn = get_active_connection();
    let guard = active_conn.lock().map_err(|e| format!("Lock error: {}", e))?;
    Ok(guard.clone())
}

/// Get the current active MELISA user
pub fn get_user() -> Result<Option<String>, String> {
    let active_user = get_active_user();
    let guard = active_user.lock().map_err(|e| format!("Lock error: {}", e))?;
    Ok(guard.clone())
}

/// Check if connection is active
pub fn is_connected() -> Result<bool, String> {
    let state = get_connection_state();
    let guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
    Ok(*guard == ConnectionState::Connected)
}

/// Get current connection state
pub fn get_state() -> Result<ConnectionState, String> {
    let state = get_connection_state();
    let guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
    Ok(guard.clone())
}

/// Mark connection as failed
pub fn set_failed(reason: String) -> Result<(), String> {
    let state = get_connection_state();
    let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
    *guard = ConnectionState::Failed(reason);
    Ok(())
}

/// Clear connection state (disconnect)
pub fn disconnect() -> Result<(), String> {
    let state = get_connection_state();
    let mut state_guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
    *state_guard = ConnectionState::Disconnected;
    
    let active_conn = get_active_connection();
    let mut conn_guard = active_conn.lock().map_err(|e| format!("Lock error: {}", e))?;
    *conn_guard = None;
    
    let active_user = get_active_user();
    let mut user_guard = active_user.lock().map_err(|e| format!("Lock error: {}", e))?;
    *user_guard = None;
    
    Ok(())
}

/// Require an active connection; returns error if none
pub fn require_connection() -> Result<String, String> {
    let conn = get_connection()?;
    conn.ok_or_else(|| "No active server connection. Use 'melisa auth add' to connect.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_lifecycle() {
        let _ = disconnect();
        
        assert!(matches!(get_state().ok(), Some(ConnectionState::Disconnected)));
        assert_eq!(get_connection().ok().flatten(), None);
        
        let _ = set_connection("user@example.com".to_string());
        assert!(matches!(get_state().ok(), Some(ConnectionState::Connected)));
        assert_eq!(get_connection().ok().flatten(), Some("user@example.com".to_string()));
        
        let _ = disconnect();
        assert!(matches!(get_state().ok(), Some(ConnectionState::Disconnected)));
    }
}
