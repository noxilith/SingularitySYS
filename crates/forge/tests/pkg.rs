//! `forge pkg add/remove` against a scratch root with a signed local repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn forge(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forge")).args(args).output().expect("run forge")
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct World {
    dir: tempfile::TempDir,
}

impl World {
    /// A system root with a trusted key and a signed repository holding one package, `tool`.
    fn new() -> Self {
        let w = World { dir: tempfile::tempdir().unwrap() };
        let (keys, root) = (w.p("keys"), w.p("root"));
        let o = forge(&["key", "generate", "k", "--dir", keys.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        let trusted = root.join("etc/forge/keys");
        let o = forge(&["key", "trust", keys.join("k.pub").to_str().unwrap(), "--dir", trusted.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));

        let tree = w.p("tree/usr/bin");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("tool"), "#!/bin/sh\necho tool\n").unwrap();
        let archive = w.p("tool.tar.zst");
        let o = forge(&["store", "pack", w.p("tree").to_str().unwrap(), "--name", "tool", "--version", "1.0", "--libc", "musl", "--out", archive.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        let repo = root.join("srv/repo");
        let o = forge(&["store", "publish", archive.to_str().unwrap(), "--repo", repo.to_str().unwrap(), "--sign", keys.join("k.key").to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));

        fs::create_dir_all(root.join("etc/forge")).unwrap();
        fs::write(root.join("etc/forge/system.toml"), "hostname = \"t\"\nlibc = \"musl\"\nrepos = [\"/srv/repo\"]\npackages = []\n").unwrap();
        w
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn root(&self) -> String {
        self.p("root").to_str().unwrap().to_string()
    }
    fn decl(&self) -> String {
        fs::read_to_string(self.p("root/etc/forge/system.toml")).unwrap()
    }
    fn installed(&self) -> bool {
        let link = self.p("root/forge/profiles/system/current/usr/bin/tool");
        link.exists() || fs::symlink_metadata(&link).is_ok()
    }
}

#[test]
fn pkg_add_edits_the_declaration_and_installs_the_package() {
    let w = World::new();
    let o = forge(&["pkg", "add", "tool", "--root", &w.root(), "-y"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(w.decl().contains("packages = [\"tool\"]"), "{}", w.decl());
    assert!(w.installed(), "the package is linked into the system profile: {}", text(&o));
    let real = w.p("root/usr/bin/tool");
    assert!(real.is_file() && !fs::symlink_metadata(&real).unwrap().file_type().is_symlink(), "a real copy in the system tree");
    assert!(text(&o).contains("applied"), "{}", text(&o));
}

#[test]
fn pkg_remove_unlinks_it_again() {
    let w = World::new();
    assert!(forge(&["pkg", "add", "tool", "--root", &w.root(), "-y"]).status.success());
    let o = forge(&["pkg", "remove", "tool", "--root", &w.root(), "-y"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(w.decl().contains("packages = []"), "{}", w.decl());
    assert!(!w.installed());
    assert!(!w.p("root/usr/bin/tool").exists(), "and gone from the system tree");
}

#[test]
fn a_package_that_does_not_exist_leaves_the_declaration_alone() {
    let w = World::new();
    let before = w.decl();
    let o = forge(&["pkg", "add", "no-such-package", "--root", &w.root(), "-y"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("was not changed"), "{}", text(&o));
    assert_eq!(w.decl(), before);
}

#[test]
fn an_untrusted_repository_changes_nothing() {
    let w = World::new();
    let before = w.decl();
    let empty = w.p("nokeys");
    fs::create_dir_all(&empty).unwrap();
    let o = forge(&["pkg", "add", "tool", "--root", &w.root(), "--keys", empty.to_str().unwrap(), "-y"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("trusted"), "{}", text(&o));
    assert_eq!(w.decl(), before);
    assert!(!w.installed());
}

#[test]
fn no_apply_only_edits_the_declaration() {
    let w = World::new();
    let o = forge(&["pkg", "add", "tool", "--root", &w.root(), "--no-apply"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(w.decl().contains("\"tool\""));
    assert!(!w.installed());
    assert!(text(&o).contains("not applied"), "{}", text(&o));
}

#[test]
fn adding_twice_says_so_and_does_nothing() {
    let w = World::new();
    assert!(forge(&["pkg", "add", "tool", "--root", &w.root(), "-y"]).status.success());
    let o = forge(&["pkg", "add", "tool", "--root", &w.root(), "-y"]);
    assert!(o.status.success());
    assert!(text(&o).contains("already in"), "{}", text(&o));
    let _: &Path = w.dir.path();
}

impl World {
    /// Publish another version of `tool` into the repository.
    fn publish_tool(&self, version: &str) {
        let archive = self.p(&format!("tool-{version}.tar.zst"));
        let o = forge(&["store", "pack", self.p("tree").to_str().unwrap(), "--name", "tool", "--version", version, "--libc", "musl", "--out", archive.to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
        let o = forge(&["store", "publish", archive.to_str().unwrap(), "--repo", self.p("root/srv/repo").to_str().unwrap(), "--sign", self.p("keys/k.key").to_str().unwrap()]);
        assert!(o.status.success(), "{}", text(&o));
    }
}

#[test]
fn install_and_remove_are_top_level_commands_that_take_several_names() {
    let w = World::new();
    let o = forge(&["install", "tool", "--root", &w.root(), "-y"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(w.decl().contains("packages = [\"tool\"]") && w.installed(), "{}", text(&o));
    let o = forge(&["remove", "tool", "--root", &w.root(), "-y"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(w.decl().contains("packages = []") && !w.installed());
    // Several names are one edit and one apply.
    let o = forge(&["install", "tool", "no-such-package", "--root", &w.root(), "-y"]);
    assert!(!o.status.success() && text(&o).contains("was not changed"), "{}", text(&o));
    assert!(w.decl().contains("packages = []"), "a bad name in the list changes nothing: {}", w.decl());
}

#[test]
fn upgrade_moves_declared_packages_to_the_newest_version() {
    let w = World::new();
    assert!(forge(&["install", "tool", "--root", &w.root(), "-y"]).status.success());
    let o = forge(&["upgrade", "--root", &w.root(), "-y"]);
    assert!(text(&o).contains("nothing to do"), "{}", text(&o));
    w.publish_tool("1.1");
    let o = forge(&["upgrade", "--root", &w.root(), "--dry-run"]);
    assert!(text(&o).contains("tool-1.1"), "{}", text(&o));
    let o = forge(&["upgrade", "--root", &w.root(), "-y"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = forge(&["list", "--root", &w.root()]);
    assert!(text(&o).contains("tool-1.1"), "{}", text(&o));
}

#[test]
fn upgrade_with_nothing_installed_says_so() {
    let w = World::new();
    let o = forge(&["upgrade", "--root", &w.root(), "-y"]);
    assert!(o.status.success() && text(&o).contains("nothing to upgrade"), "{}", text(&o));
}

#[test]
fn search_lists_the_newest_version_of_matching_packages() {
    let w = World::new();
    w.publish_tool("1.1");
    let o = forge(&["search", "too", "--root", &w.root()]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("tool") && text(&o).contains("1.1") && !text(&o).contains("1.0"), "{}", text(&o));
    let o = forge(&["search", "zzz", "--root", &w.root()]);
    assert!(text(&o).contains("no package matches"), "{}", text(&o));
}
