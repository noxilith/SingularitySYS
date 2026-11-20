use std::path::Path;
use forge_deb::apt::*;
fn main() -> anyhow::Result<()> {
    let w = Path::new(&std::env::args().nth(1).unwrap()).to_path_buf();
    let keys = w.join("keys");
    let s = Source { name: "debian".into(), url: "https://deb.debian.org/debian".into(), suite: "trixie".into(), components: vec!["main".into()], key: "debian.asc".into() };
    let n = update_source(&w.join("cache"), &keys, &s)?;
    println!("{n} packages");
    let mut srcs = Sources::default(); srcs.sources.push(s);
    let p = load_packages(&w.join("cache"), &srcs)?;
    println!("loaded {}", p.len());
    Ok(())
}
