//! Reading `.deb` files: an `ar` archive holding `control.tar.*` and `data.tar.*`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

struct Member {
    name: String,
    offset: u64,
    size: u64,
}

fn members(f: &mut File) -> Result<Vec<Member>> {
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    if &magic != b"!<arch>\n" {
        bail!("not a .deb file (no ar header)");
    }
    let mut out = Vec::new();
    loop {
        let mut h = [0u8; 60];
        match f.read(&mut h[..1])? {
            0 => break,
            _ => f.read_exact(&mut h[1..])?,
        }
        if &h[58..60] != b"`\n" {
            bail!("damaged .deb (bad member header)");
        }
        let name = String::from_utf8_lossy(&h[..16]).trim().trim_end_matches('/').to_string();
        let size: u64 = String::from_utf8_lossy(&h[48..58]).trim().parse().map_err(|_| anyhow!("damaged .deb (bad member size)"))?;
        let offset = f.stream_position()?;
        out.push(Member { name, offset, size });
        f.seek(SeekFrom::Start(offset + size + (size & 1)))?;
    }
    Ok(out)
}

fn open_member(path: &Path, prefix: &str) -> Result<Box<dyn Read>> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let ms = members(&mut f)?;
    let m = ms.iter().find(|m| m.name.starts_with(prefix)).ok_or_else(|| anyhow!("{} has no {prefix}*", path.display()))?;
    f.seek(SeekFrom::Start(m.offset))?;
    let raw = f.take(m.size);
    let name = m.name.clone();
    if name.ends_with(".gz") {
        Ok(Box::new(flate2::read::GzDecoder::new(raw)))
    } else if name.ends_with(".bz2") {
        Ok(Box::new(bzip2::read::BzDecoder::new(raw)))
    } else if name.ends_with(".zst") {
        Ok(Box::new(ruzstd::decoding::StreamingDecoder::new(raw).map_err(|e| anyhow!("bad zstd in {}: {e}", path.display()))?))
    } else if name.ends_with(".xz") {
        xz_reader(raw)
    } else {
        Ok(Box::new(raw))
    }
}

/// xz: the `xz` program when there is one (fast, streaming), else the pure-Rust decoder (slower, in memory).
fn xz_reader(mut raw: impl Read + Send + 'static) -> Result<Box<dyn Read>> {
    if let Ok(mut child) = Command::new("xz").arg("-dc").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() {
        let mut stdin = child.stdin.take().expect("piped");
        let out = child.stdout.take().expect("piped");
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut raw, &mut stdin);
        });
        std::mem::forget(child);
        return Ok(Box::new(out));
    }
    let mut data = Vec::new();
    raw.read_to_end(&mut data)?;
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut std::io::Cursor::new(data), &mut out).map_err(|e| anyhow!("bad xz data: {e:?}"))?;
    Ok(Box::new(std::io::Cursor::new(out)))
}

/// The package's files as a tar stream.
pub fn data_reader(deb: &Path) -> Result<Box<dyn Read>> {
    open_member(deb, "data.tar")
}

