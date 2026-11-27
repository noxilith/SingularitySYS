use serde::{Deserialize, Serialize};

use crate::SchemaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Libc {
    Glibc,
    Musl,
}

impl Libc {
    pub fn as_str(self) -> &'static str {
        match self {
            Libc::Glibc => "glibc",
            Libc::Musl => "musl",
        }
    }

    /// The target triple recipes can pass to configure scripts.
    pub fn target(self) -> &'static str {
        match self {
            Libc::Glibc => "x86_64-linux-gnu",
            Libc::Musl => "x86_64-linux-musl",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitSection {
    /// Which init system is active (e.g. "openrc").
    #[serde(default = "default_init")]
    pub system: String,
    /// Services enabled under that init.
    #[serde(default)]
    pub services: Vec<String>,
}

impl Default for InitSection {
    fn default() -> Self {
        Self { system: default_init(), services: Vec::new() }
    }
}

fn default_init() -> String {
    "openrc".into()
}


/// `init = "systemd"` (just the name) or the `[init]` table with `system` and `services`.
fn de_init<'de, D: serde::Deserializer<'de>>(d: D) -> Result<InitSection, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Name(String),
        Table(InitSection),
    }
    Ok(match Either::deserialize(d)? {
        Either::Name(system) => InitSection { system, services: Vec::new() },
        Either::Table(t) => t,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAccount {
    pub name: String,
    #[serde(default)]
    pub groups: Vec<String>,
    /// The login shell as a program name or path (`fish`, `/usr/bin/fish`); the system's `shell` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// A system account forge itself adds (the login screen's `greeter`): no home directory, never a login.
    #[serde(skip)]
    pub system: bool,
    /// Groups forge adds on its own (the desktop's `video`, `input`, `audio`), as opposed to the ones the user declared.
    #[serde(skip)]
    pub implied_groups: Vec<String>,
}

/// The login shell for root and for users who do not name their own: a program name (`bash`, `fish`). Anything
/// other than the base system's bash is installed for you.
fn default_shell() -> String {
    "bash".into()
}

/// `packages = ["a", "b"]` or `packages = """ a, b """`: names separated by commas and/or white space.
fn de_packages<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        List(Vec<String>),
        Text(String),
    }
    let mut out = Vec::new();
    let parts: Vec<String> = match Either::deserialize(d)? {
        Either::List(l) => l,
        Either::Text(t) => vec![t],
    };
    for part in parts {
        // A `#` starts a comment that runs to the end of the line (the block can carry examples and notes).
        for line in part.lines() {
            let code = line.split('#').next().unwrap_or("");
            out.extend(code.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(String::from));
        }
    }
    Ok(out)
}

/// `/etc/forge/system.toml`: the single source of truth for the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemDecl {
    pub hostname: String,
    pub libc: Libc,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_keymap")]
    pub keymap: String,
    #[serde(default, deserialize_with = "de_init")]
    pub init: InitSection,
    /// Services to enable under the init system (shorthand for `[init] services`).
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub users: Vec<UserAccount>,
    #[serde(default)]
    pub kernel_modules: Vec<String>,
    /// Packages you chose, on top of the base system (which is not listed): a list, or names separated by commas and
    /// white space in one multi-line string.
    #[serde(default, deserialize_with = "de_packages")]
    pub packages: Vec<String>,
    /// The login shell for root and for users who name none: `bash` (the base) or another, which is installed for you.
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Package repositories, as local directories (searched in order).
    #[serde(default)]
    pub repos: Vec<String>,
    /// `zram` (compressed swap in memory, the default) or `none`.
    #[serde(default = "default_swap")]
    pub swap: String,
    #[serde(default)]
    pub firewall: crate::extras::FirewallDecl,
    #[serde(default)]
    pub maintenance: crate::extras::MaintenanceDecl,
    /// Flatpak applications (`org.mozilla.firefox`, or `remote:id` for a remote other than flathub): `forge flatpak`
    /// installs them and keeps them updated, and `forge apply` installs the ones that are missing.
    #[serde(default)]
    pub flatpaks: Vec<String>,
    /// AppImages (`name`, or `name=https://address#sha256=HEX`): `forge appimage` installs them as the .AppImage file with a
    /// launcher and a menu entry, and `forge apply` brings back the ones whose file or launcher is missing.
    #[serde(default)]
    pub appimages: Vec<String>,
}

fn default_swap() -> String {
    "zram".into()
}

fn default_timezone() -> String {
    "UTC".into()
}
fn default_locale() -> String {
    "en_US.UTF-8".into()
}
fn default_keymap() -> String {
    "us".into()
}

impl SystemDecl {

