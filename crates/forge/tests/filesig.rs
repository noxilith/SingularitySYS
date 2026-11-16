//! `forge key sign` and `forge key verify`: the way an ISO is signed and checked.
use std::fs;
use std::process::Command;

fn forge(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_forge")).args(args).output().unwrap()
}

#[test]
fn a_signed_file_verifies_and_any_change_or_other_key_does_not() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let keys = d.join("keys");
    assert!(forge(&["key", "generate", "rel", "--dir", keys.to_str().unwrap()]).status.success());
    assert!(forge(&["key", "generate", "other", "--dir", keys.to_str().unwrap()]).status.success());
    let iso = d.join("a.iso");
    fs::write(&iso, vec![7u8; 3_000_000]).unwrap();

    let o = forge(&["key", "sign", iso.to_str().unwrap(), "--key", keys.join("rel.key").to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(d.join("a.iso.sig").is_file());

    let ok = |extra: &[&str]| {
        let mut a = vec!["key", "verify", iso.to_str().unwrap()];
        a.extend_from_slice(extra);
        forge(&a)
    };
    let rel_pub = keys.join("rel.pub");
    assert!(ok(&["--pub", rel_pub.to_str().unwrap()]).status.success(), "the right key accepts it");
    assert!(!ok(&["--pub", keys.join("other.pub").to_str().unwrap()]).status.success(), "another key does not");

    // A ring directory works like --pub.
    let ring = d.join("ring");
    fs::create_dir_all(&ring).unwrap();
    fs::copy(&rel_pub, ring.join("rel.pub")).unwrap();
    assert!(ok(&["--dir", ring.to_str().unwrap()]).status.success());

    // One changed byte breaks it.
    let mut bytes = fs::read(&iso).unwrap();
    bytes[1_000_000] ^= 1;
    fs::write(&iso, bytes).unwrap();
    let bad = ok(&["--pub", rel_pub.to_str().unwrap()]);
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("does NOT match"));
}
