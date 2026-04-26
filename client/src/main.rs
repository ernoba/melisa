//! # MELISA Client — Cross-Platform Remote Management CLI
//!
//! Entry point dan command router. Berjalan native di Linux, macOS, dan Windows.

mod platform;
mod mproto;
mod mshell;

use std::process;
use mshell::color::{log_error, BOLD, CYAN, RED, RESET, YELLOW};
use platform::verify_dependencies;
use mshell::auth;
use mproto::exec;
use mproto::filter;

fn main() {
    #[cfg(target_os = "windows")]
    enable_ansi_on_windows();

    if let Err(msg) = verify_dependencies() {
        eprintln!("{RED}[FATAL ERROR]{RESET} {msg}");
        process::exit(1);
    }

    if let Err(e) = auth::init_auth() {
        log_error(&format!("Failed to initialise MELISA configuration directory: {e}"));
        process::exit(1);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_help();
        process::exit(1);
    }

    let command = args[0].as_str();
    let rest    = &args[1..];

    match command {
        // ── Authentication ────────────────────────────────────────────────────
        "auth" => {
            let sub      = rest.first().map(|s| s.as_str()).unwrap_or("");
            let sub_args = if rest.is_empty() { &[][..] } else { &rest[1..] };

            match sub {
                "add" => {
                    if sub_args.len() < 2 {
                        eprintln!("{RED}[ERROR]{RESET} Usage: melisa auth add <n> <user@ip>");
                        process::exit(1);
                    }
                    if let Err(e) = auth::auth_add(&sub_args[0], &sub_args[1]) {
                        log_error(&e.to_string());
                        process::exit(1);
                    }
                }
                "switch" => {
                    if sub_args.is_empty() {
                        eprintln!("{RED}[ERROR]{RESET} Usage: melisa auth switch <n>");
                        process::exit(1);
                    }
                    if let Err(e) = auth::auth_switch(&sub_args[0]) {
                        log_error(&e.to_string());
                        process::exit(1);
                    }
                }
                "list" => {
                    if let Err(e) = auth::auth_list() { log_error(&e.to_string()); }
                }
                "remove" => {
                    if sub_args.is_empty() {
                        eprintln!("{RED}[ERROR]{RESET} Usage: melisa auth remove <n>");
                        process::exit(1);
                    }
                    if let Err(e) = auth::auth_remove(&sub_args[0]) {
                        log_error(&e.to_string());
                        process::exit(1);
                    }
                }
                _ => {
                    eprintln!("{RED}[ERROR]{RESET} Unknown auth sub-command. Valid: [add | switch | list | remove]");
                    process::exit(1);
                }
            }
        }

        // ── Project synchronisation ───────────────────────────────────────────
        "clone" => {
            if rest.is_empty() {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa clone <project_name> [--force]");
                process::exit(1);
            }
            let force = rest.iter().any(|a| a == "--force");
            if let Err(e) = exec::exec_clone(&rest[0], force) { log_error(&e.to_string()); }
        }
        "sync" => {
            let name = rest.first().map(|s| s.as_str()).unwrap_or(".");
            if let Err(e) = exec::exec_sync(name) { log_error(&e.to_string()); }
        }
        "get" => {
            if rest.is_empty() {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa get <project_name> [--force]");
                process::exit(1);
            }
            let force = rest.iter().any(|a| a == "--force");
            if let Err(e) = exec::exec_clone(&rest[0], force) { log_error(&e.to_string()); }
        }

        // ── Remote execution ──────────────────────────────────────────────────
        "run" => {
            if rest.len() < 2 {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa run <container_name> <local_file_path>");
                process::exit(1);
            }
            if let Err(e) = exec::exec_run(&rest[0], &rest[1]) { log_error(&e.to_string()); }
        }
        "run-tty" => {
            if rest.len() < 2 {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa run-tty <container_name> <local_file_path>");
                process::exit(1);
            }
            if let Err(e) = exec::exec_run_tty(&rest[0], &rest[1]) { log_error(&e.to_string()); }
        }
        "upload" => {
            if rest.len() < 3 {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa upload <container_name> <local_dir> <destination_path>");
                process::exit(1);
            }
            if let Err(e) = exec::exec_upload(&rest[0], &rest[1], &rest[2]) {
                log_error(&e.to_string());
            }
        }
        "shell" => {
            if let Err(e) = exec::exec_shell() { log_error(&e.to_string()); }
        }

        // ── Tunnel management ─────────────────────────────────────────────────
        "tunnel" => {
            if rest.len() < 2 {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa tunnel <container> <remote_port> [local_port]");
                println!("  Example: melisa tunnel myapp 3000");
                process::exit(1);
            }
            let remote_port = rest[1].parse::<u16>().unwrap_or_else(|_| {
                eprintln!("{RED}[ERROR]{RESET} Invalid port number: '{}'", rest[1]);
                process::exit(1);
            });
            let local_port = rest.get(2).and_then(|p| p.parse::<u16>().ok());
            if let Err(e) = exec::exec_tunnel(&rest[0], remote_port, local_port) {
                log_error(&e.to_string());
            }
        }
        "tunnel-list" => {
            if let Err(e) = exec::exec_tunnel_list() { log_error(&e.to_string()); }
        }
        "tunnel-stop" => {
            if rest.is_empty() {
                eprintln!("{RED}[ERROR]{RESET} Usage: melisa tunnel-stop <container> [remote_port]");
                process::exit(1);
            }
            let remote_port = rest.get(1).and_then(|p| p.parse::<u16>().ok());
            if let Err(e) = exec::exec_tunnel_stop(&rest[0], remote_port) {
                log_error(&e.to_string());
            }
        }

        // ── Interactive Shell ─────────────────────────────────────────────────
        "interactive" | "i" => {
            println!("{BOLD}{CYAN}Entering MELISA interactive shell...{RESET}");
            // Note: Interactive shell would be launched here
            // This would require async runtime setup
            println!("{YELLOW}Interactive mode not yet fully implemented.{RESET}");
        }

        // ── Help ──────────────────────────────────────────────────────────────
        "--help" | "-h" | "help" => print_help(),

        // ── Fallback: forward ke remote MELISA host ───────────────────────────
        other => {
            let other_owned    = other.to_string();
            let forward_args: Vec<String> = rest.to_vec();
            if let Err(e) = exec::exec_forward(&other_owned, &forward_args) {
                log_error(&e.to_string());
            }
        }
    }
}

