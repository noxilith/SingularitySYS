use std::fs;
use std::path::Path;

use forge_schema::SystemDecl;
use tempfile::TempDir;

use super::*;

fn decl(extra: &str) -> SystemDecl {
    SystemDecl::parse(&format!("hostname = \"singularity\"\nlibc = \"musl\"\n{extra}")).unwrap()
}

/// A root with the OpenRC scripts the tests may enable.
fn root() -> TempDir {
    let t = tempfile::tempdir().unwrap();
    let init = t.path().join("etc/init.d");
    fs::create_dir_all(&init).unwrap();
    for s in ["sshd", "chronyd", "networking"] {
        fs::write(init.join(s), "#!/sbin/openrc-run\n").unwrap();
    }
    t
}

/// The key that signs every test repository, and the policy that trusts it.
fn test_key() -> SecretKey {
    SecretKey::from_seed([9; 32])
}

fn trust_of(keys: &[&SecretKey]) -> Trust {
    let mut ring = Keyring::empty();
    for k in keys {
        ring.insert(k.public());
    }
    Trust::Require(ring)
}

fn opts() -> Options {
    Options { rebuild: false, trust: trust_of(&[&test_key()]), fetch: Default::default(), adopt: false, base: vec![], upgrade: false, purge: false }
}

fn plan(d: &SystemDecl, r: &Path) -> Plan {
    plan_system(d, r, opts()).unwrap()
}

fn read(r: &Path, rel: &str) -> String {
    fs::read_to_string(r.join(rel)).unwrap()
}

#[test]
fn fresh_root_gets_basic_settings() {
    let r = root();
    let p = plan(&decl(""), r.path());
    let sections: Vec<_> = p.changes.iter().map(|c| c.section).collect();
    for s in ["hostname", "timezone", "locale", "keymap"] {
        assert!(sections.contains(&s), "missing {s}");
    }
    assert!(p.changes.iter().all(|c| c.kind == Kind::Add));
    apply(&p).unwrap();
    assert_eq!(read(r.path(), "etc/hostname"), "singularity\n");
    assert_eq!(read(r.path(), "etc/locale.conf"), "LANG=en_US.UTF-8\n");
    assert_eq!(read(r.path(), "etc/conf.d/keymaps"), "keymap=\"us\"\n");
    assert_eq!(
        fs::read_link(r.path().join("etc/localtime")).unwrap(),
        Path::new("/usr/share/zoneinfo/UTC")
    );
}

#[test]
fn second_apply_has_nothing_to_do() {
    let r = root();
    let d = decl("kernel_modules = [\"btrfs\"]\n[init]\nservices = [\"sshd\"]\n");
    apply(&plan(&d, r.path())).unwrap();
    assert!(plan(&d, r.path()).is_empty());
}

#[test]
fn rebuild_reapplies_everything() {
    let r = root();
    let d = decl("[init]\nservices = [\"sshd\"]\n");
    apply(&plan(&d, r.path())).unwrap();
    let p = plan_system(&d, r.path(), Options { rebuild: true, ..opts() }).unwrap();
    assert!(!p.changes.is_empty());
    assert!(p.changes.iter().all(|c| c.kind == Kind::Modify));
    assert!(p.changes.iter().all(|c| c.summary.ends_with("(re-apply)")));
    apply(&p).unwrap();
}

#[test]
fn changed_hostname_is_a_modify() {
    let r = root();
    apply(&plan(&decl(""), r.path())).unwrap();
    let d = SystemDecl::parse("hostname = \"other\"\nlibc = \"musl\"\n").unwrap();
    let p = plan(&d, r.path());
    assert_eq!(p.changes.len(), 1);
    assert_eq!(p.changes[0].kind, Kind::Modify);
    apply(&p).unwrap();
    assert_eq!(read(r.path(), "etc/hostname"), "other\n");
}

#[test]
fn enabling_services_links_them_into_the_default_runlevel() {
    let r = root();
    let d = decl("[init]\nservices = [\"sshd\", \"chronyd\"]\n");
    apply(&plan(&d, r.path())).unwrap();
    for s in ["sshd", "chronyd"] {
        let link = r.path().join(format!("etc/runlevels/default/{s}"));
        assert_eq!(fs::read_link(link).unwrap(), Path::new(&format!("/etc/init.d/{s}")));
    }
    assert!(read(r.path(), "var/lib/forge/state.toml").contains("sshd"));
}

#[test]
fn only_services_forge_enabled_are_disabled_again() {
    let r = root();
    // `networking` was enabled by someone else (e.g. the live ISO).
    let rl = r.path().join("etc/runlevels/default");
    fs::create_dir_all(&rl).unwrap();
    symlink("/etc/init.d/networking", rl.join("networking")).unwrap();

    apply(&plan(&decl("[init]\nservices = [\"sshd\"]\n"), r.path())).unwrap();
    let p = plan(&decl(""), r.path());
    let removals: Vec<_> = p.changes.iter().filter(|c| c.kind == Kind::Remove).collect();
    assert_eq!(removals.len(), 1);
    assert!(removals[0].summary.contains("sshd"));
    apply(&p).unwrap();

    assert!(rl.join("sshd").symlink_metadata().is_err());
    assert!(rl.join("networking").symlink_metadata().is_ok(), "must not touch unmanaged services");
    assert!(plan(&decl(""), r.path()).is_empty());
}

#[test]
fn declaring_vesper_sets_up_the_desktop_and_removing_it_takes_it_all_away_again() {
    let r = root();
    publish_v(r.path(), "vesper", "1.0", "desktop");
    fs::write(r.path().join("etc/passwd"), "root:x:0:0::/root:/bin/sh\nalice:x:1000:1000::/home/alice:/bin/sh\n").unwrap();
    fs::write(r.path().join("etc/group"), "root:x:0:\nalice:x:1000:\nvideo:x:12:\ninput:x:13:\naudio:x:14:\n").unwrap();
    let d = decl("repos = [\"/srv/repo\"]\npackages = [\"vesper\"]\n[[users]]\nname = \"alice\"\ngroups = [\"wheel\"]\n");
    let p = plan(&d, r.path());
    let text: Vec<String> = p.changes.iter().map(|c| c.summary.clone()).collect();
    assert!(text.iter().any(|s| s.contains("system user greeter: create")), "{text:?}");
    assert!(text.iter().any(|s| s.contains("alice: join") && s.contains("video") && s.contains("input") && s.contains("audio")), "{text:?}");
    for s in forge_schema::DESKTOP_SERVICES {
        assert!(text.iter().any(|t| t.contains(s)), "{s} is enabled: {text:?}");
    }

    // Later: the desktop is installed (the state remembers what forge added) and then taken out of the declaration.
    fs::write(r.path().join("etc/passwd"), "root:x:0:0::/root:/bin/sh\nalice:x:1000:1000::/home/alice:/bin/sh\ngreeter:x:990:990::/var/lib/greeter:/usr/sbin/nologin\n").unwrap();
    fs::write(r.path().join("etc/group"), "root:x:0:\nalice:x:1000:\nvideo:x:12:alice,greeter\ninput:x:13:alice,greeter\naudio:x:14:alice\n").unwrap();
    fs::create_dir_all(r.path().join("var/lib/forge")).unwrap();
    fs::write(
        r.path().join("var/lib/forge/state.toml"),
        "managed_services = [\"seatd\", \"greetd\"]\nmanaged_users = [\"greeter\"]\nmanaged_memberships = [\"alice:video\", \"alice:input\", \"alice:audio\"]\n",
    )
    .unwrap();
    let gone = decl("[[users]]\nname = \"alice\"\ngroups = [\"wheel\"]\n");
    let p = plan(&gone, r.path());
    let text: Vec<String> = p.changes.iter().map(|c| c.summary.clone()).collect();
    assert!(text.iter().any(|s| s.contains("user greeter: remove")), "{text:?}");
    for g in ["video", "input", "audio"] {
        assert!(text.iter().any(|s| s.contains("alice: leave") && s.contains(g)), "{g}: {text:?}");
    }
    // A group the user declared themselves is not taken away.
    let keeps = decl("[[users]]\nname = \"alice\"\ngroups = [\"wheel\", \"video\"]\n");
    let text: Vec<String> = plan(&keeps, r.path()).changes.iter().map(|c| c.summary.clone()).collect();
    assert!(!text.iter().any(|s| s.contains("alice: leave video")), "{text:?}");
    assert!(text.iter().any(|s| s.contains("alice: leave input")), "{text:?}");
}

