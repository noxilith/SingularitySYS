//! Automatic run-time dependencies for dynamically linked packages.
//!
//! A program built against glibc names the shared libraries it needs (`DT_NEEDED`) and its dynamic loader
//! (`PT_INTERP`) in its ELF headers. After the install step, every ELF file in the package is read, each needed
//! library is looked up among the build dependencies' `lib` directories, and the packages that provide them
//! become run dependencies, so installing the package brings along exactly the libraries it links.
//!
//! Only the headers are read (never a whole file), and nothing but `std` is used.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use forge_store::{Store, StoreId};

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;

/// Directories (relative to a package root) where shared libraries and loaders live.
const LIB_DIRS: [&str; 4] = ["usr/lib", "lib", "usr/lib64", "lib64"];

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

fn read_at(f: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// The libraries (and the loader's file name) a little-endian 64-bit ELF file asks for. `None` for anything that
/// is not a dynamically linked executable or shared object.
pub(crate) fn needed_libraries(path: &Path) -> Option<BTreeSet<String>> {
    let mut f = File::open(path).ok()?;
    let head = read_at(&mut f, 0, 64).ok()?;
    // ELF, 64-bit, little-endian; executable or shared object.
    if &head[0..4] != b"\x7fELF" || head[4] != 2 || head[5] != 1 {
        return None;
    }
    let e_type = u16_at(&head, 16);
    if e_type != 2 && e_type != 3 {
        return None;
    }
    let (phoff, phentsize, phnum) = (u64_at(&head, 32), u16_at(&head, 54) as usize, u16_at(&head, 56) as usize);
    if phentsize < 56 || phnum == 0 || phnum > 4096 {
        return None;
    }
    let phdrs = read_at(&mut f, phoff, phentsize * phnum).ok()?;
    let mut out = BTreeSet::new();
    let (mut dynamic, mut loads) = (None, Vec::new());
    for i in 0..phnum {
        let p = &phdrs[i * phentsize..];
        let (kind, offset, vaddr, filesz) = (u32_at(p, 0), u64_at(p, 8), u64_at(p, 16), u64_at(p, 32));
        match kind {
            PT_LOAD => loads.push((vaddr, offset, filesz)),
            PT_DYNAMIC => dynamic = Some((offset, filesz)),
            PT_INTERP => {
                let raw = read_at(&mut f, offset, filesz.min(512) as usize).ok()?;
                let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                let interp = String::from_utf8_lossy(&raw[..end]).into_owned();
                if let Some(name) = interp.rsplit('/').next().filter(|n| !n.is_empty()) {
                    out.insert(name.to_string());
                }
            }
            _ => {}
        }
    }
    let (dyn_off, dyn_size) = dynamic?;
    if dyn_size > 1 << 20 {
        return None;
    }
    let dynamic = read_at(&mut f, dyn_off, dyn_size as usize).ok()?;
    let (mut strtab, mut strsz, mut needed) = (0u64, 0u64, Vec::new());
    for entry in dynamic.chunks_exact(16) {
        let (tag, val) = (u64_at(entry, 0), u64_at(entry, 8));
        match tag {
            DT_NULL => break,
            DT_NEEDED => needed.push(val),
            DT_STRTAB => strtab = val,
            DT_STRSZ => strsz = val,
            _ => {}
        }
    }
    if needed.is_empty() {
        return Some(out);
    }
    // The string table's address is virtual; find the file offset through the segment that holds it.
    let file_off = loads.iter().find(|(v, _, sz)| strtab >= *v && strtab < v + sz).map(|(v, o, _)| o + (strtab - v))?;
    let strings = read_at(&mut f, file_off, strsz.min(1 << 20) as usize).ok()?;
    for off in needed {
        let tail = strings.get(off as usize..)?;
        let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
        out.insert(String::from_utf8_lossy(&tail[..end]).into_owned());
    }
    Some(out)
}

/// Every library name the package's own files need, found by reading all ELF files under `dest`.
fn needed_by_tree(dest: &Path) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    let mut stack = vec![dest.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_dir() {
                stack.push(entry.path());
            } else if ty.is_file()
                && let Some(n) = needed_libraries(&entry.path())
            {
                all.extend(n);
            }
        }
    }
    all
}

/// File names in a package's library directories (symlinks included), for matching against `DT_NEEDED`.
fn provided_names(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for d in LIB_DIRS {
        if let Ok(rd) = fs::read_dir(root.join(d)) {
            names.extend(rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()));
        }
    }
    names
}

/// The build dependencies that provide the libraries the finished package needs, as store ids; also the names
/// nothing provided (the host's, for a build with host tools, or the package's own).
pub(crate) fn auto_run_deps(dest: &Path, build_deps: &BTreeSet<StoreId>, store: &Store) -> (Vec<String>, Vec<String>) {
    let mut needed = needed_by_tree(dest);
    // Libraries the package ships itself need no other package.
    for own in provided_names(dest) {
        needed.remove(&own);
    }
    let mut providers: BTreeMap<String, StoreId> = BTreeMap::new();
    for id in build_deps {
        for name in provided_names(&store.root().join(id.to_string())) {
            providers.entry(name).or_insert_with(|| id.clone());
        }
    }
    let (mut deps, mut missing) = (BTreeSet::new(), Vec::new());
    for n in needed {
        match providers.get(&n) {
            Some(id) => {
                deps.insert(id.to_string());
            }
            None => missing.push(n),
        }
    }
    (deps.into_iter().collect(), missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dynamic_test_binary_names_a_loader_and_libraries() {
        let exe = std::env::current_exe().unwrap();
        let n = needed_libraries(&exe).expect("the test binary is a dynamic ELF");
        assert!(n.iter().any(|s| s.starts_with("ld-linux") || s.starts_with("libc.")), "{n:?}");
    }

    #[test]
    fn text_files_and_missing_files_are_not_elf() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("script");
        fs::write(&p, "#!/bin/sh\necho hi\n").unwrap();
        assert!(needed_libraries(&p).is_none());
        assert!(needed_libraries(&t.path().join("nope")).is_none());
    }

    #[test]
    fn libraries_a_package_ships_itself_are_not_dependencies() {
        let t = tempfile::tempdir().unwrap();
        let dest = t.path().join("dest");
        fs::create_dir_all(dest.join("usr/lib")).unwrap();
        fs::write(dest.join("usr/lib/libthing.so.1"), b"x").unwrap();
        assert!(provided_names(&dest).contains("libthing.so.1"));
        let store = Store::new(t.path().join("store"));
        let (deps, missing) = auto_run_deps(&dest, &BTreeSet::new(), &store);
        assert!(deps.is_empty() && missing.is_empty());
    }
}
