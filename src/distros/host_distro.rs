// ============================================================================
// src/distros/host_distro.rs
//
// Host Linux distribution detection and per-distro configuration.
//
// Detects the host OS by reading `/etc/os-release` and maps it to the
// correct package manager, LXC package names, and firewall tool.
//
// NOTE: This module now uses the trait-based DistroRegistry for definitions.
// ============================================================================

use tokio::fs;
use crate::distros::abstraction::DistroConfig;
use crate::distros::registry::DistroRegistry;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Supported Linux distribution families.
/// NOTE: This enum is kept for backward compatibility.
/// New code should prefer using DistroRegistry for more flexibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDistro {
    /// Debian / Ubuntu / Linux Mint and derivatives.
    Debian,
    /// Fedora / RHEL / CentOS / Rocky Linux.
    Fedora,
    /// Arch Linux / Manjaro.
    Arch,
    /// Alpine Linux.
    Alpine,
    /// openSUSE Leap / Tumbleweed.
    Suse,
    /// OrbStack Linux (detected by ID=orbstack in /etc/os-release).
    OrbStack,
    /// Any distribution not matched by the above families.
    Unknown,
}

// Re-export firewall types from abstraction for backward compatibility
pub use crate::distros::abstraction::FirewallKind;

// ── Public API ────────────────────────────────────────────────────────────────

/// Detects the host Linux distribution by parsing `/etc/os-release`.
///
/// Falls back to [`HostDistro::Unknown`] when the file is absent or
/// the ID is not recognised.
// src/distros/host_distro.rs
pub async fn detect_host_distro() -> HostDistro {
    let os_release = fs::read_to_string("/etc/os-release")
        .await
        .unwrap_or_default();

    // Cek OrbStack lebih dulu via konten penuh (bukan hanya ID=)
    if os_release.to_lowercase().contains("orbstack") {
        return HostDistro::OrbStack;
    }

    let id_line = os_release
        .lines()
        .find(|line| line.starts_with("ID="))          // ← ID= saja dulu
        .or_else(|| os_release.lines()
            .find(|line| line.starts_with("ID_LIKE="))) // ← ID_LIKE= sebagai fallback
        .unwrap_or("");

    let id_value = id_line
        .split('=')
        .nth(1)
        .unwrap_or("")
        .trim_matches('"')
        .to_lowercase();

    classify_distro(&id_value)
}

/// Returns the [`DistroConfig`] for the detected host distribution.
///
/// This function now uses the trait-based registry internally.
/// # Arguments
/// * `distro` - The detected [`HostDistro`] variant.
pub fn get_distro_config(distro: &HostDistro) -> DistroConfig {
    let registry = DistroRegistry::new();
    
    // Map enum to registry lookup
    let id = match distro {
        HostDistro::Debian => "debian",
        HostDistro::Fedora => "fedora",
        HostDistro::Arch => "arch",
        HostDistro::Alpine => "alpine",
        HostDistro::Suse => "suse",
        HostDistro::OrbStack => "orbstack",
        HostDistro::Unknown => "unknown",
    };
    
    registry.get_host_distro(id).unwrap_or_else(|| {
        // Fallback to unknown distro config
        registry.get_host_distro("unknown")
            .expect("Unknown distro should always be available")
    })
}

