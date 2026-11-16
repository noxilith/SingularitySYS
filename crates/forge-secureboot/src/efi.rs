//! What firmware stores for Secure Boot: signature lists, and authenticated variables that carry them.

use std::fs;
use std::path::Path;

use crate::cms;
use crate::keys::Identity;
use crate::Error;

/// A GUID in the mixed-endian byte order UEFI uses.
pub fn guid(s: &str) -> [u8; 16] {
    let h: Vec<u8> = s.replace('-', "").as_bytes().chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect();
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&[h[3], h[2], h[1], h[0]]);
    g[4..6].copy_from_slice(&[h[5], h[4]]);
    g[6..8].copy_from_slice(&[h[7], h[6]]);
    g[8..16].copy_from_slice(&h[8..16]);
    g
}

pub const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
pub const DB_GUID: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";
const X509_GUID: &str = "a5c059a1-94e4-4aa7-87b5-ab155c2bf072";
const PKCS7_GUID: &str = "4aafd29d-68df-49ee-8aa9-347d375665a7";
/// Who owns the entries forge enrolls ("SingularitySYS").
const OWNER_GUID: &str = "5a1d0c6e-7c02-4a56-9d3b-8a1f2f6f0c11";

/// non-volatile, boot-service access, run-time access, time-based authenticated write
pub const ATTRIBUTES: u32 = 0x27;

/// An `EFI_SIGNATURE_LIST` holding one X.509 certificate.
pub fn esl(cert: &[u8]) -> Vec<u8> {
    let sig_size = 16 + cert.len();
    let mut v = Vec::new();
    v.extend(guid(X509_GUID));
    v.extend(((28 + sig_size) as u32).to_le_bytes());
    v.extend(0u32.to_le_bytes());
    v.extend((sig_size as u32).to_le_bytes());
    v.extend(guid(OWNER_GUID));
    v.extend(cert);
    v
}

/// (year, month, day) of a Unix time.
pub fn civil_date(unix: u64) -> (u16, u8, u8) {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as u16;
    (y, m, d)
}

/// The `EFI_TIME` of an authenticated write: the date only (the rest must be zero), never before 2020.
fn efi_time(unix: u64) -> [u8; 16] {
    let (y, m, d) = civil_date(unix.max(1_577_836_800));
    let mut t = [0u8; 16];
    t[0..2].copy_from_slice(&y.to_le_bytes());
    t[2] = m;
    t[3] = d;
    t
}

/// The payload of a time-based authenticated write of `data` to variable `name` (GUID `vendor`), signed by `id`:
/// the `EFI_VARIABLE_AUTHENTICATION_2` header, then the data. (A `.auth` file is exactly this.)
pub fn authenticated(id: &Identity, name: &str, vendor: &str, data: &[u8], unix: u64) -> Result<Vec<u8>, Error> {
    let time = efi_time(unix);
    let mut signed = Vec::new();
    for c in name.encode_utf16() {
        signed.extend(c.to_le_bytes());
    }
    signed.extend(guid(vendor));
    signed.extend(ATTRIBUTES.to_le_bytes());
    signed.extend(time);
    signed.extend(data);
    let p7 = cms::signed_data(id, cms::ID_DATA, None, &signed)?;
    let mut out = Vec::new();
    out.extend(time);
    out.extend(((8 + 16 + p7.len()) as u32).to_le_bytes());
    out.extend(0x0200u16.to_le_bytes());
    out.extend(0x0ef1u16.to_le_bytes());
    out.extend(guid(PKCS7_GUID));
    out.extend(&p7);
    out.extend(data);
    Ok(out)
}

fn var_path(efivars: &Path, name: &str, vendor: &str) -> std::path::PathBuf {
    efivars.join(format!("{name}-{vendor}"))
}

/// A variable's value (without its four attribute bytes).
pub fn read_var(efivars: &Path, name: &str, vendor: &str) -> Option<Vec<u8>> {
    fs::read(var_path(efivars, name, vendor)).ok().filter(|b| b.len() >= 4).map(|b| b[4..].to_vec())
}

/// Firmware accepts new keys without a signature from an older one only in setup mode.
pub fn setup_mode(efivars: &Path) -> Option<bool> {
    read_var(efivars, "SetupMode", GLOBAL_GUID).and_then(|v| v.first().map(|b| *b == 1))
}

pub fn secure_boot_on(efivars: &Path) -> Option<bool> {
    read_var(efivars, "SecureBoot", GLOBAL_GUID).and_then(|v| v.first().map(|b| *b == 1))
}