    /// Does the declaration install the Vesper desktop?
    pub fn desktop(&self) -> bool {
        self.packages.iter().any(|p| p == crate::DESKTOP_PACKAGE)
    }

    /// The declaration with the forge packages that declared packages need added: the libraries a `deb-*` package was
    /// converted against are kept in `var/lib/forge/deb/needs.toml` (package -> names), not in the declaration, so the
    /// declaration lists only what you chose and taking a package out withdraws what only it needed.
    pub fn with_wants(&self, wants: &std::collections::BTreeMap<String, Vec<String>>) -> SystemDecl {
        let mut d = self.clone();
        for p in &self.packages {
            for w in wants.get(p).into_iter().flatten() {
                if !d.packages.iter().any(|x| x == w) {
                    d.packages.push(w.clone());
                }
            }
        }
        d
    }

    /// The declaration with what the desktop needs added: its services (and NetworkManager in place of forge's own
    /// network setup), the `greeter` account of the login screen, and the `video`, `input` and `audio` groups for every
    /// user. Take the desktop out of the packages and all of it is withdrawn again, because it is derived, not stored.
    pub fn with_desktop(&self) -> SystemDecl {
        let mut d = self.clone();
        if !self.desktop() {
            return d;
        }
        for s in crate::DESKTOP_SERVICES {
            if !d.init.services.iter().any(|x| x == s) {
                d.init.services.push((*s).to_string());
            }
        }
        if d.init.system == "openrc" {
            for s in crate::DESKTOP_OPENRC_SERVICES {
                if !d.init.services.iter().any(|x| x == s) {
                    d.init.services.push((*s).to_string());
                }
            }
        }
        for u in &mut d.users {
            for g in crate::DESKTOP_GROUPS {
                if !u.groups.iter().any(|x| x == g) {
                    u.groups.push((*g).to_string());
                    u.implied_groups.push((*g).to_string());
                }
            }
        }
        if !d.users.iter().any(|u| u.name == crate::DESKTOP_USER) {
            d.users.push(UserAccount {
                name: crate::DESKTOP_USER.into(),
                groups: vec!["video".into(), "input".into()],
                shell: Some("/usr/sbin/nologin".into()),
                system: true,
                implied_groups: Vec::new(),
            });
        }
        d
    }
    pub fn parse(text: &str) -> Result<Self, SchemaError> {
        let mut decl: Self = toml::from_str(text)?;
        let extra = std::mem::take(&mut decl.services);
        for s in extra {
            if !decl.init.services.contains(&s) {
                decl.init.services.push(s);
            }
        }
        decl.validate()?;
        Ok(decl)
    }

    pub fn validate(&self) -> Result<(), SchemaError> {
        if self.hostname.is_empty() {
            return Err(SchemaError::Invalid("hostname must not be empty".into()));
        }
        if !crate::INIT_SYSTEMS.contains(&self.init.system.as_str()) {
            return Err(SchemaError::Invalid(format!("unknown init system `{}` (supported: {})", self.init.system, crate::INIT_SYSTEMS.join(", "))));
        }
        if !crate::extras::SWAP_KINDS.contains(&self.swap.as_str()) {
            return Err(SchemaError::Invalid(format!("unknown swap kind `{}` (supported: {})", self.swap, crate::extras::SWAP_KINDS.join(", "))));
        }
        self.firewall.validate()?;
        if self.maintenance.keep_generations == 0 {
            return Err(SchemaError::Invalid("maintenance.keep_generations must be at least 1".into()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.packages {
            if !seen.insert(p) {
                return Err(SchemaError::Invalid(format!("duplicate package `{p}`")));
            }
        }
        Ok(())
    }
}

/// `~/.config/forge/user.toml`: one user's own packages (visible only to them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDecl {
    #[serde(default, deserialize_with = "de_packages")]
    pub packages: Vec<String>,
}

impl UserDecl {
    pub fn parse(text: &str) -> Result<Self, SchemaError> {
        Ok(toml::from_str(text)?)
    }
}

/// Where the project's packages are published: a GitHub Release holding `index.toml`, `index.toml.sig`
/// and every archive as flat files. A new system's declaration starts with this repository, and the
/// keys that sign it ship in `/etc/forge/keys`.
pub const DEFAULT_REPO_URL: &str = "https://github.com/noxilith/SingularitySYS/releases/download/repo-glibc";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_of_declared_packages_are_added_and_withdrawn_with_them() {
        let wants = parse_wants("deb-flatpak = [\"fuse3\", \"json-glib\"]\ndeb-other = [\"libXau\"]\n");
        let d = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = [\"deb-flatpak\", \"fuse3\"]\n").unwrap();
        assert_eq!(d.with_wants(&wants).packages, ["deb-flatpak", "fuse3", "json-glib"]);
        let without = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = [\"htop\"]\n").unwrap();
        assert_eq!(without.with_wants(&wants).packages, ["htop"]);
        assert!(parse_wants("not toml [").is_empty());
    }

    #[test]
    fn parses_minimal_system() {
        let d = SystemDecl::parse("hostname = \"singularity\"\nlibc = \"musl\"\n").unwrap();
        assert_eq!(d.libc, Libc::Musl);
        assert_eq!(d.init.system, "openrc");
        assert_eq!(d.timezone, "UTC");
        assert!(d.packages.is_empty());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\nbogus = 1\n").is_err());
    }

    #[test]
    fn rejects_unknown_libc() {
        assert!(SystemDecl::parse("hostname = \"x\"\nlibc = \"uclibc\"\n").is_err());
    }

    #[test]
    fn rejects_duplicate_packages() {
        let t = "hostname = \"x\"\nlibc = \"glibc\"\npackages = [\"kitty\", \"kitty\"]\n";
        assert!(SystemDecl::parse(t).is_err());
    }

    #[test]
    fn parses_repos() {
        let d = SystemDecl::parse("hostname = \"x\"\nlibc = \"musl\"\nrepos = [\"/srv/repo\"]\n").unwrap();
        assert_eq!(d.repos, ["/srv/repo"]);
        assert!(SystemDecl::parse("hostname = \"x\"\nlibc = \"musl\"\n").unwrap().repos.is_empty());
    }


    #[test]
    fn init_can_be_just_a_name() {
        let d = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\ninit = \"systemd\"\nservices = [\"sshd\"]\n").unwrap();
        assert_eq!(d.init.system, "systemd");
        assert_eq!(d.init.services, ["sshd"]);
    }

