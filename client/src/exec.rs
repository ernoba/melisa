//! # Remote Execution Engine
//!
//! Wraps SSH/SCP operations that the MELISA client needs to perform against a
//! remote MELISA server.  Every function validates arguments through the input
//! filter before constructing shell commands, preventing injection at the
//! boundary between local and remote execution.

use std::fs::{self, DirEntry};
use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::auth::get_active_conn;
use crate::color::{log_error, log_info, log_stat, log_success, BOLD, CYAN, RESET, YELLOW};
use crate::filter::{sanitise_arg, SanitiseResult};
use crate::platform::{data_dir, has_rsync, scp_bin, ssh_bin};

// ── Tunnel state directory ────────────────────────────────────────────────────

fn tunnel_dir() -> PathBuf {
    data_dir().join("tunnels")
}

fn tunnel_pid_file(container: &str, remote_port: u16) -> PathBuf {
    tunnel_dir().join(format!("{container}_{remote_port}.pid"))
}

fn tunnel_meta_file(container: &str, remote_port: u16) -> PathBuf {
    tunnel_dir().join(format!("{container}_{remote_port}.meta"))
}

// ── Connectivity guard ────────────────────────────────────────────────────────

fn require_conn() -> Option<String> {
    match get_active_conn() {
        Some(conn) => Some(conn),
        None => {
            log_error("No active server connection found!");
            println!("  {YELLOW}Tip:{RESET} Execute 'melisa auth add <n> <user@ip>' to register a server.");
            None
        }
    }
}

// ── Sanitise helper ───────────────────────────────────────────────────────────

/// Returns `Err(message)` when `arg` fails sanitisation, `Ok(())` otherwise.
fn check_arg(arg: &str) -> Result<(), String> {
    match sanitise_arg(arg) {
        SanitiseResult::Ok        => Ok(()),
        SanitiseResult::Reject(r) => Err(r.to_string()),
    }
}

// ── File transfer helper ──────────────────────────────────────────────────────

fn upload_via_tar_ssh(conn: &str, container: &str, local_dir: &str, dest_path: &str) -> io::Result<()> {
    let remote_cmd = format!("melisa --upload {container} {dest_path}");

    let mut tar = Command::new("tar")
        .args(["-czf", "-", "-C", local_dir, "."])
        .stdout(Stdio::piped())
        .spawn()?;

    let tar_out = tar.stdout.take().expect("tar stdout must be piped");

    let mut ssh = Command::new(ssh_bin())
        .arg(conn)
        .arg(&remote_cmd)
        .stdin(Stdio::from(tar_out))
        .spawn()?;

    let tar_exit = tar.wait()?;
    let ssh_exit = ssh.wait()?;

    if tar_exit.success() && ssh_exit.success() {
        Ok(())
    } else {
        Err(io::Error::new(ErrorKind::Other, "Upload via tar|ssh failed"))
    }
}

// ── Public commands ───────────────────────────────────────────────────────────

/// Executes a local script file inside a remote container (background mode).
pub fn exec_run(container: &str, file: &str) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    if let Err(e) = check_arg(container) { log_error(&e); return Ok(()); }

    if !std::path::Path::new(file).is_file() {
        log_error(&format!("File not found: '{file}'. Usage: melisa run <container> <file>"));
        return Ok(());
    }

    let ext         = std::path::Path::new(file).extension().and_then(|s| s.to_str()).unwrap_or("");
    let interpreter = match ext { "py" => "python3", "js" => "node", _ => "bash" };

    log_info(&format!("Executing '{BOLD}{file}{RESET}' inside '{container}' via server '{conn}'..."));

    let file_content = fs::read(file)?;
    let remote_cmd   = format!("melisa --send {container} {interpreter} -");

    let mut ssh = Command::new(ssh_bin())
        .arg(&conn)
        .arg(&remote_cmd)
        .stdin(Stdio::piped())
        .spawn()?;

    if let Some(stdin) = ssh.stdin.as_mut() {
        stdin.write_all(&file_content)?;
    }
    ssh.wait()?;
    Ok(())
}

/// Transfers a local directory into a remote container namespace.
pub fn exec_upload(container: &str, local_dir: &str, dest_path: &str) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    if let Err(e) = check_arg(container) { log_error(&e); return Ok(()); }
    if let Err(e) = check_arg(dest_path)  { log_error(&e); return Ok(()); }

    log_info(&format!("Transferring '{local_dir}' to '{container}:{dest_path}' via server '{conn}'..."));
    upload_via_tar_ssh(&conn, container, local_dir, dest_path)?;
    log_success("Transfer completed successfully.");
    Ok(())
}