#[test]
fn removals_come_after_everything_else() {
    let r = root();
    apply(&plan(&decl("[init]\nservices = [\"sshd\"]\n"), r.path())).unwrap();
    let p = plan(
        &SystemDecl::parse("hostname = \"x\"\nlibc = \"musl\"\n[init]\nservices = [\"chronyd\"]\n")
            .unwrap(),
        r.path(),
    );
    let kinds: Vec<_> = p.changes.iter().map(|c| c.kind).collect();
    assert_eq!(kinds.last(), Some(&Kind::Remove));
    assert!(kinds.iter().rev().skip(1).all(|k| *k != Kind::Remove));
}

#[test]
fn removing_all_kernel_modules_removes_the_file() {
    let r = root();
    apply(&plan(&decl("kernel_modules = [\"btrfs\"]\n"), r.path())).unwrap();
    assert!(read(r.path(), "etc/modules-load.d/forge.conf").contains("btrfs"));
    let p = plan(&decl(""), r.path());
    assert!(p.changes.iter().any(|c| c.kind == Kind::Remove && c.section == "kernel modules"));
    apply(&p).unwrap();
    assert!(!r.path().join("etc/modules-load.d/forge.conf").exists());
}

#[test]
fn missing_service_is_a_planning_error() {
    let r = root();
    let d = decl("[init]\nservices = [\"nosuchd\"]\n");
    assert!(matches!(
        plan_system(&d, r.path(), opts()),
        Err(PlanError::ServiceMissing(s)) if s == "nosuchd"
    ));
}

#[test]
fn rejects_unsafe_values() {
    let r = root();
    for extra in [
        "timezone = \"../../etc/passwd\"\n",
        "timezone = \"/etc/passwd\"\n",
        "locale = \"en US\"\n",
        "keymap = \"us; rm\"\n",
        "kernel_modules = [\"a b\"]\n",
    ] {
        assert!(plan_system(&decl(extra), r.path(), opts()).is_err(), "{extra}");
    }
    let bad_host = SystemDecl::parse("hostname = \"a/b\"\nlibc = \"musl\"\n").unwrap();
    assert!(plan_system(&bad_host, r.path(), opts()).is_err());
}

#[test]
fn declared_users_are_created_with_their_groups_and_shell() {
    let r = root();
    let d = decl("[[users]]\nname = \"alice\"\ngroups = [\"wheel\", \"audio\"]\n");
    let p = plan(&d, r.path());
    let users: Vec<_> = p.changes.iter().filter(|c| c.section == "users").map(|c| c.summary.clone()).collect();
    assert!(users.iter().any(|s| s == "group wheel: create") && users.iter().any(|s| s == "group audio: create"), "{users:?}");
    assert!(users.iter().any(|s| s.starts_with("user alice: create") && s.contains("sudo passwd alice")), "{users:?}");
    assert!(p.pending.iter().all(|x| x.section != "users"), "no longer pending");
}

#[test]
fn an_existing_user_is_only_adjusted_where_it_differs() {
    let r = root();
    fs::write(r.path().join("etc/passwd"), "alice:x:1000:1000::/home/alice:/bin/sh\nroot:x:0:0::/root:/bin/bash\n").unwrap();
    fs::write(r.path().join("etc/group"), "wheel:x:10:alice\naudio:x:11:\nalice:x:1000:\n").unwrap();
    let d = decl("[[users]]\nname = \"alice\"\ngroups = [\"wheel\", \"audio\"]\n");
    let users: Vec<_> = plan(&d, r.path()).changes.iter().filter(|c| c.section == "users").map(|c| c.summary.clone()).collect();
    assert_eq!(users, ["user alice: shell /bin/sh -> /usr/bin/bash", "user alice: join audio"], "{users:?}");
    // Already as declared: nothing to do for the user.
    fs::write(r.path().join("etc/passwd"), "alice:x:1000:1000::/home/alice:/bin/bash\n").unwrap();
    fs::write(r.path().join("etc/group"), "wheel:x:10:alice\naudio:x:11:alice\n").unwrap();
    assert!(plan(&d, r.path()).changes.iter().all(|c| c.section != "users"));
}

#[test]
fn another_shell_is_installed_and_listed_in_etc_shells() {
    let r = root();
    fs::write(r.path().join("etc/shells"), "/bin/sh\n/bin/bash\n").unwrap();
    fs::write(r.path().join("etc/passwd"), "root:x:0:0::/root:/bin/bash\n").unwrap();
    publish_all(r.path(), &["fish"]);
    let d = decl("repos = [\"/srv/repo\"]\nshell = \"fish\"\n");
    let p = plan(&d, r.path());
    assert!(p.changes.iter().any(|c| c.section == "packages" && c.summary.starts_with("fish-")), "the shell's package is installed");
    assert!(p.changes.iter().any(|c| c.section == "shell" && c.summary.contains("/etc/shells")));
    assert!(p.changes.iter().any(|c| c.section == "users" && c.summary == "user root: shell /bin/bash -> /usr/bin/fish"));
}

#[test]
fn the_base_system_is_installed_without_being_declared() {
    let r = root();
    publish_all(r.path(), &["basepkg", "a", "openrc", "chrony"]);
    let o = Options { base: vec!["basepkg".into()], ..opts() };
    let p = plan_system(&pkg_decl(&["a"]), r.path(), o).unwrap();
    let names: Vec<_> = p.changes.iter().filter_map(|c| c.package_id()).map(|id| id.name().to_string()).collect();
    assert!(names.contains(&"basepkg".to_string()) && names.contains(&"a".to_string()), "{names:?}");
}

// ---- packages -------------------------------------------------------------

use forge_store::{Keyring, LocalRepo, PackOptions, Profile, SecretKey, Store, Trust, pack_dir};