    #[test]
    fn init_table_form_still_works() {
        let d = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\n[init]\nsystem = \"openrc\"\nservices = [\"sshd\"]\n").unwrap();
        assert_eq!(d.init.system, "openrc");
        assert_eq!(d.init.services, ["sshd"]);
    }

    #[test]
    fn unknown_init_system_is_rejected() {
        let err = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\ninit = \"runit\"\n").unwrap_err().to_string();
        assert!(err.contains("runit") && err.contains("systemd"), "{err}");
    }

    #[test]
    fn packages_may_be_a_list_or_readable_text() {
        let list = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = [\"a\", \"b\"]\n").unwrap();
        assert_eq!(list.packages, ["a", "b"]);
        let text = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = \"\"\"\n  htop,\n  git, vim\n\n  fish\n\"\"\"\n").unwrap();
        assert_eq!(text.packages, ["htop", "git", "vim", "fish"]);
        let empty = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = \"\"\"\n\"\"\"\n").unwrap();
        assert!(empty.packages.is_empty());
        assert!(SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = \"\"\"\n a,\n a\n\"\"\"\n").is_err(), "duplicates are still refused");
    }


    #[test]
    fn comments_inside_the_packages_text_are_ignored() {
        let d = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\npackages = \"\"\"\n  # git,  # not yet\n  htop, vim # editor\n\"\"\"\n").unwrap();
        assert_eq!(d.packages, ["htop", "vim"]);
    }
    #[test]
    fn the_shell_setting_and_per_user_shells() {
        let d = SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\nshell = \"fish\"\n[[users]]\nname = \"ada\"\ngroups = [\"wheel\"]\n[[users]]\nname = \"bob\"\nshell = \"zsh\"\n").unwrap();
        assert_eq!(d.shell, "fish");
        assert_eq!(d.users[0].shell, None);
        assert_eq!(d.users[1].shell.as_deref(), Some("zsh"));
        assert_eq!(SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\n").unwrap().shell, "bash");
    }

    #[test]
    fn the_base_system_is_known_and_includes_what_boots_it() {
        assert!(crate::is_base_package("forge") && crate::is_base_package("limine") && crate::is_base_package("systemd"));
        assert!(!crate::is_base_package("htop") && !crate::is_base_package("linux"));
    }
    #[test]
    fn user_decl_defaults_empty() {
        assert!(UserDecl::parse("").unwrap().packages.is_empty());
    }
}

/// Where (below the root) the libraries each Debian-made package needs are recorded: `deb-NAME = ["fuse3", ...]`.
pub const WANTS_FILE: &str = "var/lib/forge/deb/needs.toml";

/// Read that file's text (a damaged file counts as empty).
pub fn parse_wants(text: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    toml::from_str(text).unwrap_or_default()
}