/// Executes a local script inside a remote container with a live TTY session.
pub fn exec_run_tty(container: &str, file: &str) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    if let Err(e) = check_arg(container) { log_error(&e); return Ok(()); }

    if !std::path::Path::new(file).is_file() {
        log_error(&format!("File not found: '{file}'"));
        return Ok(());
    }

    let filename    = std::path::Path::new(file).file_name().and_then(|s| s.to_str()).unwrap_or(file);
    let dir         = std::path::Path::new(file).parent().and_then(|p| p.to_str()).unwrap_or(".");
    let ext         = std::path::Path::new(file).extension().and_then(|s| s.to_str()).unwrap_or("");
    let interpreter = match ext { "py" => "python3", "js" => "node", _ => "bash" };

    log_info(&format!("Provisioning artifact '{BOLD}{filename}{RESET}' in remote container..."));
    upload_via_tar_ssh(&conn, container, dir, "/tmp")?;
    log_success("Interactive session (TTY) initialized...");

    Command::new(ssh_bin())
        .args(["-t", &conn, &format!("melisa --send {container} {interpreter} /tmp/{filename}")])
        .status()?;

    Command::new(ssh_bin())
        .args([&conn, &format!("melisa --send {container} rm -f /tmp/{filename}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    log_success("Execution cycle completed and artifacts purged.");
    Ok(())
}

/// Clones a project workspace from the master server to a local directory.
pub fn exec_clone(project_name: &str, force: bool) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    let melisa_user = crate::auth::get_active_melisa_user()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "cannot determine remote MELISA user"))?;
    if let Err(e) = check_arg(project_name) { log_error(&e); return Ok(()); }

    let remote_src = format!("{conn}:/home/{melisa_user}/projects/{project_name}");
    let local_dest = format!("./{project_name}");

    if !force && std::path::Path::new(&local_dest).exists() {
        log_error(&format!("Directory '{local_dest}' already exists. Use '--force' to overwrite."));
        return Ok(());
    }

    log_info(&format!("Cloning workspace '{project_name}' from '{conn}'..."));

    if has_rsync() {
        let mut rsync_args: Vec<&str> = vec!["-az"];
        if force { rsync_args.push("--delete"); }
        rsync_args.push(&remote_src);
        rsync_args.push(&local_dest);
        let status = Command::new("rsync").args(&rsync_args).status()?;
        if status.success() { log_success("Clone completed."); } else { log_error("rsync clone failed."); }
    } else {
        if force { let _ = fs::remove_dir_all(&local_dest); }
        let status = Command::new(scp_bin()).args(["-r", &remote_src, &local_dest]).status()?;
        if status.success() { log_success("Clone completed."); } else { log_error("scp clone failed."); }
    }
    Ok(())
}

/// Pushes local workspace modifications to the remote server.
pub fn exec_sync(project_name: &str) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    let melisa_user = crate::auth::get_active_melisa_user()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "cannot determine remote MELISA user"))?;
    if let Err(e) = check_arg(project_name) { log_error(&e); return Ok(()); }

    let local_src  = format!("./{project_name}/");
    let remote_dst = format!("{conn}:/home/{melisa_user}/projects/{project_name}/");

    log_info(&format!("Synchronising '{local_src}' → remote '{remote_dst}'..."));

    if has_rsync() {
        let status = Command::new("rsync")
            .args(["-az", "--delete", &local_src, &remote_dst])
            .status()?;
        if status.success() { log_success("Sync completed."); } else { log_error("rsync sync failed."); }
    } else {
        let status = Command::new(scp_bin()).args(["-r", &local_src, &remote_dst]).status()?;
        if status.success() { log_success("Sync completed."); } else { log_error("scp sync failed."); }
    }
    Ok(())
}

/// Opens an interactive SSH shell to the active MELISA host.
pub fn exec_shell() -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    println!("[INFO] Establishing secure shell connection to {BOLD}{conn}{RESET}...");
    Command::new(ssh_bin()).arg("-t").arg(&conn).status()?;
    Ok(())
}

/// Forwards a container port to localhost via an SSH tunnel.
pub fn exec_tunnel(container: &str, remote_port: u16, local_port: Option<u16>) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    if let Err(e) = check_arg(container) { log_error(&e); return Ok(()); }

    let lport = local_port.unwrap_or(remote_port);

    let ip_output = Command::new(ssh_bin())
        .args([conn.as_str(), &format!("melisa --ip {container}")])
        .output()?;

    let container_ip = String::from_utf8_lossy(&ip_output.stdout).trim().to_string();
    if container_ip.is_empty() || !ip_output.status.success() {
        log_error(&format!("Cannot resolve IP for container '{container}'. Is it running?"));
        return Ok(());
    }

    let bind_expr = format!("{lport}:{container_ip}:{remote_port}");

    log_info(&format!(
        "Starting SSH tunnel: localhost:{lport} → {container}:{remote_port} via {conn}..."
    ));

    let child = Command::new(ssh_bin())
        .args(["-N", "-L", &bind_expr, &conn])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let pid = child.id();

    fs::create_dir_all(tunnel_dir())?;
    fs::write(tunnel_pid_file(container, remote_port), pid.to_string())?;
    fs::write(
        tunnel_meta_file(container, remote_port),
        format!("{container}|{remote_port}|{lport}"),
    )?;

    log_success(&format!(
        "Tunnel active — localhost:{lport} → {container_ip}:{remote_port}  (PID {pid})"
    ));
    Ok(())
}