/// Build a package and publish it to `<root>/srv/repo`; returns its store id.
fn publish(root: &Path, name: &str, files: &[(&str, &str)], deps: &[&str]) -> String {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let src = scratch.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for (path, content) in files {
        let p = src.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        if path.contains("bin/") {
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let opts = PackOptions {
        name: name.into(),
        version: "1.0".into(),
        libc: forge_schema::Libc::Musl,
        hash: None,
        deps: deps.iter().map(|d| d.to_string()).collect(),
    };
    let out = scratch.path().join("pkg.tar.zst");
    pack_dir(&src, &opts, &out).unwrap();
    LocalRepo::new(root.join("srv/repo")).publish(&out, Some(&test_key())).unwrap().id().unwrap().to_string()
}

fn pkg_decl(packages: &[&str]) -> SystemDecl {
    let list = packages.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
    decl(&format!("repos = [\"/srv/repo\"]\npackages = [{list}]\n"))
}

fn system_profile(r: &Path) -> Profile {
    Profile::new(r.join("forge/profiles/system"))
}

/// A path in the system tree (packages are copied into the real tree; the profile only records generations).
fn view(r: &Path, rel: &str) -> std::path::PathBuf {
    r.join(rel)
}

#[test]
fn declared_packages_are_installed_and_linked_with_their_dependencies() {
    let r = root();
    let lib = publish(r.path(), "libfoo", &[("usr/lib/libfoo.so", "lib")], &[]);
    publish(r.path(), "hello", &[("usr/bin/hello", "#!/bin/sh\n")], &[&lib]);

    let p = plan(&pkg_decl(&["hello"]), r.path());
    let pkgs: Vec<_> = p.changes.iter().filter(|c| c.section == "packages").collect();
    assert_eq!(pkgs.len(), 2, "the dependency is pulled in: {pkgs:?}");
    assert!(pkgs.iter().all(|c| c.kind == Kind::Add && c.summary.contains("install from")));
    apply(&p).unwrap();

    let store = Store::new(r.path().join("forge/store"));
    assert_eq!(store.list().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(view(r.path(), "usr/bin/hello")).unwrap(), "#!/bin/sh\n");
    assert_eq!(fs::read_to_string(view(r.path(), "usr/lib/libfoo.so")).unwrap(), "lib");
    // The system tree itself holds real copies, not links, and the state records what forge put there.
    let real = r.path().join("usr/bin/hello");
    assert_eq!(fs::read_to_string(&real).unwrap(), "#!/bin/sh\n");
    assert!(!fs::symlink_metadata(&real).unwrap().file_type().is_symlink());
    assert!(forge_store::Materialized::load(r.path()).unwrap().entries.iter().any(|e| e.path == "usr/bin/hello"));
    let env = read(r.path(), "etc/profile.d/forge.sh");
    assert!(env.contains("/usr/bin") && !env.contains("forge/profiles"), "PATH has /usr, never the store links: {env}");
    assert!(plan(&pkg_decl(&["hello"]), r.path()).is_empty(), "converged");
}

#[test]
fn removing_a_package_unlinks_it_but_keeps_it_in_the_store_until_gc() {
    let r = root();
    publish(r.path(), "a", &[("usr/bin/a", "a")], &[]);
    publish(r.path(), "b", &[("usr/bin/b", "b")], &[]);
    apply(&plan(&pkg_decl(&["a", "b"]), r.path())).unwrap();

    let p = plan(&pkg_decl(&["a"]), r.path());
    let removals: Vec<_> = p.changes.iter().filter(|c| c.kind == Kind::Remove && c.section == "packages").collect();
    assert_eq!(removals.len(), 1);
    assert!(removals[0].summary.starts_with("b-"));
    apply(&p).unwrap();

    assert!(!view(r.path(), "usr/bin/b").exists());
    assert!(view(r.path(), "usr/bin/a").exists());
    assert!(!r.path().join("usr/bin/b").exists(), "the removed package is gone from the real tree too");
    assert!(r.path().join("usr/bin/a").exists());
    assert_eq!(Store::new(r.path().join("forge/store")).list().unwrap().len(), 2, "still stored");
    assert_eq!(system_profile(r.path()).generations().unwrap().len(), 2, "the old set is a generation");
    assert!(plan(&pkg_decl(&["a"]), r.path()).is_empty());
}

#[test]
fn a_package_already_in_the_store_is_only_linked() {
    let r = root();
    publish(r.path(), "a", &[("usr/bin/a", "a")], &[]);
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap();
    apply(&plan(&pkg_decl(&[]), r.path())).unwrap();
    assert!(!view(r.path(), "usr/bin/a").exists());

    // Break the repository: the package must still come back from the store.
    for e in fs::read_dir(r.path().join("srv/repo")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "zst") {
            fs::remove_file(p).unwrap();
        }
    }
    let p = plan(&pkg_decl(&["a"]), r.path());
    assert!(p.changes.iter().any(|c| c.summary.contains("already in the store")));
    apply(&p).unwrap();
    assert!(view(r.path(), "usr/bin/a").exists());
}

#[test]
fn packages_without_a_repository_are_a_planning_error() {
    let r = root();
    assert!(matches!(
        plan_system(&decl("packages = [\"kitty\"]\n"), r.path(), opts()),
        Err(PlanError::NoRepo)
    ));
}

#[test]
fn unknown_packages_and_missing_indexes_are_planning_errors_and_change_nothing() {
    let r = root();
    publish(r.path(), "a", &[("usr/bin/a", "a")], &[]);
    let err = plan_system(&pkg_decl(&["nosuchpkg"]), r.path(), opts()).unwrap_err();
    assert!(err.to_string().contains("nosuchpkg"), "{err}");

    let d = decl("repos = [\"/nowhere\"]\npackages = [\"a\"]\n");
    assert!(matches!(plan_system(&d, r.path(), opts()), Err(PlanError::Store(_))));
    assert!(!r.path().join("forge").exists());
}

#[test]
fn a_tampered_archive_is_rejected_and_nothing_is_installed() {
    let r = root();
    publish(r.path(), "a", &[("usr/bin/a", "a")], &[]);
    let archive = fs::read_dir(r.path().join("srv/repo"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "zst"))
        .unwrap();
    let mut bytes = fs::read(&archive).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&archive, bytes).unwrap();

    let p = plan(&pkg_decl(&["a"]), r.path());
    let err = apply(&p).unwrap_err();
    assert!(err.summary.contains("install from"), "{err}");
    assert!(Store::new(r.path().join("forge/store")).list().unwrap().is_empty());
    assert_eq!(system_profile(r.path()).current().unwrap(), None);
}

#[test]
fn two_packages_providing_the_same_file_fail_without_changing_the_profile() {
    let r = root();
    publish(r.path(), "a", &[("usr/bin/tool", "from a")], &[]);
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap();
    publish(r.path(), "b", &[("usr/bin/tool", "from b")], &[]);

    let err = apply(&plan(&pkg_decl(&["a", "b"]), r.path())).unwrap_err();
    assert!(err.to_string().contains("provided by both"), "{err}");
    assert_eq!(fs::read_to_string(view(r.path(), "usr/bin/tool")).unwrap(), "from a");
    assert_eq!(system_profile(r.path()).generations().unwrap().len(), 1);
}

