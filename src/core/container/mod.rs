// ============================================================================
// src/core/container/mod.rs
//
// Public API for the LXC container subsystem.
// ============================================================================

pub mod lifecycle;
pub mod network;
pub mod query;
pub mod types;

// Re-export the most commonly used items at the `container` level
// so callers can write `crate::core::container::start_container`.
pub use types::{LXC_BASE_PATH, LXC_PATH, DistroMetadata, ContainerStatus};
pub use lifecycle::{create_container, delete_container, start_container, stop_container, attach_to_container};
pub use network::{
    add_shared_folder, remove_shared_folder, ensure_host_network_ready,
    inject_network_config, setup_container_dns, unlock_container_dns,
    ensure_nat_routing_ready,  
};
pub use query::{
    list_containers, get_container_ip, is_container_running,
    container_exists, send_command, upload_to_container,
};