/// Write an authenticated variable through efivarfs: four attribute bytes, then the payload, in one write.
pub fn write_var(efivars: &Path, name: &str, vendor: &str, payload: &[u8]) -> Result<(), Error> {
    let path = var_path(efivars, name, vendor);
    let mut data = ATTRIBUTES.to_le_bytes().to_vec();
    data.extend(payload);
    // An existing variable is immutable until told otherwise.
    if path.exists() {
        let _ = std::process::Command::new("chattr").arg("-i").arg(&path).status();
    }
    fs::write(&path, &data).map_err(|e| Error::io(&path, e))
}

/// Put the identity into the firmware as database key, key-exchange key and platform key (in that order: the last one
/// ends setup mode). Only valid in setup mode.
pub fn enroll(id: &Identity, efivars: &Path, unix: u64) -> Result<(), Error> {
    match setup_mode(efivars) {
        Some(true) => {}
        Some(false) => return Err(Error::NotSetupMode),
        None if !efivars.is_dir() => return Err(Error::NoEfi),
        None => return Err(Error::NoSecureBootVars),
    }
    let list = esl(&id.cert);
    write_var(efivars, "db", DB_GUID, &authenticated(id, "db", DB_GUID, &list, unix)?)?;
    write_var(efivars, "KEK", GLOBAL_GUID, &authenticated(id, "KEK", GLOBAL_GUID, &list, unix)?)?;
    write_var(efivars, "PK", GLOBAL_GUID, &authenticated(id, "PK", GLOBAL_GUID, &list, unix)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_use_the_mixed_endian_layout() {
        assert_eq!(guid(GLOBAL_GUID), [0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c]);
    }

    #[test]
    fn a_signature_list_has_the_sizes_firmware_checks() {
        let l = esl(&[7u8; 300]);
        let list_size = u32::from_le_bytes(l[16..20].try_into().unwrap()) as usize;
        let sig_size = u32::from_le_bytes(l[24..28].try_into().unwrap()) as usize;
        assert_eq!((list_size, sig_size, l.len()), (28 + 316, 316, 28 + 316));
        assert_eq!(&l[28..44], guid(OWNER_GUID));
    }

    #[test]
    fn dates_come_out_right() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(1_577_836_800), (2020, 1, 1));
        assert_eq!(civil_date(1_794_700_800), (2026, 11, 15));
        assert_eq!(civil_date(1_709_164_800), (2024, 2, 29));
    }

    #[test]
    fn an_authenticated_payload_is_a_header_a_signature_and_the_data() {
        let id = crate::cms_test_identity();
        let data = esl(&id.cert);
        let p = authenticated(id, "PK", GLOBAL_GUID, &data, 0).unwrap();
        assert_eq!(&p[0..2], 2020u16.to_le_bytes(), "never before 2020, so a clock that is wrong cannot matter");
        assert_eq!(&p[16 + 4..16 + 6], [0x00, 0x02]);
        assert_eq!(&p[16 + 6..16 + 8], [0xf1, 0x0e]);
        assert!(p.ends_with(&data));
        let cert_len = u32::from_le_bytes(p[16..20].try_into().unwrap()) as usize;
        assert_eq!(16 + cert_len + data.len(), p.len(), "the header's length covers exactly the signature");
    }

    #[test]
    fn enrolling_needs_setup_mode() {
        let d = std::env::temp_dir().join(format!("forge-efivars-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let id = crate::cms_test_identity();
        assert!(matches!(enroll(id, &d, 0), Err(Error::NoSecureBootVars)), "efivars exist but no SetupMode");
        assert!(matches!(enroll(id, &d.join("missing"), 0), Err(Error::NoEfi)));
        std::fs::write(d.join(format!("SetupMode-{GLOBAL_GUID}")), [6, 0, 0, 0, 0]).unwrap();
        assert!(matches!(enroll(id, &d, 0), Err(Error::NotSetupMode)));
        std::fs::write(d.join(format!("SetupMode-{GLOBAL_GUID}")), [6, 0, 0, 0, 1]).unwrap();
        enroll(id, &d, 0).unwrap();
        for (n, g) in [("db", DB_GUID), ("KEK", GLOBAL_GUID), ("PK", GLOBAL_GUID)] {
            let v = std::fs::read(d.join(format!("{n}-{g}"))).unwrap();
            assert_eq!(&v[0..4], ATTRIBUTES.to_le_bytes());
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