#[test]
fn corrupt_state_file_is_reported_not_ignored() {
    let r = root();
    fs::create_dir_all(r.path().join("var/lib/forge")).unwrap();
    fs::write(r.path().join("var/lib/forge/state.toml"), "managed_services = 3").unwrap();
    assert!(matches!(
        plan_system(&decl(""), r.path(), opts()),
        Err(PlanError::BadState { .. })
    ));
}

// ---- repository trust ------------------------------------------------------

/// A one-package repository at `<root>/srv/repo`, signed by `signer` if given.
fn publish_as(root: &Path, name: &str, signer: Option<&SecretKey>) {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let src = scratch.path().join("src/usr/bin");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(name), name).unwrap();
    fs::set_permissions(src.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    let opts = PackOptions {
        name: name.into(),
        version: "1.0".into(),
        libc: forge_schema::Libc::Musl,
        hash: None,
        deps: vec![],
    };
    let out = scratch.path().join("pkg.tar.zst");
    pack_dir(&scratch.path().join("src"), &opts, &out).unwrap();
    LocalRepo::new(root.join("srv/repo")).publish(&out, signer).unwrap();
}

fn store_is_empty(r: &Path) -> bool {
    Store::new(r.join("forge/store")).list().unwrap().is_empty()
}

#[test]
fn packages_from_an_unsigned_repository_are_refused_and_nothing_is_touched() {
    let r = root();
    publish_as(r.path(), "a", None);
    for options in [opts(), Options::default()] {
        let err = plan_system(&pkg_decl(&["a"]), r.path(), options).unwrap_err();
        assert!(err.to_string().contains("no signature"), "{err}");
    }
    assert!(store_is_empty(r.path()));
    assert_eq!(system_profile(r.path()).current().unwrap(), None);
}

#[test]
fn allow_unsigned_is_an_explicit_insecure_opt_out() {
    let r = root();
    publish_as(r.path(), "a", None);
    let insecure = Options { trust: Trust::AllowUnsigned, ..Options::default() };
    let p = plan_system(&pkg_decl(&["a"]), r.path(), insecure).unwrap();
    apply(&p).unwrap();
    assert!(view(r.path(), "usr/bin/a").exists());
    // An unsigned index has no trustworthy serial, so none is remembered.
    let state = fs::read_to_string(r.path().join("var/lib/forge/state.toml")).unwrap_or_default();
    assert!(!state.contains("repo_serials"), "{state}");
}

#[test]
fn a_repository_signed_by_an_untrusted_key_is_refused() {
    let r = root();
    let stranger = SecretKey::from_seed([77; 32]);
    publish_as(r.path(), "a", Some(&stranger));
    let err = plan_system(&pkg_decl(&["a"]), r.path(), opts()).unwrap_err();
    assert!(err.to_string().contains("not among"), "{err}");
    assert!(store_is_empty(r.path()));
}

#[test]
fn an_index_altered_after_signing_is_refused() {
    let r = root();
    publish_as(r.path(), "a", Some(&test_key()));
    let index = r.path().join("srv/repo/index.toml");
    let text = fs::read_to_string(&index).unwrap();
    let hash_line = text.lines().find(|l| l.starts_with("archive_sha256")).unwrap().to_string();
    // The attacker points the index at a different archive hash.
    fs::write(&index, text.replace(&hash_line, &format!("archive_sha256 = \"{}\"", "0".repeat(64)))).unwrap();
    let err = plan_system(&pkg_decl(&["a"]), r.path(), opts()).unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");
    assert!(store_is_empty(r.path()));
}

#[test]
fn the_serial_of_a_signed_repository_is_remembered() {
    let r = root();
    publish_as(r.path(), "a", Some(&test_key()));
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap();
    let state = read(r.path(), "var/lib/forge/state.toml");
    assert!(state.contains("[repo_serials]") && state.contains("\"/srv/repo\" = 1"), "{state}");
}

#[test]
fn an_older_but_genuinely_signed_index_is_refused_as_a_rollback() {
    let r = root();
    let repo = r.path().join("srv/repo");
    publish_as(r.path(), "a", Some(&test_key())); // serial 1
    let (old_index, old_sig) = (fs::read(repo.join("index.toml")).unwrap(), fs::read(repo.join("index.toml.sig")).unwrap());
    publish_as(r.path(), "b", Some(&test_key())); // serial 2
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap(); // remembers serial 2

    // The attacker replays the old index with its (valid!) old signature.
    fs::write(repo.join("index.toml"), &old_index).unwrap();
    fs::write(repo.join("index.toml.sig"), &old_sig).unwrap();
    let err = plan_system(&pkg_decl(&["a"]), r.path(), opts()).unwrap_err();
    assert!(err.to_string().contains("went backwards") && err.to_string().contains("serial 1"), "{err}");

    // Allowing unsigned skips the signature and with it the serial; that is its documented cost.
    let insecure = Options { trust: Trust::AllowUnsigned, ..Options::default() };
    assert!(plan_system(&pkg_decl(&["a"]), r.path(), insecure).is_ok());
}

#[test]
fn the_same_serial_again_is_not_a_rollback_and_newer_ones_are_remembered() {
    let r = root();
    publish_as(r.path(), "a", Some(&test_key()));
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap();
    plan_system(&pkg_decl(&["a"]), r.path(), opts()).expect("the same serial is fine");
    publish_as(r.path(), "b", Some(&test_key()));
    apply(&plan(&pkg_decl(&["a", "b"]), r.path())).unwrap();
    assert!(read(r.path(), "var/lib/forge/state.toml").contains("\"/srv/repo\" = 2"));
}

#[test]
fn repositories_are_only_consulted_when_packages_are_declared() {
    let r = root();
    publish_as(r.path(), "a", None); // unsigned, but nothing is asked of it
    let p = plan_system(&pkg_decl(&[]), r.path(), opts()).unwrap();
    assert!(p.changes.iter().all(|c| c.section != "packages"));
}

// ---- repositories served over HTTP(S) ---------------------------------------------------------------

use forge_store::{Fetch, FetchHandle, StoreError as SErr};
use std::sync::{Arc, Mutex};

/// Serves `<root>/srv/repo` as `https://example.test/repo/...` and remembers what was asked for.
struct Served {
    dir: std::path::PathBuf,
    log: Mutex<Vec<String>>,
}

impl Served {
    fn path(&self, url: &str) -> Result<std::path::PathBuf, SErr> {
        self.log.lock().unwrap().push(url.to_string());
        let name = url
            .strip_prefix("https://example.test/repo/")
            .ok_or_else(|| SErr::Fetch { url: url.into(), reason: "unknown host".into() })?;
        Ok(self.dir.join(name))
    }
}

impl Fetch for Served {
    fn get(&self, url: &str) -> Result<Vec<u8>, SErr> {
        fs::read(self.path(url)?).map_err(|e| SErr::Fetch { url: url.into(), reason: e.to_string() })
    }
    fn download(&self, url: &str, dest: &Path) -> Result<(), SErr> {
        fs::copy(self.path(url)?, dest).map(drop).map_err(|e| SErr::Fetch { url: url.into(), reason: e.to_string() })
    }
}

fn remote_decl(names: &[&str]) -> SystemDecl {
    let list: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    SystemDecl::parse(&format!(
        "hostname = \"x\"\nlibc = \"musl\"\nrepos = [\"https://example.test/repo\"]\npackages = [{}]\n",
        list.join(", ")
    ))
    .unwrap()
}

