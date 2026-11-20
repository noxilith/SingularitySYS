//! A safety check on a converted package: does every symbol that its programs and libraries need exist somewhere?
//!
//! Debian's libraries are built with options that this system's own builds of the same libraries may lack (GTK needs
//! cairo's X11 support and libepoxy's GLX entry points, for instance). The program would then start and die with
//! "undefined symbol". Finding that now, at install time, is better than finding it when the program is started.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use object::{Object, ObjectSymbol};

fn is_elf(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() < 64 {
        return None;
    }
    let data = fs::read(path).ok()?;
    data.starts_with(b"\x7fELF").then_some(data)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() && depth < 8 {
            walk(&p, out, depth + 1);
        } else if t.is_file() {
            out.push(p);
        }
    }
}

pub struct Problem {
    pub file: String,
    pub missing: Vec<String>,
}

/// Undefined symbols of the ELF files in `tree` that no ELF file in `tree` or in the system's library directories
/// defines.
pub fn missing_symbols(tree: &Path, root: &Path) -> Vec<Problem> {
    // 1. what the tree needs
    let mut files = Vec::new();
    walk(tree, &mut files, 0);
    let mut needs: HashMap<String, Vec<String>> = HashMap::new(); // file -> undefined symbols
    let mut wanted: HashSet<String> = HashSet::new();
    let mut defined: HashSet<String> = HashSet::new();
    for f in &files {
        let Some(data) = is_elf(f) else { continue };
        let Ok(obj) = object::File::parse(&*data) else { continue };
        let rel = f.strip_prefix(tree).unwrap_or(f).display().to_string();
        let mut list = Vec::new();
        for s in obj.dynamic_symbols() {
            let Ok(name) = s.name() else { continue };
            if name.is_empty() {
                continue;
            }
            if s.is_undefined() {
                if !s.is_weak() {
                    list.push(name.to_string());
                    wanted.insert(name.to_string());
                }
            } else {
                defined.insert(name.to_string());
            }
        }
        if !list.is_empty() {
            needs.insert(rel, list);
        }
    }
    // 2. what the system provides
    let mut libs = Vec::new();
    for d in ["usr/lib", "usr/lib64"] {
        walk(&root.join(d), &mut libs, 0);
    }
    for f in libs {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !name.contains(".so") {
            continue;
        }
        let Some(data) = is_elf(&f) else { continue };
        let Ok(obj) = object::File::parse(&*data) else { continue };
        for s in obj.dynamic_symbols() {
            if s.is_undefined() {
                continue;
            }
            if let Ok(n) = s.name() {
                if wanted.contains(n) {
                    defined.insert(n.to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    for (file, list) in needs {
        let missing: Vec<String> = list.into_iter().filter(|n| !defined.contains(n)).collect();
        if !missing.is_empty() {
            out.push(Problem { file, missing });
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    out
}
