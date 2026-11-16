//! The signing identity: one RSA key and a self-signed certificate. The same certificate is the platform key, the
//! key-exchange key and the signature database entry of the machine, so one pair of files is all there is to keep safe.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::rand_core::{OsRng, RngCore};
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;

use crate::der;
use crate::Error;

pub const KEY_FILE: &str = "sb.key";
pub const CERT_FILE: &str = "sb.der";

pub struct Identity {
    pub key: RsaPrivateKey,
    /// The certificate, DER.
    pub cert: Vec<u8>,
}

fn name(cn: &str) -> Vec<u8> {
    der::seq(&[&der::set(&[&der::seq(&[&der::oid("2.5.4.3"), &der::utf8_string(cn)])])])
}

impl Identity {
    /// A fresh 2048-bit key and a certificate for it, valid from 2020 to 2120 (firmware does not check dates, and a
    /// machine whose clock is wrong at first boot must not matter).
    pub fn generate(common_name: &str) -> Result<Self, Error> {
        let key = RsaPrivateKey::new(&mut OsRng, 2048).map_err(|e| Error::Crypto(e.to_string()))?;
        Self::for_key(key, common_name)
    }

    pub fn for_key(key: RsaPrivateKey, common_name: &str) -> Result<Self, Error> {
        let spki = key.to_public_key().to_public_key_der().map_err(|e| Error::Crypto(e.to_string()))?;
        let mut serial = [0u8; 16];
        OsRng.fill_bytes(&mut serial);
        serial[0] &= 0x7f;
        let ext_basic = der::seq(&[&der::oid("2.5.29.19"), &[0x01, 0x01, 0xff], &der::octets(&der::seq(&[&[0x01, 0x01, 0xff]]))]);
        let ext_usage = der::seq(&[&der::oid("2.5.29.15"), &[0x01, 0x01, 0xff], &der::octets(&[0x03, 0x02, 0x01, 0x86])]);
        let n = name(common_name);
        let tbs = der::seq(&[
            &der::tlv(0xa0, &der::int_small(2)),
            &der::int_unsigned(&serial),
            &der::alg(der::SHA256_WITH_RSA),
            &n,
            &der::seq(&[&der::tlv(0x17, b"200101000000Z"), &der::tlv(0x18, b"21200101000000Z")]),
            &n,
            spki.as_bytes(),
            &der::tlv(0xa3, &der::seq(&[&ext_basic, &ext_usage])),
        ]);
        let sig = SigningKey::<Sha256>::new(key.clone()).sign(&tbs).to_bytes();
        let cert = der::seq(&[&tbs, &der::alg(der::SHA256_WITH_RSA), &der::bit_string(&sig)]);
        Ok(Identity { key, cert })
    }

    /// The issuer name and the serial number of the certificate, as DER (what a signature names its signer by).
    pub fn issuer_and_serial(&self) -> Result<(Vec<u8>, Vec<u8>), Error> {
        issuer_and_serial(&self.cert)
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        SigningKey::<Sha256>::new(self.key.clone()).sign(message).to_bytes().to_vec()
    }

    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let pem = self.key.to_pkcs8_pem(LineEnding::LF).map_err(|e| Error::Crypto(e.to_string()))?;
        let key_path = dir.join(KEY_FILE);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&key_path).map_err(|e| Error::io(&key_path, e))?;
        f.write_all(pem.as_bytes()).map_err(|e| Error::io(&key_path, e))?;
        let cert_path = dir.join(CERT_FILE);
        fs::write(&cert_path, &self.cert).map_err(|e| Error::io(&cert_path, e))
    }

    pub fn load(dir: &Path) -> Result<Self, Error> {
        let key_path = dir.join(KEY_FILE);
        let pem = fs::read_to_string(&key_path).map_err(|e| Error::io(&key_path, e))?;
        let key = RsaPrivateKey::from_pkcs8_pem(&pem).map_err(|e| Error::Crypto(format!("{}: {e}", key_path.display())))?;
        let cert_path = dir.join(CERT_FILE);
        let cert = fs::read(&cert_path).map_err(|e| Error::io(&cert_path, e))?;
        Ok(Identity { key, cert })
    }
}

pub fn issuer_and_serial(cert: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let bad = || Error::Format("not a certificate".into());
    let (_, body, _) = der::next(cert).ok_or_else(bad)?;
    let (_, tbs, _) = der::next(body).ok_or_else(bad)?;
    let mut rest = tbs;
    // [0] version (optional), serial, signature algorithm, issuer
    let (tag, _, after) = der::next(rest).ok_or_else(bad)?;
    if tag == 0xa0 {
        rest = after;
    }
    let (serial_tag, serial_content, after) = der::next(rest).ok_or_else(bad)?;
    let serial = der::tlv(serial_tag, serial_content);
    let (_, _, after) = der::next(after).ok_or_else(bad)?;
    let (issuer_tag, issuer_content, _) = der::next(after).ok_or_else(bad)?;
    Ok((der::tlv(issuer_tag, issuer_content), serial))
}