fn remote_opts(r: &Path) -> (Options, Arc<Served>) {
    let served = Arc::new(Served { dir: r.join("srv/repo"), log: Default::default() });
    let opts = Options { rebuild: false, trust: trust_of(&[&test_key()]), fetch: FetchHandle(served.clone()), adopt: false, upgrade: false, purge: false, base: vec![] };
    (opts, served)
}

#[test]
fn packages_from_a_remote_repository_are_downloaded_verified_and_linked() {
    let r = root();
    publish_as(r.path(), "tool", Some(&test_key()));
    let (opts, served) = remote_opts(r.path());
    let plan = plan_system(&remote_decl(&["tool"]), r.path(), opts).unwrap();
    // Planning reads the index and its signature only: no archive is fetched before you agree.
    let asked = served.log.lock().unwrap().clone();
    assert_eq!(asked, ["https://example.test/repo/index.toml", "https://example.test/repo/index.toml.sig"]);
    assert!(store_is_empty(r.path()), "planning changes nothing");
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert!(shown.iter().any(|s| s.contains("https://example.test/repo/tool-1.0-")), "{shown:?}");

    apply(&plan).unwrap();
    let asked = served.log.lock().unwrap().clone();
    assert!(asked.last().unwrap().ends_with(".tar.zst"), "the archive was downloaded when applying: {asked:?}");
    assert!(!store_is_empty(r.path()));
    assert!(r.path().join("usr/bin/tool").exists(), "copied into the system tree");
    assert!(system_profile(r.path()).current().unwrap().is_some(), "and recorded as a generation");
    assert!(!r.path().join("forge/store/.downloads").read_dir().map(|mut d| d.next().is_some()).unwrap_or(false), "no download debris");
}

#[test]
fn a_tampered_remote_index_is_refused_and_nothing_is_touched() {
    let r = root();
    publish_as(r.path(), "tool", Some(&test_key()));
    let idx = r.path().join("srv/repo/index.toml");
    let text = fs::read_to_string(&idx).unwrap();
    fs::write(&idx, format!("{text}\n# tampered\n")).unwrap();
    let (opts, served) = remote_opts(r.path());
    let err = plan_system(&remote_decl(&["tool"]), r.path(), opts).unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");
    assert!(store_is_empty(r.path()));
    assert!(served.log.lock().unwrap().iter().all(|u| !u.ends_with(".tar.zst")), "no archive was fetched");
}

#[test]
fn a_remote_archive_that_does_not_match_the_signed_index_installs_nothing() {
    let r = root();
    publish_as(r.path(), "tool", Some(&test_key()));
    let (opts, _) = remote_opts(r.path());
    let plan = plan_system(&remote_decl(&["tool"]), r.path(), opts).unwrap();
    for e in fs::read_dir(r.path().join("srv/repo")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "zst") {
            fs::write(&p, b"not the package the index promised").unwrap();
        }
    }
    assert!(apply(&plan).is_err());
    assert!(store_is_empty(r.path()));
}

#[test]
fn the_serial_of_a_remote_repository_is_remembered_under_its_url() {
    let r = root();
    publish_as(r.path(), "tool", Some(&test_key()));
    let (opts, _) = remote_opts(r.path());
    let plan = plan_system(&remote_decl(&["tool"]), r.path(), opts).unwrap();
    apply(&plan).unwrap();
    let state = fs::read_to_string(r.path().join("var/lib/forge/state.toml")).unwrap_or_default();
    assert!(state.contains("https://example.test/repo"), "{state}");
}

// ---- switching the init system -----------------------------------------------

/// A root the way the image leaves it: OpenRC is the init, with its power scripts, and resolv.conf is a plain file.
fn image_root() -> TempDir {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let r = root();
    let sbin = r.path().join("usr/sbin");
    fs::create_dir_all(&sbin).unwrap();
    symlink("openrc-init", sbin.join("init")).unwrap();
    for (path, entry) in forge_schema::plumbing("openrc") {
        if let forge_schema::Entry::File { content, mode } = entry {
            let p = r.path().join(path.trim_start_matches('/'));
            fs::write(&p, content).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        }
    }
    fs::write(r.path().join("etc/resolv.conf"), "nameserver 1.1.1.1\n").unwrap();
    r
}

fn publish_all(root: &Path, names: &[&str]) {
    for n in names {
        publish_as(root, n, Some(&test_key()));
    }
}

fn init_decl(system: &str, packages: &[&str]) -> SystemDecl {
    let list = packages.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
    decl(&format!("init = \"{system}\"\nrepos = [\"/srv/repo\"]\npackages = [{list}]\nservices = [\"sshd\"]\n"))
}

fn names_in_profile(r: &Path) -> Vec<String> {
    let mut v: Vec<_> = system_profile(r).current_packages().unwrap().iter().map(|id| id.name().to_string()).collect();
    v.sort();
    v
}

#[test]
fn an_openrc_system_that_stays_openrc_is_not_touched() {
    let r = image_root();
    let p = plan(&decl(""), r.path());
    assert!(p.changes.iter().all(|c| c.section != "init"), "{:?}", p.changes);
}

#[test]
fn switching_to_systemd_swaps_the_packages_the_init_links_and_the_default_units() {
    let r = image_root();
    publish_all(r.path(), &["openrc", "chrony", "systemd", "dbus", "a"]);
    // The image declared OpenRC among its packages.
    apply(&plan(&pkg_decl(&["openrc", "a"]), r.path())).unwrap();
    assert_eq!(names_in_profile(r.path()), ["a", "openrc"]);

    let p = plan(&init_decl("systemd", &["openrc", "a"]), r.path());
    assert!(p.changes.iter().any(|c| c.section == "init"));
    apply(&p).unwrap();

    assert_eq!(names_in_profile(r.path()), ["a", "dbus", "systemd"], "openrc is gone, systemd and dbus are in");
    assert!(!view(r.path(), "usr/bin/openrc").exists(), "OpenRC's files were removed from the tree");
    assert!(view(r.path(), "usr/bin/systemd").exists());
    let link = |rel: &str| fs::read_link(r.path().join(rel)).unwrap();
    assert_eq!(link("usr/sbin/init"), Path::new("../lib/systemd/systemd"));
    assert!(fs::read_to_string(r.path().join("usr/sbin/reboot")).unwrap().contains("systemctl reboot"), "reboot works under either init");
    assert_eq!(link("etc/systemd/system/multi-user.target.wants/dbus.service"), Path::new("/usr/lib/systemd/system/dbus.service"));
    assert_eq!(link("etc/systemd/system/sockets.target.wants/dbus.socket"), Path::new("/usr/lib/systemd/system/dbus.socket"));
    assert_eq!(link("etc/systemd/system/multi-user.target.wants/sshd.service"), Path::new("/usr/lib/systemd/system/sshd.service"));
    assert!(fs::read_link(r.path().join("etc/resolv.conf")).is_err(), "no link to systemd-resolved: dhcpcd writes resolv.conf");
    assert_eq!(read(r.path(), "etc/vconsole.conf"), "KEYMAP=us\n");
    assert!(read(r.path(), "etc/systemd/system/forge-network.service").contains("netwatch"), "the same network engine as under OpenRC");
    assert_eq!(link("etc/systemd/system/multi-user.target.wants/forge-network.service"), Path::new("/etc/systemd/system/forge-network.service"));
    assert!(!r.path().join("etc/systemd/system/multi-user.target.wants/systemd-networkd.service").exists(), "networkd is not used");
    assert!(read(r.path(), "var/lib/forge/state.toml").contains("init = \"systemd\""));

    // Nothing more to do the second time.
    assert!(plan(&init_decl("systemd", &["openrc", "a"]), r.path()).is_empty());
}