fn print_help() {
    println!("{BOLD}{CYAN}MELISA REMOTE MANAGER — CLI CLIENT{RESET}");
    println!("{BOLD}Usage:{RESET} melisa <command> [arguments]\n");

    println!("{BOLD}AUTHENTICATION & CONNECTIONS:{RESET}");
    println!("  auth add <n> <user@ip>           : Register a new remote MELISA server");
    println!("  auth switch <n>                  : Switch active session to another server");
    println!("  auth list                        : Display all registered remote servers");
    println!("  auth remove <n>                  : Unregister and delete a remote server");
 
    println!("\n{BOLD}PROJECT SYNCHRONISATION:{RESET}");
    println!("  clone <n> [--force]              : Clone a project workspace from the host");
    println!("  sync  <n>                        : Push local workspace modifications to the host");
    println!("  get   <n> [--force]              : Pull the latest master data into local workspace");

    println!("\n{BOLD}REMOTE OPERATIONS:{RESET}");
    println!("  run     <cont> <file>            : Execute a local script remotely (background)");
    println!("  run-tty <cont> <file>            : Execute a script interactively (TTY)");
    println!("  upload  <cont> <dir> <dst>       : Transfer a local directory into a container");
    println!("  shell                            : Open a direct SSH shell to the MELISA host");
    println!("  --list / --active                : Enumerate provisioned containers");

    println!("\n{BOLD}TUNNEL & EXPOSE:{RESET}");
    println!("  tunnel      <cont> <port> [lp]   : Forward container port to localhost");
    println!("  tunnel-list                      : Show all active SSH tunnels");
    println!("  tunnel-stop <cont> [port]        : Stop a running tunnel");

    println!("\n{YELLOW}Execute 'melisa <command> --help' for command-specific details.{RESET}");
}

#[cfg(target_os = "windows")]
fn enable_ansi_on_windows() {
    use std::ffi::c_void;
    extern "system" {
        unsafe fn GetStdHandle(n: u32) -> *mut c_void;
        unsafe fn GetConsoleMode(h: *mut c_void, mode: *mut u32) -> i32;
        unsafe fn SetConsoleMode(h: *mut c_void, mode: u32) -> i32;
    }
    unsafe {
        let h = GetStdHandle(0xFFFFFFF5_u32);
        let mut mode: u32 = 0;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | 0x0004);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_args_slicing_works_correctly() {
        let args: Vec<String> = vec!["auth".into(), "add".into(), "prod".into(), "root@1.2.3.4".into()];
        let command  = args[0].as_str();
        let sub_args = &args[1..];
        assert_eq!(command, "auth");
        assert_eq!(sub_args[0], "add");
        assert_eq!(sub_args[1], "prod");
    }

    #[test]
    fn test_force_flag_detection() {
        let args: Vec<String> = vec!["myproject".into(), "--force".into()];
        assert!(args.iter().any(|a| a == "--force"));
    }

    #[test]
    fn test_port_parsing() {
        assert_eq!("8080".parse::<u16>().unwrap(), 8080);
    }
}