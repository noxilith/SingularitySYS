//! Install Debian packages from apt repositories as forge packages.
//!
//! `forge deb source add` records an apt repository and its signing key; `forge deb update` downloads and checks
//! its package lists; `forge deb install NAME` resolves the Debian dependencies, converts the packages into one
//! forge package (`deb-NAME`) in a local signed repository, and the normal forge apply installs it.

pub mod apt;
pub mod check;
pub mod control;
pub mod convert;
mod debfile;
pub mod install;
pub mod resolve;