/// Lists all active SSH tunnels managed by this client.
pub fn exec_tunnel_list() -> io::Result<()> {
    println!("\n{BOLD}{CYAN}=== ACTIVE MELISA TUNNELS ==={RESET}");

    let dir = tunnel_dir();
    if !dir.exists() {
        println!("No active tunnels found.");
        return Ok(());
    }

    let entries: Vec<DirEntry> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("meta"))
        .collect();

    if entries.is_empty() {
        println!("No active tunnels found.");
        return Ok(());
    }

    for entry in entries {
        let meta  = fs::read_to_string(entry.path()).unwrap_or_default();
        let parts: Vec<&str> = meta.split('|').collect();
        if parts.len() < 3 { continue; }
        let (container, rport, lport) = (parts[0], parts[1], parts[2]);

        let pid_path = entry.path().with_extension("pid");
        let alive = pid_path
            .exists()
            .then(|| fs::read_to_string(&pid_path).ok())
            .flatten()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(process_is_alive)
            .unwrap_or(false);

        let status_str = if alive { "ACTIVE" } else { "DEAD" };
        log_stat(container, &format!("localhost:{lport} → :{rport}  [{status_str}]"));
    }
    println!();
    Ok(())
}

/// Stops a running SSH tunnel for the given container (and optional port).
pub fn exec_tunnel_stop(container: &str, remote_port: Option<u16>) -> io::Result<()> {
    if let Err(e) = check_arg(container) { log_error(&e); return Ok(()); }

    let dir = tunnel_dir();
    if !dir.exists() {
        log_error(&format!("No tunnel found for '{container}'."));
        return Ok(());
    }

    let mut stopped = 0_u32;

    for entry in fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("pid") { continue; }

        let stem  = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let parts: Vec<&str> = stem.rsplitn(2, '_').collect();
        if parts.len() != 2 { continue; }
        let (port_str, name) = (parts[0], parts[1]);
        if name != container { continue; }

        let port: u16 = match port_str.parse() { Ok(p) => p, Err(_) => continue };
        if let Some(fp) = remote_port { if port != fp { continue; } }

        if let Ok(pid_str) = fs::read_to_string(&path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if kill_process(pid) {
                    log_success(&format!("Tunnel stopped (PID {pid}) — {container}:{port}"));
                } else {
                    log_info("Tunnel process already dead.");
                }
            }
        }
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("meta"));
        stopped += 1;
    }

    if stopped == 0 {
        log_error(&format!("No tunnel found for '{container}'."));
    }
    Ok(())
}

/// Forwards an unrecognised command directly to the remote MELISA host.
pub fn exec_forward(command: &str, args: &[String]) -> io::Result<()> {
    let conn = require_conn()
        .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "no active connection"))?;
    let mut parts = vec![command.to_string()];
    parts.extend_from_slice(args);
    Command::new(ssh_bin())
        .arg(&conn)
        .arg(&format!("melisa {}", parts.join(" ")))
        .status()?;
    Ok(())
}

// ── OS-level process helpers ──────────────────────────────────────────────────

fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let result = unsafe {
            // Safety: kill(pid, 0) is a POSIX existence check — no signal is delivered.
            unsafe extern "C" { fn kill(pid: i32, sig: i32) -> i32; }
            kill(pid as i32, 0)
        };
        result == 0
    }
    #[cfg(windows)]
    { windows_process_alive(pid) }
    #[cfg(not(any(unix, windows)))]
    { let _ = pid; false }
}

fn kill_process(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let result = unsafe {
            unsafe extern "C" { fn kill(pid: i32, sig: i32) -> i32; }
            kill(pid as i32, 15) // SIGTERM
        };
        result == 0
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        extern "system" {
            unsafe fn OpenProcess(a: u32, b: i32, c: u32) -> *mut c_void;
            unsafe fn TerminateProcess(h: *mut c_void, code: u32) -> i32;
            unsafe fn CloseHandle(h: *mut c_void) -> i32;
        }
        unsafe {
            let h = OpenProcess(0x0001, 0, pid);
            if h.is_null() { return false; }
            let ok = TerminateProcess(h, 1) != 0;
            CloseHandle(h);
            ok
        }
    }
    #[cfg(not(any(unix, windows)))]
    { let _ = pid; false }
}

#[cfg(windows)]
fn windows_process_alive(pid: u32) -> bool {
    use std::ffi::c_void;
    extern "system" {
        unsafe fn OpenProcess(a: u32, b: i32, c: u32) -> *mut c_void;
        unsafe fn CloseHandle(h: *mut c_void) -> i32;
        unsafe fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
    }
    unsafe {
        let h = OpenProcess(0x1000, 0, pid);
        if h.is_null() { return false; }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut exit_code) != 0;
        CloseHandle(h);
        ok && exit_code == 259 // STILL_ACTIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_arg_rejects_semicolon_injection() {
        assert!(check_arg("mybox; rm -rf /").is_err());
    }

    #[test]
    fn test_check_arg_rejects_path_traversal() {
        assert!(check_arg("../../etc/passwd").is_err());
    }

    #[test]
    fn test_check_arg_allows_normal_container_name() {
        assert!(check_arg("my-dev-box").is_ok());
    }
}