#[test]
fn switching_back_to_openrc_restores_its_scripts_and_drops_systemd() {
    use std::os::unix::fs::PermissionsExt;
    let r = image_root();
    publish_all(r.path(), &["openrc", "chrony", "systemd", "dbus", "a"]);
    apply(&plan(&pkg_decl(&["openrc", "a"]), r.path())).unwrap();
    apply(&plan(&init_decl("systemd", &["a"]), r.path())).unwrap();
    apply(&plan(&init_decl("openrc", &["a"]), r.path())).unwrap();

    assert_eq!(names_in_profile(r.path()), ["a", "chrony", "openrc"]);
    assert_eq!(fs::read_link(r.path().join("usr/sbin/init")).unwrap(), Path::new("openrc-init"));
    let reboot = r.path().join("usr/sbin/reboot");
    assert!(reboot.symlink_metadata().unwrap().is_file(), "the link became a script again");
    assert_eq!(fs::metadata(&reboot).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read_to_string(&reboot).unwrap(), forge_schema::power_script("reboot"));
    assert!(!r.path().join("etc/systemd/system/multi-user.target.wants/dbus.service").exists());
    assert!(fs::read_link(r.path().join("etc/resolv.conf")).is_err(), "no stub link, DHCP writes the file");
    assert!(plan(&init_decl("openrc", &["a"]), r.path()).is_empty());
}

#[test]
fn an_unknown_init_system_never_reaches_the_planner() {
    assert!(SystemDecl::parse("hostname = \"x\"\nlibc = \"glibc\"\ninit = \"runit\"\n").is_err());
}

// ---- CPU microcode ------------------------------------------------------------

#[test]
fn the_microcode_image_is_copied_to_boot_and_again_only_when_it_changes() {
    unsafe { std::env::set_var("FORGE_CPU_VENDOR", "AuthenticAMD") };
    let r = root();
    fs::create_dir_all(r.path().join("boot")).unwrap();
    fs::create_dir_all(r.path().join("usr/lib/microcode")).unwrap();
    fs::write(r.path().join("usr/lib/microcode/early-amd.cpio"), "ONE").unwrap();
    fs::write(r.path().join("usr/lib/microcode/early-intel.cpio"), "INTEL").unwrap();
    publish_all(r.path(), &["microcode"]);
    let d = pkg_decl(&["microcode"]);
    let p = plan(&d, r.path());
    assert!(p.changes.iter().any(|c| c.section == "boot" && c.summary.contains("microcode.img")), "{:?}", p.changes);
    apply(&p).unwrap();
    assert_eq!(fs::read_to_string(r.path().join("boot/microcode.img")).unwrap(), "ONE", "the image for this CPU's vendor");
    assert!(plan(&d, r.path()).is_empty(), "nothing to do the second time");
    fs::write(r.path().join("usr/lib/microcode/early-amd.cpio"), "TWO").unwrap();
    let p = plan(&d, r.path());
    assert!(p.changes.iter().any(|c| c.section == "boot"), "a changed image is copied again");
    apply(&p).unwrap();
    assert_eq!(fs::read_to_string(r.path().join("boot/microcode.img")).unwrap(), "TWO");
}

// ---- files forge keeps right outside the packages ------------------------------------

#[test]
fn programs_and_services_forge_owns_are_kept_current_and_your_config_is_left_alone() {
    let r = image_root();
    let p = plan(&decl(""), r.path());
    assert!(p.changes.iter().any(|c| c.section == "defaults" && c.summary == "/usr/bin/wifi"), "{:?}", p.changes);
    apply(&p).unwrap();
    assert!(read(r.path(), "usr/bin/wifi").starts_with("#!/bin/sh"));
    assert!(read(r.path(), "etc/init.d/network").contains("openrc-run"));
    assert_eq!(fs::read_link(r.path().join("etc/runlevels/default/network")).unwrap(), Path::new("/etc/init.d/network"));
    assert!(plan(&decl(""), r.path()).is_empty(), "nothing to do the second time");

    // An old copy of a program is replaced; a link someone removed comes back; your own config stays.
    fs::write(r.path().join("usr/bin/wifi"), "an old version").unwrap();
    fs::remove_file(r.path().join("etc/runlevels/default/chronyd")).unwrap();
    fs::write(r.path().join("etc/conf.d/network"), "interfaces=\"wlan0\"\n").unwrap();
    let p = plan(&decl(""), r.path());
    assert!(p.changes.iter().any(|c| c.summary == "/usr/bin/wifi") && p.changes.iter().any(|c| c.summary.contains("chronyd")), "{:?}", p.changes);
    assert!(p.changes.iter().all(|c| !c.summary.contains("conf.d/network")), "config is never touched once it exists");
    apply(&p).unwrap();
    assert!(read(r.path(), "usr/bin/wifi").starts_with("#!/bin/sh"));
    assert_eq!(read(r.path(), "etc/conf.d/network"), "interfaces=\"wlan0\"\n");
}

#[test]
fn systemd_machines_are_not_given_openrc_services() {
    let r = image_root();
    publish_all(r.path(), &["systemd", "dbus"]);
    let p = plan(&init_decl("systemd", &[]), r.path());
    let defaults: Vec<_> = p.changes.iter().filter(|c| c.section == "defaults").map(|c| c.summary.clone()).collect();
    assert!(defaults.iter().any(|s| s == "/usr/bin/wifi"), "the shared defaults still apply: {defaults:?}");
    assert!(defaults.iter().all(|s| !s.contains("init.d") && !s.contains("runlevels")), "no OpenRC services for systemd: {defaults:?}");
}

// ---- the boot loader ------------------------------------------------------------------

#[test]
fn the_limine_files_on_the_boot_partition_follow_the_package() {
    let r = root();
    fs::create_dir_all(r.path().join("boot/EFI/BOOT")).unwrap();
    fs::create_dir_all(r.path().join("usr/share/limine")).unwrap();
    fs::write(r.path().join("boot/EFI/BOOT/BOOTX64.EFI"), "OLD").unwrap();
    fs::write(r.path().join("usr/share/limine/BOOTX64.EFI"), "NEW").unwrap();
    publish_all(r.path(), &["limine"]);
    let d = pkg_decl(&["limine"]);
    let p = plan(&d, r.path());
    assert!(p.changes.iter().any(|c| c.section == "boot" && c.summary.contains("BOOTX64.EFI")), "{:?}", p.changes);
    apply(&p).unwrap();
    assert_eq!(fs::read_to_string(r.path().join("boot/EFI/BOOT/BOOTX64.EFI")).unwrap(), "NEW");
    assert!(plan(&d, r.path()).is_empty(), "already current");
    // Not touched when limine is not part of the system.
    fs::write(r.path().join("boot/EFI/BOOT/BOOTX64.EFI"), "OLD").unwrap();
    let q = plan(&pkg_decl(&[]), r.path());
    assert!(q.changes.iter().all(|c| c.section != "boot"));
}

