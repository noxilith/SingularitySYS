//! Authenticode signatures on PE files (UEFI executables): what Secure Boot firmware checks before it runs one.

use sha2::{Digest, Sha256};

use crate::cms;
use crate::der;
use crate::keys::Identity;
use crate::Error;

fn bad(why: &str) -> Error {
    Error::Format(format!("not a usable PE (UEFI) file: {why}"))
}

fn u16_at(b: &[u8], o: usize) -> Result<usize, Error> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize).ok_or_else(|| bad("truncated"))
}

fn u32_at(b: &[u8], o: usize) -> Result<usize, Error> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize).ok_or_else(|| bad("truncated"))
}

struct Layout {
    checksum: usize,
    cert_dir: usize,
    size_of_headers: usize,
    /// (raw data pointer, raw data size) of each section that has data, in file order.
    sections: Vec<(usize, usize)>,
}

fn layout(pe: &[u8]) -> Result<Layout, Error> {
    if pe.get(0..2) != Some(b"MZ") {
        return Err(bad("no MZ header"));
    }
    let lfanew = u32_at(pe, 0x3c)?;
    if pe.get(lfanew..lfanew + 4) != Some(b"PE\0\0") {
        return Err(bad("no PE signature"));
    }
    let coff = lfanew + 4;
    let sections = u16_at(pe, coff + 2)?;
    let opt_size = u16_at(pe, coff + 16)?;
    let opt = coff + 20;
    if u16_at(pe, opt)? != 0x20b {
        return Err(bad("not a 64-bit (PE32+) image"));
    }
    let n_dirs = u32_at(pe, opt + 108)?;
    if n_dirs <= 4 {
        return Err(bad("no certificate table entry"));
    }
    let mut secs = Vec::new();
    for i in 0..sections {
        let s = opt + opt_size + 40 * i;
        let (size, ptr) = (u32_at(pe, s + 16)?, u32_at(pe, s + 20)?);
        if size > 0 {
            secs.push((ptr, size));
        }
    }
    secs.sort_by_key(|(ptr, _)| *ptr);
    Ok(Layout { checksum: opt + 64, cert_dir: opt + 112 + 4 * 8, size_of_headers: u32_at(pe, opt + 60)?, sections: secs })
}

/// The Authenticode digest: the file without its checksum field, its certificate-table entry and the certificate table.
pub fn authenticode_hash(pe: &[u8]) -> Result<[u8; 32], Error> {
    let l = layout(pe)?;
    let (cert_off, cert_size) = (u32_at(pe, l.cert_dir)?, u32_at(pe, l.cert_dir + 4)?);
    let part = |from: usize, to: usize| pe.get(from..to).ok_or_else(|| bad("a section lies outside the file"));
    let mut h = Sha256::new();
    h.update(part(0, l.checksum)?);
    h.update(part(l.checksum + 4, l.cert_dir)?);
    h.update(part(l.cert_dir + 8, l.size_of_headers)?);
    let mut hashed = l.size_of_headers;
    for (ptr, size) in &l.sections {
        h.update(part(*ptr, ptr + size)?);
        hashed += size;
    }
    let end = if cert_size > 0 { cert_off } else { pe.len() };
    if end > hashed {
        h.update(part(hashed, end)?);
    }
    Ok(h.finalize().into())
}

/// `SpcIndirectDataContent`: the image's digest, in the structure Authenticode signs.
fn spc_content(hash: &[u8; 32]) -> Vec<u8> {
    let obsolete: Vec<u8> = "<<<Obsolete>>>".encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
    let file = der::tlv(0xa0, &der::tlv(0xa2, &der::tlv(0x80, &obsolete)));
    let image_data = der::seq(&[&der::tlv(0x03, &[0]), &file]);
    let data = der::seq(&[&der::oid("1.3.6.1.4.1.311.2.1.15"), &image_data]);
    let digest_info = der::seq(&[&der::alg(der::SHA256), &der::octets(hash)]);
    der::seq(&[&data, &digest_info])
}