/// Get distro config directly from registry using ID string
/// This is a new convenience function for registry-based code
pub fn get_distro_config_by_id(id: &str) -> Option<DistroConfig> {
    let registry = DistroRegistry::new();
    registry.get_host_distro(id)
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Maps a distribution ID string to a [`HostDistro`] variant.
fn classify_distro(id: &str) -> HostDistro {
    if id.contains("ubuntu") || id.contains("debian") || id.contains("mint") || id.contains("raspbian") {
        HostDistro::Debian
    } else if id.contains("fedora") || id.contains("rhel") || id.contains("centos") || id.contains("rocky") || id.contains("almalinux") {
        HostDistro::Fedora
    } else if id.contains("arch") || id.contains("manjaro") || id.contains("endeavour") {
        HostDistro::Arch
    } else if id.contains("alpine") {
        HostDistro::Alpine
    } else if id.contains("suse") || id.contains("opensuse") {
        HostDistro::Suse
    } 
    else if id.contains("orbstack") {
        HostDistro::OrbStack
    } else {
        HostDistro::Unknown
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_distro ──────────────────────────────────────────────────────

    #[test]
    fn test_classify_distro_ubuntu_maps_to_debian_family() {
        assert_eq!(
            classify_distro("ubuntu"),
            HostDistro::Debian,
            "Ubuntu must be classified in the Debian family"
        );
    }

    #[test]
    fn test_classify_distro_debian_maps_to_debian_family() {
        assert_eq!(
            classify_distro("debian"),
            HostDistro::Debian,
            "Debian must be classified in the Debian family"
        );
    }

    #[test]
    fn test_classify_distro_fedora_maps_to_fedora_family() {
        assert_eq!(
            classify_distro("fedora"),
            HostDistro::Fedora,
            "Fedora must be classified in the Fedora family"
        );
    }

    #[test]
    fn test_classify_distro_centos_maps_to_fedora_family() {
        assert_eq!(
            classify_distro("centos"),
            HostDistro::Fedora,
            "CentOS must be classified in the Fedora family"
        );
    }

    #[test]
    fn test_classify_distro_arch_maps_to_arch_family() {
        assert_eq!(
            classify_distro("arch"),
            HostDistro::Arch,
            "Arch must be classified in the Arch family"
        );
    }

    #[test]
    fn test_classify_distro_alpine_maps_to_alpine() {
        assert_eq!(
            classify_distro("alpine"),
            HostDistro::Alpine,
            "Alpine must be classified as Alpine"
        );
    }

    #[test]
    fn test_classify_distro_opensuse_maps_to_suse() {
        assert_eq!(
            classify_distro("opensuse"),
            HostDistro::Suse,
            "openSUSE must be classified in the SUSE family"
        );
    }

    #[test]
    fn test_classify_distro_unknown_returns_unknown() {
        assert_eq!(
            classify_distro("haiku"),
            HostDistro::Unknown,
            "Unrecognised distribution ID must return Unknown"
        );
    }

    // ── get_distro_config ─────────────────────────────────────────────────────

    #[test]
    fn test_get_distro_config_debian_uses_apt_get() {
        let config = get_distro_config(&HostDistro::Debian);
        assert_eq!(
            config.pkg_manager, "apt-get",
            "Debian family must use apt-get"
        );
        assert_eq!(
            config.firewall_tool,
            FirewallKind::Ufw,
            "Debian family must use UFW"
        );
    }

    #[test]
    fn test_get_distro_config_fedora_uses_dnf_and_firewalld() {
        let config = get_distro_config(&HostDistro::Fedora);
        assert_eq!(config.pkg_manager, "dnf", "Fedora family must use dnf");
        assert_eq!(
            config.firewall_tool,
            FirewallKind::Firewalld,
            "Fedora family must use firewalld"
        );
    }

    #[test]
    fn test_get_distro_config_arch_uses_pacman_and_iptables() {
        let config = get_distro_config(&HostDistro::Arch);
        assert_eq!(config.pkg_manager, "pacman", "Arch must use pacman");
        assert_eq!(
            config.firewall_tool,
            FirewallKind::Iptables,
            "Arch must use iptables"
        );
    }

    #[test]
    fn test_get_distro_config_all_variants_have_non_empty_lxc_packages() {
        for distro in &[
            HostDistro::Debian, HostDistro::Fedora, HostDistro::Arch,
            HostDistro::Alpine, HostDistro::Suse, HostDistro::Unknown,
        ] {
            let config = get_distro_config(distro);
            assert!(
                !config.lxc_packages.is_empty(),
                "Every distribution config must define at least one LXC package"
            );
            assert!(
                config.lxc_packages.iter().any(|p| p.contains("lxc")),
                "Every distribution config must include an 'lxc' package"
            );
        }
    }

    #[test]
    fn test_firewall_kind_equality() {
        assert_eq!(FirewallKind::Ufw, FirewallKind::Ufw);
        assert_ne!(FirewallKind::Ufw, FirewallKind::Firewalld);
        assert_ne!(FirewallKind::Firewalld, FirewallKind::Iptables);
    }
}

#[cfg(test)]
mod orbstack_tests {
    use super::*;

    #[test]
    fn test_classify_distro_detects_orbstack() {
        // Simulate ID string from OrbStack /etc/os-release
        let orb_id = "orbstack";
        let result = classify_distro(orb_id);
        
        assert_eq!(
            result, 
            HostDistro::OrbStack, 
            "Must detect 'orbstack' as HostDistro::OrbStack"
        );
    }

    #[test]
    fn test_orbstack_config_values() {
        let config = get_distro_config(&HostDistro::OrbStack);
        
        assert_eq!(config.pkg_manager, "apt-get");
        assert_eq!(config.firewall_tool, FirewallKind::Iptables);
        
        // Ensure at least 3 basic packages are defined
        assert!(
            config.lxc_packages.len() >= 3, 
            "OrbStack must have at least lxc, lxc-templates, and uidmap packages"
        );
        
        // Verify display name
        assert!(config.name.contains("OrbStack"));
    }
}