#[test]
fn with_secure_boot_the_signed_loader_is_never_replaced_by_the_packages_copy() {
    let r = root();
    fs::create_dir_all(r.path().join("boot/EFI/BOOT")).unwrap();
    fs::create_dir_all(r.path().join("usr/share/limine")).unwrap();
    fs::create_dir_all(r.path().join("etc/forge/secureboot")).unwrap();
    fs::write(r.path().join("etc/forge/secureboot/enabled"), "").unwrap();
    fs::write(r.path().join("boot/EFI/BOOT/BOOTX64.EFI"), "SIGNED").unwrap();
    fs::write(r.path().join("usr/share/limine/BOOTX64.EFI"), "NEW").unwrap();
    publish_all(r.path(), &["limine"]);
    let p = plan(&pkg_decl(&["limine"]), r.path());
    assert!(p.changes.iter().all(|c| !c.summary.contains("the Limine boot loader")), "no plain copy: {:?}", p.changes);
    apply(&p).unwrap();
    assert_eq!(fs::read_to_string(r.path().join("boot/EFI/BOOT/BOOTX64.EFI")).unwrap(), "SIGNED");
}

// ---- moving to a new signing key ------------------------------------------------------

#[test]
fn a_new_signing_key_announced_by_the_current_one_is_trusted_from_then_on() {
    let r = root();
    let old = test_key();
    let new = SecretKey::from_seed([42; 32]);
    publish_as(r.path(), "a", Some(&old));
    let repo = LocalRepo::new(r.path().join("srv/repo"));
    repo.sign_announcing(&old, 0, &[new.public()], &[]).unwrap();
    let keys = r.path().join("etc/forge/keys");
    fs::create_dir_all(&keys).unwrap();
    fs::write(keys.join("old.pub"), forge_store::public_key_file_text(&old.public())).unwrap();
    let d = pkg_decl(&["a"]);
    let mut o = opts();
    o.trust = Trust::Require(Keyring::load_dir(&keys).unwrap());
    let p = plan_system(&d, r.path(), o).unwrap();
    assert!(p.changes.iter().any(|c| c.section == "keys" && c.summary.contains(&new.public().id())), "{:?}", p.changes);
    apply(&p).unwrap();
    let ring = Keyring::load_dir(&keys).unwrap();
    assert!(ring.ids().contains(&new.public().id()), "the announced key is in the keyring now");

    // The repository can now be signed by the new key alone, and the system still trusts it.
    repo.sign(&new).unwrap();
    let loaded = repo.load(&Trust::Require(ring)).unwrap();
    assert_eq!(loaded.signed_by, Some(new.public().id()));
}

#[test]
fn keys_announced_by_an_unsigned_repository_are_ignored() {
    let r = root();
    let stranger = SecretKey::from_seed([77; 32]);
    publish_as(r.path(), "a", None);
    LocalRepo::new(r.path().join("srv/repo")).sign_announcing(&stranger, 0, &[SecretKey::from_seed([5; 32]).public()], &[]).unwrap();
    // Signed by a key nobody trusts: refused outright, so nothing it announces can matter.
    assert!(plan_system(&pkg_decl(&["a"]), r.path(), opts()).is_err());
}

// ---- revoking a signing key -----------------------------------------------------------

#[test]
fn a_revoked_key_is_deleted_remembered_and_cannot_be_announced_back() {
    let r = root();
    let good = test_key();
    let leaked = SecretKey::from_seed([66; 32]);
    publish_as(r.path(), "a", Some(&good));
    let repo = LocalRepo::new(r.path().join("srv/repo"));
    let keys = r.path().join("etc/forge/keys");
    fs::create_dir_all(&keys).unwrap();
    fs::write(keys.join("good.pub"), forge_store::public_key_file_text(&good.public())).unwrap();
    fs::write(keys.join("leaked.pub"), forge_store::public_key_file_text(&leaked.public())).unwrap();
    let leaked_id = leaked.public().id();
    repo.sign_announcing(&good, 0, &[], &[leaked_id.clone()]).unwrap();
    let with_ring = || {
        let mut o = opts();
        o.trust = Trust::Require(Keyring::load_dir(&keys).unwrap());
        o
    };
    let p = plan_system(&pkg_decl(&["a"]), r.path(), with_ring()).unwrap();
    assert!(p.changes.iter().any(|c| c.section == "keys" && c.kind == Kind::Remove && c.summary.contains(&leaked_id)), "{:?}", p.changes);
    apply(&p).unwrap();
    assert!(!keys.join("leaked.pub").exists() && keys.join("good.pub").exists());
    assert!(forge_store::revoked_ids(&keys).contains(&leaked_id));
    assert!(!Keyring::load_dir(&keys).unwrap().ids().contains(&leaked_id), "no longer trusted");
    // Even if the file comes back (an old image) or an old index announces it, it stays out.
    fs::write(keys.join("leaked.pub"), forge_store::public_key_file_text(&leaked.public())).unwrap();
    assert!(!Keyring::load_dir(&keys).unwrap().ids().contains(&leaked_id));
    repo.sign_announcing(&good, 0, &[leaked.public()], &[]).unwrap();
    let again = plan_system(&pkg_decl(&["a"]), r.path(), with_ring()).unwrap();
    assert!(again.changes.iter().all(|c| !c.summary.contains("a new signing key")), "{:?}", again.changes);
}

#[test]
fn an_index_cannot_revoke_the_key_that_signed_it() {
    let r = root();
    let good = test_key();
    publish_as(r.path(), "a", Some(&good));
    let repo = LocalRepo::new(r.path().join("srv/repo"));
    assert!(repo.sign_announcing(&good, 0, &[], &[good.public().id()]).is_err(), "refused when signing");
    assert!(repo.sign_announcing(&good, 0, &[], &["not-a-key-id".into()]).is_err());
}

// ---- the init system that is running keeps its packages until the reboot -------------------

#[test]
fn the_running_init_system_is_not_removed_until_the_machine_has_rebooted_into_the_new_one() {
    let d = init_decl("systemd", &["a"]);
    let base: Vec<String> = vec![];
    // OpenRC is running and systemd is declared: both stay installed, so reboot and shutdown still work.
    let names = effective_packages(&d, false, &base, Some("openrc"));
    assert!(names.contains(&"openrc".to_string()) && names.contains(&"chrony".to_string()) && names.contains(&"systemd".to_string()), "{names:?}");
    // After the reboot into systemd, the old one goes.
    let names = effective_packages(&d, true, &base, Some("systemd"));
    assert!(!names.contains(&"openrc".to_string()) && names.contains(&"systemd".to_string()), "{names:?}");
    // Not knowing what runs (a root that is not `/`) keeps the swap immediate.
    let names = effective_packages(&init_decl("systemd", &["openrc", "a"]), false, &base, None);
    assert!(!names.contains(&"openrc".to_string()), "{names:?}");
}

