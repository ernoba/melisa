// ============================================================================
// src/deployment/deployer.rs
//
// MELISA Deployment Engine — orchestrates the full container deployment
// pipeline described in a `.mel` manifest file.
//
// Public entry points:
//   cmd_up      — deploy / start a project  (`melisa --up`)
//   cmd_down    — stop a project            (`melisa --down`)
//   cmd_mel_info — display manifest summary  (`melisa --mel-info`)
// ============================================================================

use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

use crate::cli::color::{BOLD, CYAN, GREEN, RED, RESET, YELLOW};
use crate::cli::loading::execute_with_spinner;
use crate::core::container::{
    LXC_BASE_PATH, add_shared_folder, create_container,
    start_container, stop_container,
};
use crate::deployment::dependency::{
    detect_package_manager, has_lang_deps,
    install_lang_deps, install_system_deps,
    lxc_exec, lxc_exec_silent,
};
use crate::deployment::manifest::{
    load_mel_file, MelManifest, MelParseError, HealthSection,
};
use crate::distros::lxc_distro::get_lxc_distro_list;

// ── Health check plan ─────────────────────────────────────────────────────────

/// Resolved health-check parameters with defaults applied.
#[derive(Debug)]
pub struct HealthCheckPlan {
    /// Shell command used to test application readiness.
    pub command: String,
    /// Number of retry attempts before reporting failure.
    pub retries: u32,
    /// Seconds to wait between retry attempts.
    pub interval_secs: u64,
    /// Seconds before a single attempt times out.
    pub timeout_secs: u64,
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Deploys a project from a `.mel` manifest file.
///
/// If the container already exists it is started (if stopped).
/// If the container does not exist it is created from the manifest's distro.
///
/// Full deployment pipeline:
/// 1. Parse and validate manifest.
/// 2. Provision / start the container.
/// 3. Detect package manager.
/// 4. Install system dependencies.
/// 5. Install language dependencies.
/// 6. Mount volumes (triggers container restart).
/// 7. Inject environment variables.
/// 8. Run `on_create` lifecycle hooks.
/// 9. Run health check (if configured).
///
/// # Arguments
/// * `mel_path` - Filesystem path to the `.mel` manifest.
/// * `audit`    - When `true`, subprocess output is forwarded to the terminal.
pub async fn cmd_up(mel_path: &str, audit: bool) {
    println!("\n{}━━━ MELISA DEPLOYMENT ENGINE ━━━{}", BOLD, RESET);
    println!("{}[UP]{} Reading manifest: {}{}{}", CYAN, RESET, BOLD, mel_path, RESET);

    let manifest = match load_mel_file(mel_path).await {
        Ok(m) => m,
        Err(MelParseError::NotFound(p)) => {
            println!("{}[ERROR]{} File '{}' not found.", RED, RESET, p);
            println!(
                "{}Tip:{} Verify the path is correct. Example: melisa --up ./myapp/program.mel",
                YELLOW, RESET
            );
            return;
        }
        Err(MelParseError::TomlParse(err)) => {
            println!("{}[ERROR]{} Invalid .mel file:\n  {}", RED, RESET, err);
            return;
        }
        Err(err) => {
            println!("{}[ERROR]{} {}", RED, RESET, err);
            return;
        }
    };

    let container_name = manifest.container.effective_name(&manifest.project.name);
    print_manifest_summary(&manifest, &container_name);

    // ── Step 1: Provision or start the container ─────────────────────────────
    // ── Step 1: Provision or start the container ─────────────────────────────
    let already_exists = container_exists(&container_name).await;
    if !already_exists {
        println!("\n{}[STEP 1/7]{} Provisioning new container…{}", BOLD, RESET, RESET);
        let (distro_list, is_from_cache) = execute_with_spinner(
            "Validating manifest distro…",
            |_pb| get_lxc_distro_list(audit),
            audit,
        )
        .await;

        if is_from_cache {
            println!("{}[CACHE]{} Using local distro data.", YELLOW, RESET);
        }

        let distro_code = &manifest.container.distro;
        let distro_meta = match distro_list.into_iter().find(|d| &d.slug == distro_code) {
            Some(m) => m,
            None => {
                println!(
                    "{}[ERROR]{} Distro '{}' not found in the distribution registry.",
                    RED, RESET, distro_code
                );
                println!(
                    "{}Tip:{} Run 'melisa --search' to list valid distribution codes.",
                    YELLOW, RESET
                );
                return;
            }
        };

        execute_with_spinner(
            &format!("Creating container '{}'…", container_name),
            |pb| create_container(&container_name, distro_meta, pb, audit),
            audit,
        )
        .await;
    } else {
        println!(
            "{}[INFO]{} Container '{}' already exists — skipping provisioning.",
            YELLOW, RESET, container_name
        );
        if !is_container_running(&container_name).await {
            println!("\n{}[STEP 1/7]{} Starting container…{}", BOLD, RESET, RESET);
            start_container(&container_name, audit).await;
            wait_for_ready(&container_name).await;
        }
    }

    // ── Step 2: Detect package manager ───────────────────────────────────────
    println!("\n{}[STEP 2/7]{} Detecting container environment…{}", BOLD, RESET, RESET);
    let pkg_manager = match detect_package_manager(&container_name).await {
        Some(pm) => {
            println!(
                "{}[INFO]{} Package manager detected: {}{}{}",
                CYAN, RESET, BOLD, pm, RESET
            );
            pm
        }
        None => {
            println!(
                "{}[WARNING]{} No supported package manager found — system deps will be skipped.",
                YELLOW, RESET
            );
            String::new()
        }
    };

    // ── Step 3: System dependencies ───────────────────────────────────────────
    println!("\n{}[STEP 3/7]{} Installing system dependencies…{}", BOLD, RESET, RESET);
    if !pkg_manager.is_empty() {
        let ok = install_system_deps(&container_name, &manifest.dependencies, &pkg_manager).await;
        if !ok {
            println!(
                "{}[WARNING]{} Some system dependencies failed to install — continuing.",
                YELLOW, RESET
            );
        }
    }

    // ── Step 4: Language dependencies ─────────────────────────────────────────
    println!("\n{}[STEP 4/7]{} Installing language dependencies…{}", BOLD, RESET, RESET);
    if has_lang_deps(&manifest.dependencies) {
        let ok = install_lang_deps(&container_name, &manifest.dependencies).await;
        if !ok {
            println!(
                "{}[WARNING]{} Some language dependencies failed — continuing.",
                YELLOW, RESET
            );
        }
    } else {
        println!("{}[INFO]{} No language dependencies defined.", YELLOW, RESET);
    }

    // ── Step 5: Volumes ───────────────────────────────────────────────────────
    println!("\n{}[STEP 5/7]{} Configuring volumes…{}", BOLD, RESET, RESET);
    let mut volumes_were_added = false;

    for mount_spec in &manifest.volumes.mounts {
        let parts: Vec<&str> = mount_spec.split(':').collect();
        if parts.len() == 2 {
            add_shared_folder(&container_name, parts[0], parts[1]).await;
            volumes_were_added = true;
        }
    }

    if volumes_were_added {
        println!(
            "{}[INFO]{} Restarting container to activate volume mounts…",
            YELLOW, RESET
        );
        stop_container(&container_name, audit).await;
        start_container(&container_name, audit).await;
        wait_for_ready(&container_name).await;
    } else {
        println!("{}[INFO]{} No volumes configured.", YELLOW, RESET);
    }

    // ── Step 6: Environment variables ─────────────────────────────────────────
    println!("\n{}[STEP 6/7]{} Injecting environment variables…{}", BOLD, RESET, RESET);
    if !manifest.env.is_empty() {
        inject_env_vars(&container_name, &manifest.env).await;
    } else {
        println!("{}[INFO]{} No environment variables defined.", YELLOW, RESET);
    }

    // ── Step 7: Lifecycle hooks ───────────────────────────────────────────────
    println!("\n{}[STEP 7/7]{} Running lifecycle hooks…{}", BOLD, RESET, RESET);
    run_lifecycle_hooks(&container_name, &manifest.lifecycle.on_create, "on_create").await;

    // ── Health check ──────────────────────────────────────────────────────────
    if let Some(ref health_config) = manifest.health {
        println!("\n{}[HEALTH]{} Running health check…{}", BOLD, RESET, RESET);
        run_health_check(&container_name, health_config).await;
    }

    // ── Summary ───────────────────────────────────────────────────────────────
    println!("\n{}━━━ DEPLOYMENT COMPLETE ━━━{}", GREEN, RESET);
    println!(
        "{}[OK]{} Container '{}{}{}' deployed successfully!",
        GREEN, RESET, BOLD, container_name, RESET
    );

    let enabled_services: Vec<_> = manifest.services.iter()
        .filter(|(_, s)| s.enabled)
        .collect();

    if !enabled_services.is_empty() {
        println!("\n{}Configured services:{}", BOLD, RESET);
        for (name, svc) in &enabled_services {
            println!("  {}•{} {} → {}", CYAN, RESET, name, svc.command);
        }
        println!(
            "\n{}Tip:{} Use 'melisa --send {} <cmd>' to run a service.",
            YELLOW, RESET, container_name
        );
    }

    if !manifest.ports.expose.is_empty() {
        println!("\n{}Exposed ports:{}", BOLD, RESET);
        for port in &manifest.ports.expose {
            println!("  {}•{} {}", CYAN, RESET, port);
        }
    }
    println!();
}

/// Stops a deployed project by reading its `.mel` manifest.
///
/// Runs `on_stop` lifecycle hooks before stopping the container.
///
/// # Arguments
/// * `mel_path` - Filesystem path to the `.mel` manifest.
/// * `audit`    - When `true`, subprocess output is forwarded to the terminal.
pub async fn cmd_down(mel_path: &str, audit: bool) {
    println!("\n{}[DOWN]{} Reading manifest: {}", CYAN, RESET, mel_path);

    let manifest = match load_mel_file(mel_path).await {
        Ok(m) => m,
        Err(err) => {
            println!("{}[ERROR]{} {}", RED, RESET, err);
            return;
        }
    };

    let container_name = manifest.container.effective_name(&manifest.project.name);

    if !is_container_running(&container_name).await {
        println!(
            "{}[INFO]{} Container '{}' is already stopped.",
            YELLOW, RESET, container_name
        );
        return;
    }

    if !manifest.lifecycle.on_stop.is_empty() {
        println!("{}[INFO]{} Running on_stop hooks…", CYAN, RESET);
        run_lifecycle_hooks(&container_name, &manifest.lifecycle.on_stop, "on_stop").await;
    }

    stop_container(&container_name, audit).await;
    println!(
        "{}[OK]{} Container '{}' has been stopped.",
        GREEN, RESET, container_name
    );
}

/// Displays a summary of the `.mel` manifest without modifying any container.
///
/// # Arguments
/// * `mel_path` - Filesystem path to the `.mel` manifest.
pub async fn cmd_mel_info(mel_path: &str) {
    let manifest = match load_mel_file(mel_path).await {
        Ok(m) => m,
        Err(err) => {
            println!("{}[ERROR]{} {}", RED, RESET, err);
            return;
        }
    };

    let container_name = manifest.container.effective_name(&manifest.project.name);
    let is_running = is_container_running(&container_name).await;

    println!("\n{}━━━ MELISA MANIFEST INFO ━━━{}", BOLD, RESET);
    println!(
        "  {}Project   :{} {} v{}",
        BOLD, RESET,
        manifest.project.name,
        manifest.project.version.as_deref().unwrap_or("?")
    );
    if let Some(ref desc) = manifest.project.description {
        println!("  {}Description:{} {}", BOLD, RESET, desc);
    }
    println!(
        "  {}Container :{} {} ({})",
        BOLD, RESET,
        container_name,
        if is_running {
            format!("{}RUNNING{}", GREEN, RESET)
        } else {
            format!("{}STOPPED{}", RED, RESET)
        }
    );
    println!("  {}Distro    :{} {}", BOLD, RESET, manifest.container.distro);
    println!("  {}File      :{} {}", BOLD, RESET, mel_path);

    let sys_pkg_count = manifest.dependencies.apt.len()
        + manifest.dependencies.pacman.len()
        + manifest.dependencies.dnf.len()
        + manifest.dependencies.apk.len();
    let lang_pkg_count = manifest.dependencies.pip.len()
        + manifest.dependencies.npm.len()
        + manifest.dependencies.cargo.len()
        + manifest.dependencies.gem.len()
        + manifest.dependencies.composer.len();

    println!("\n  {}Dependencies:{}", BOLD, RESET);
    println!("    System  : {} package(s)", sys_pkg_count);
    println!("    Language: {} package(s)", lang_pkg_count);

    if !manifest.ports.expose.is_empty() {
        println!("\n  {}Ports:{}", BOLD, RESET);
        for p in &manifest.ports.expose {
            println!("    {}", p);
        }
    }

    if !manifest.volumes.mounts.is_empty() {
        println!("\n  {}Volumes:{}", BOLD, RESET);
        for v in &manifest.volumes.mounts {
            println!("    {}", v);
        }
    }

    if !manifest.services.is_empty() {
        println!("\n  {}Services:{}", BOLD, RESET);
        for (name, svc) in &manifest.services {
            let status_label = if svc.enabled {
                format!("{}enabled{}", GREEN, RESET)
            } else {
                format!("{}disabled{}", YELLOW, RESET)
            };
            println!("    {} [{}] → {}", name, status_label, svc.command);
        }
    }
    println!();
}

// ── Private pipeline helpers ──────────────────────────────────────────────────

/// Checks whether the container exists in LXC.
async fn container_exists(name: &str) -> bool {
    let out = Command::new("sudo")
        .args(&["lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    out.map(|s| s.success()).unwrap_or(false)
}

/// Returns `true` if the container is in the RUNNING state.
async fn is_container_running(name: &str) -> bool {
    let out = Command::new("sudo")
        .args(&["lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
        .output()
        .await;
    if let Ok(o) = out {
        return String::from_utf8_lossy(&o.stdout).contains("RUNNING");
    }
    false
}

/// Polls the container until it reports RUNNING, up to 30 seconds.
async fn wait_for_ready(name: &str) {
    for _ in 0..30 {
        if is_container_running(name).await {
            return;
        }
        sleep(Duration::from_secs(1)).await;
    }
    println!(
        "{}[WARNING]{} Container '{}' did not become ready within 30 seconds.",
        YELLOW, RESET, name
    );
}

/// Injects key-value environment variables into the container's `/etc/environment`.
async fn inject_env_vars(container: &str, env_map: &HashMap<String, String>) {
    for (key, value) in env_map {
        let inject_cmd = build_env_inject_cmd(key, value);
        if lxc_exec_silent(container, &inject_cmd).await {
            println!("{}[ENV]{} Set {}={}", CYAN, RESET, key, value);
        } else {
            println!("{}[WARNING]{} Failed to set {}.", YELLOW, RESET, key);
        }
    }
}

/// Builds the shell command to inject a single environment variable.
///
/// Uses `sed` to remove any existing entry for the key before appending
/// the new value, ensuring idempotency.
pub fn build_env_inject_cmd(key: &str, value: &str) -> String {
    format!(
        "sed -i '/^{key}=/d' /etc/environment && echo '{key}={value}' >> /etc/environment",
        key = key,
        value = value
    )
}

/// Executes a list of shell commands inside the container as lifecycle hooks.
async fn run_lifecycle_hooks(container: &str, hooks: &[String], phase: &str) {
    if hooks.is_empty() {
        println!(
            "{}[INFO]{} No '{}' hooks defined.",
            YELLOW, RESET, phase
        );
        return;
    }
    for hook in hooks {
        println!("{}[{}]{} Running: {}", CYAN, phase, RESET, hook);
        let ok = lxc_exec(container, hook).await;
        if !ok {
            println!(
                "{}[WARNING]{} Hook '{}' failed — continuing.",
                YELLOW, RESET, hook
            );
        }
    }
}

/// Executes the health check with retry logic.
async fn run_health_check(container: &str, health_config: &HealthSection) {
    let plan = build_health_check_retry_plan(health_config);
    println!(
        "{}[HEALTH]{} Command: {}  |  Retries: {}  |  Interval: {}s",
        BOLD, RESET, plan.command, plan.retries, plan.interval_secs
    );

    for attempt in 1..=plan.retries {
        sleep(Duration::from_secs(plan.interval_secs)).await;
        println!(
            "{}[HEALTH]{} Attempt {}/{}…",
            BOLD, RESET, attempt, plan.retries
        );
        if lxc_exec_silent(container, &plan.command).await {
            println!("{}[HEALTH OK]{} Application is healthy.", GREEN, RESET);
            return;
        }
    }

    println!(
        "{}[HEALTH FAIL]{} Application did not become healthy after {} attempt(s).",
        RED, RESET, plan.retries
    );
}

/// Resolves a `HealthSection` into a `HealthCheckPlan` with defaults applied.
///
/// Default values:
/// * `retries`       → 3
/// * `interval_secs` → 5
/// * `timeout_secs`  → 30
pub fn build_health_check_retry_plan(health_config: &HealthSection) -> HealthCheckPlan {
    HealthCheckPlan {
        command: health_config.command.clone(),
        retries: health_config.retries.unwrap_or(3),
        interval_secs: health_config.interval.unwrap_or(5) as u64,
        timeout_secs: health_config.timeout.unwrap_or(30) as u64,
    }
}

/// Prints a human-readable summary of the manifest before deployment begins.
fn print_manifest_summary(manifest: &MelManifest, container_name: &str) {
    println!("\n{}Project  :{} {}", BOLD, RESET, manifest.project.name);
    println!("{}Container:{} {}", BOLD, RESET, container_name);
    println!("{}Distro   :{} {}", BOLD, RESET, manifest.container.distro);
}

/// Formats a port list for display in `cmd_mel_info`.
pub fn format_ports_summary(ports: &[String]) -> String {
    if ports.is_empty() {
        return String::from("(none)");
    }
    ports.join(", ")
}

/// Formats a volume mount list for display in `cmd_mel_info`.
pub fn format_volumes_summary(volumes: &[String]) -> String {
    if volumes.is_empty() {
        return String::from("(none)");
    }
    volumes.join(", ")
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::manifest::types::HealthSection;

    // ── build_env_inject_cmd ─────────────────────────────────────────────────

    #[test]
    fn test_build_env_inject_cmd_contains_key_and_value() {
        let cmd = build_env_inject_cmd("APP_PORT", "3000");
        assert!(cmd.contains("APP_PORT"), "Inject command must contain the variable name");
        assert!(cmd.contains("3000"), "Inject command must contain the value");
        assert!(
            cmd.contains("/etc/environment"),
            "Inject command must target /etc/environment"
        );
        assert!(
            cmd.contains("sed"),
            "Inject command must use sed to remove existing entries"
        );
    }

    #[test]
    fn test_build_env_inject_cmd_handles_database_url_with_special_chars() {
        let cmd = build_env_inject_cmd("DB_URL", "postgres://user:pass@localhost/db");
        assert!(cmd.contains("DB_URL"), "Command must contain variable name");
        assert!(
            cmd.contains("postgres://user:pass@localhost/db"),
            "Command must contain the full database URL value"
        );
    }

    #[test]
    fn test_build_env_inject_cmd_removes_existing_entry_before_appending() {
        let cmd = build_env_inject_cmd("MY_VAR", "new_value");
        let sed_pos = cmd.find("sed").unwrap_or(usize::MAX);
        let echo_pos = cmd.find("echo").unwrap_or(usize::MAX);
        assert!(
            sed_pos < echo_pos,
            "sed (removal) must come before echo (append) in the inject command"
        );
    }

    // ── build_health_check_retry_plan ────────────────────────────────────────

    #[test]
    fn test_build_health_check_retry_plan_applies_defaults_when_fields_are_none() {
        let health_config = HealthSection {
            command: "curl localhost".into(),
            interval: None,
            retries: None,
            timeout: None,
        };
        let plan = build_health_check_retry_plan(&health_config);
        assert_eq!(plan.retries, 3, "Default retries must be 3");
        assert_eq!(plan.interval_secs, 5, "Default interval must be 5 seconds");
        assert_eq!(plan.timeout_secs, 30, "Default timeout must be 30 seconds");
        assert_eq!(plan.command, "curl localhost");
    }

    #[test]
    fn test_build_health_check_retry_plan_uses_explicit_values() {
        let health_config = HealthSection {
            command: "wget -q localhost:8080".into(),
            interval: Some(10),
            retries: Some(5),
            timeout: Some(60),
        };
        let plan = build_health_check_retry_plan(&health_config);
        assert_eq!(plan.retries, 5, "Must use the explicit retries value");
        assert_eq!(plan.interval_secs, 10, "Must use the explicit interval value");
        assert_eq!(plan.timeout_secs, 60, "Must use the explicit timeout value");
    }

    // ── format_ports_summary ─────────────────────────────────────────────────

    #[test]
    fn test_format_ports_summary_single_port() {
        let ports = vec!["3000:3000".to_string()];
        let summary = format_ports_summary(&ports);
        assert!(summary.contains("3000:3000"), "Port summary must include the port mapping");
    }

    #[test]
    fn test_format_ports_summary_empty_returns_none_label() {
        let ports: Vec<String> = vec![];
        let summary = format_ports_summary(&ports);
        assert_eq!(summary, "(none)", "Empty port list must display as '(none)'");
    }

    #[test]
    fn test_format_ports_summary_multiple_ports_are_comma_separated() {
        let ports = vec!["8080:8080".to_string(), "443:443".to_string()];
        let summary = format_ports_summary(&ports);
        assert!(summary.contains("8080:8080"), "Summary must include first port");
        assert!(summary.contains("443:443"), "Summary must include second port");
        assert!(summary.contains(", "), "Ports must be comma-separated");
    }

    // ── format_volumes_summary ────────────────────────────────────────────────

    #[test]
    fn test_format_volumes_summary_multiple_volumes() {
        let vols = vec![
            "./src:/app/src".to_string(),
            "./data:/var/data".to_string(),
        ];
        let summary = format_volumes_summary(&vols);
        assert!(summary.contains("./src:/app/src"), "Summary must include first volume");
        assert!(summary.contains("./data:/var/data"), "Summary must include second volume");
    }

    #[test]
    fn test_format_volumes_summary_empty_returns_none_label() {
        let vols: Vec<String> = vec![];
        let summary = format_volumes_summary(&vols);
        assert_eq!(summary, "(none)", "Empty volume list must display as '(none)'");
    }
}