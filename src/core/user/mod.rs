pub mod management;
pub mod sudoers;
pub mod types;

pub use management::{
    add_melisa_user, delete_melisa_user, list_melisa_users,
    set_user_password, upgrade_user, clean_orphaned_sudoers,
};

pub use sudoers::{
    build_sudoers_rule,
    configure_sudoers,
    remove_orphaned_sudoers_files,
    remove_invalid_melisa_sudoers, // FIX: export new pre-flight repair helper
    check_if_admin,
    sudoers_file_path,
};

pub use types::UserRole;