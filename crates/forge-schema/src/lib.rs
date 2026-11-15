//! Shared schemas for Singularity: the system/user declaration and package recipes.
//!
//! Every other crate (CLI, installer, store daemon) depends on these types so
//! they cannot drift apart.

mod init;
mod recipe;
mod system;

pub use init::{BASE_PACKAGES, Entry, is_base_package, INIT_SYSTEMS, SYSTEMD_BASELINE, SYSTEMD_NETWORK_FILES, SYSTEMD_WIFI_FILES, foreign_init_packages, init_packages, power_commands, power_plumbing, power_script, plumbing, systemd_unit};
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