#[test]
fn the_firewall_forwards_and_services_come_from_the_declaration() {
    let r = image_root();
    let d = decl("packages = []\nswap = \"zram\"\n[firewall]\nallow = [\"ssh\"]\n[[firewall.forward]]\nport = 8080\nto = \"192.168.1.50:80\"\n");
    apply(&plan(&d, r.path())).unwrap();
    let conf = read(r.path(), "etc/nftables.conf");
    assert!(conf.contains("tcp dport 22 accept") && conf.contains("dnat to 192.168.1.50:80"), "{conf}");
    assert_eq!(fs::read_link(r.path().join("etc/runlevels/default/nftables")).unwrap(), Path::new("/etc/init.d/nftables"));
    assert!(read(r.path(), "etc/init.d/nftables").contains("ip_forward"));
    assert!(r.path().join("usr/libexec/singularity/zram").exists() && r.path().join("usr/libexec/singularity/maintenance-loop").exists());
    assert!(plan(&d, r.path()).changes.iter().all(|c| c.section != "defaults"), "second apply changes nothing");

    // Turning things off removes them again.
    let off = decl("packages = []\nswap = \"none\"\n[firewall]\nenable = false\n[maintenance]\nenable = false\n");
    apply(&plan(&off, r.path())).unwrap();
    for p in ["etc/nftables.conf", "etc/runlevels/default/nftables", "etc/init.d/zram", "etc/init.d/forge-maintenance"] {
        assert!(r.path().join(p).symlink_metadata().is_err(), "{p} should be gone");
    }
}

#[test]
fn the_kernel_package_stays_when_the_declaration_does_not_list_it() {
    let r = root();
    publish_all(r.path(), &["linux", "a"]);
    apply(&plan(&pkg_decl(&["linux", "a"]), r.path())).unwrap();
    // The same system, declared without the kernel (a new system's first declaration): nothing to remove.
    let p = plan(&pkg_decl(&["a"]), r.path());
    assert!(p.changes.iter().all(|c| c.kind != Kind::Remove), "{:?}", p.changes);
    assert!(p.is_empty());
}

// ---- installed packages only change when you ask --------------------------------------

fn publish_v(root: &Path, name: &str, version: &str, content: &str) {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let src = scratch.path().join("src/usr/bin");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(name), content).unwrap();
    fs::set_permissions(src.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    let opts = PackOptions { name: name.into(), version: version.into(), libc: forge_schema::Libc::Musl, hash: None, deps: vec![] };
    let out = scratch.path().join("pkg.tar.zst");
    pack_dir(&scratch.path().join("src"), &opts, &out).unwrap();
    LocalRepo::new(root.join("srv/repo")).publish(&out, Some(&test_key())).unwrap();
}

#[test]
fn applying_or_installing_never_moves_a_package_you_already_have_but_upgrade_does() {
    let r = root();
    publish_v(r.path(), "a", "1.0", "old a");
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap();
    assert_eq!(read(r.path(), "usr/bin/a"), "old a");

    // The repository now has a newer a, and a new package b.
    publish_v(r.path(), "a", "2.0", "new a");
    publish_v(r.path(), "b", "1.0", "b");
    let stay = plan(&pkg_decl(&["a"]), r.path());
    assert!(stay.profile_switch.is_none() && stay.changes.iter().all(|c| c.section != "packages"), "what is installed stays: {:?}", stay.changes);
    apply(&plan(&pkg_decl(&["a", "b"]), r.path())).unwrap();
    assert_eq!(read(r.path(), "usr/bin/b"), "b", "a new package is installed");
    assert_eq!(read(r.path(), "usr/bin/a"), "old a", "installing b did not upgrade a");

    // Only an upgrade moves it.
    let up = plan_system(&pkg_decl(&["a", "b"]), r.path(), Options { upgrade: true, ..opts() }).unwrap();
    assert!(!up.is_empty());
    apply(&up).unwrap();
    assert_eq!(read(r.path(), "usr/bin/a"), "new a");
}

#[test]
fn an_installed_package_stays_even_when_the_repository_no_longer_lists_its_version() {
    // A repository holds only the newest build of each package, so after a release the installed one is not in the index.
    let r = root();
    publish_v(r.path(), "a", "1.0", "old a");
    apply(&plan(&pkg_decl(&["a"]), r.path())).unwrap();
    // The repository is replaced by one that has only a 2.0 and a new b.
    fs::remove_dir_all(r.path().join("srv/repo")).unwrap();
    publish_v(r.path(), "a", "2.0", "new a");
    publish_v(r.path(), "b", "1.0", "b");
    apply(&plan(&pkg_decl(&["a", "b"]), r.path())).unwrap();
    assert_eq!(read(r.path(), "usr/bin/a"), "old a", "installing b did not upgrade a");
    assert_eq!(read(r.path(), "usr/bin/b"), "b");
    // Removing b from the declaration still removes it, and a stays.
    let p = plan(&pkg_decl(&["a"]), r.path());
    assert!(p.changes.iter().any(|c| c.kind == Kind::Remove && c.summary.contains("b-1.0")), "{:?}", p.changes);
    apply(&p).unwrap();
    assert_eq!(read(r.path(), "usr/bin/a"), "old a");
    // The owner asks for the newest.
    apply(&plan_system(&pkg_decl(&["a"]), r.path(), Options { upgrade: true, ..opts() }).unwrap()).unwrap();
    assert_eq!(read(r.path(), "usr/bin/a"), "new a");
}

#[test]
fn remove_full_deletes_the_settings_a_removed_package_left_but_plain_remove_keeps_them() {
    let r = root();
    let a = publish(r.path(), "a", &[("etc/a/a.conf", "default"), ("usr/bin/a", "A")], &[]);
    let _ = a;
    publish(r.path(), "b", &[("etc/b.conf", "x"), ("usr/bin/b", "B")], &[]);
    apply(&plan(&pkg_decl(&["a", "b"]), r.path())).unwrap();
    fs::write(r.path().join("etc/a/a.conf"), "edited").unwrap();
    fs::create_dir_all(r.path().join("var/lib/a")).unwrap();
    fs::write(r.path().join("var/lib/a/state"), "s").unwrap();

    // a plain remove leaves all of it
    let p = plan(&pkg_decl(&["b"]), r.path());
    assert!(!p.changes.iter().any(|c| c.section == "config"));
    // --full lists the removed package's settings, not those of a package that stays
    let full = |names: &[&str]| plan_system(&pkg_decl(names), r.path(), Options { purge: true, ..opts() }).unwrap();
    let p = full(&["b"]);
    let text: Vec<&str> = p.changes.iter().filter(|c| c.section == "config").map(|c| c.summary.as_str()).collect();
    assert!(text.iter().any(|s| s.starts_with("/etc/a/a.conf")), "{text:?}");
    assert!(text.iter().any(|s| s.starts_with("/var/lib/a ")), "{text:?}");
    apply(&p).unwrap();
    assert!(!r.path().join("etc/a").exists(), "the file and the directory it left empty are gone");
    assert!(!r.path().join("var/lib/a").exists());
    assert!(r.path().join("etc/b.conf").exists(), "b stays, so do its settings");
    assert!(!r.path().join("usr/bin/a").exists());
}
