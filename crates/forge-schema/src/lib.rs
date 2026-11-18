//! Shared schemas for Singularity: the system/user declaration and package recipes.
//!
//! Every other crate (CLI, installer, store daemon) depends on these types so
//! they cannot drift apart.

mod extras;
mod init;
mod managed;
mod recipe;
mod services;
mod system;

pub use extras::{Allow, FirewallDecl, Forward, MaintenanceDecl, SWAP_KINDS, nftables_conf, parse_allow, parse_target};
pub use services::managed_for;
pub use managed::{AGETTY_PORTS, Managed, hosts_file, OPENRC_RUNLEVELS, Own, managed, profile_script};
pub use init::{DESKTOP_SERVICES, BASE_PACKAGES, Entry, is_base_package, INIT_SYSTEMS, SYSTEMD_BASELINE, SYSTEMD_NETWORK_FILES, SYSTEMD_WIFI_FILES, foreign_init_packages, init_packages, systemd_unit_file, power_commands, power_plumbing, power_script, plumbing, systemd_unit};
pub use recipe::{Build, Depends, Effective, LibcOverride, Provides, Recipe, Source};
pub use system::{DEFAULT_REPO_URL, InitSection, Libc, SystemDecl, UserAccount, UserDecl};

/// Error parsing a declaration or recipe.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("invalid TOML")]
    Toml(#[from] toml::de::Error),
    #[error("{0}")]
    Invalid(String),
}