/// `pe` signed with `id`: any earlier signature is replaced.
pub fn sign(pe: &[u8], id: &Identity) -> Result<Vec<u8>, Error> {
    let l = layout(pe)?;
    let (cert_off, cert_size) = (u32_at(pe, l.cert_dir)?, u32_at(pe, l.cert_dir + 4)?);
    let mut out = if cert_size > 0 { pe[..cert_off].to_vec() } else { pe.to_vec() };
    out[l.cert_dir..l.cert_dir + 8].fill(0);
    let hash = authenticode_hash(&out)?;
    let content = spc_content(&hash);
    let p7 = cms::signed_data(id, "1.3.6.1.4.1.311.2.1.4", Some(&content), der::content_of(&content))?;
    while out.len() % 8 != 0 {
        out.push(0);
    }
    let offset = out.len();
    let total = (8 + p7.len()).div_ceil(8) * 8;
    out.extend((total as u32).to_le_bytes());
    out.extend(0x0200u16.to_le_bytes());
    out.extend(0x0002u16.to_le_bytes());
    out.extend(&p7);
    out.resize(offset + total, 0);
    out[l.cert_dir..l.cert_dir + 4].copy_from_slice(&(offset as u32).to_le_bytes());
    out[l.cert_dir + 4..l.cert_dir + 8].copy_from_slice(&(total as u32).to_le_bytes());
    Ok(out)
}

/// Whether `pe` carries a signature made with the certificate `cert`, over its current contents.
pub fn signed_by(pe: &[u8], cert: &[u8]) -> bool {
    let Ok(l) = layout(pe) else { return false };
    let (Ok(off), Ok(size)) = (u32_at(pe, l.cert_dir), u32_at(pe, l.cert_dir + 4)) else { return false };
    let Some(table) = pe.get(off..off + size).filter(|t| t.len() > 8) else { return false };
    let Ok(hash) = authenticode_hash(pe) else { return false };
    let find = |needle: &[u8]| table.windows(needle.len()).any(|w| w == needle);
    find(cert) && find(&hash)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A minimal PE32+ file: headers, one section of 16 bytes, a few bytes of trailing data.
    pub fn tiny_pe() -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        let coff = 0x44;
        b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        b[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
        let opt = coff + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
        b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        let sec = opt + 240;
        b[sec + 16..sec + 20].copy_from_slice(&16u32.to_le_bytes());
        b[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
        b.extend_from_slice(&[0xAB; 16]);
        b.extend_from_slice(b"trailing");
        b
    }

    #[test]
    fn signing_adds_a_certificate_table_and_changes_nothing_the_digest_covers() {
        let id = crate::cms_test_identity();
        let pe = tiny_pe();
        let before = authenticode_hash(&pe).unwrap();
        let signed = sign(&pe, id).unwrap();
        assert!(signed.len() > pe.len() && signed.len() % 8 == 0);
        assert_eq!(authenticode_hash(&signed).unwrap(), before, "the digest ignores the certificate table");
        assert!(signed_by(&signed, &id.cert) && !signed_by(&pe, &id.cert));
        // Signing again replaces the signature instead of stacking another one.
        let again = sign(&signed, id).unwrap();
        assert_eq!(again.len(), signed.len());
        assert!(signed_by(&again, &id.cert));
        // Changing the program breaks the match.
        let mut tampered = signed.clone();
        tampered[0x205] ^= 1;
        assert!(!signed_by(&tampered, &id.cert));
    }

    #[test]
    fn a_file_that_is_not_a_64_bit_pe_is_refused() {
        assert!(sign(b"MZ nonsense", crate::cms_test_identity()).is_err());
        let mut pe = tiny_pe();
        pe[0x44 + 20..0x44 + 22].copy_from_slice(&0x10bu16.to_le_bytes());
        assert!(authenticode_hash(&pe).is_err());
    }
}
