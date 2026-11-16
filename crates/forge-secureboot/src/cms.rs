//! PKCS#7 / CMS `SignedData` with SHA-256 and RSA, the signature format of both Authenticode (signed boot files) and
//! UEFI authenticated variables (signed key databases).

use sha2::{Digest, Sha256};

use crate::der;
use crate::keys::Identity;
use crate::Error;

const CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
const MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
pub const ID_DATA: &str = "1.2.840.113549.1.7.1";

/// A `ContentInfo` holding `SignedData`. `econtent` is the signed content as a complete DER value (None for a detached
/// signature); `digest_input` is the bytes the message digest covers: the content's octets, or for a detached
/// signature the data that was signed.
pub fn signed_data(id: &Identity, content_type: &str, econtent: Option<&[u8]>, digest_input: &[u8]) -> Result<Vec<u8>, Error> {
    let digest = Sha256::digest(digest_input);
    let ct_attr = der::seq(&[&der::oid(CONTENT_TYPE), &der::set(&[&der::oid(content_type)])]);
    let md_attr = der::seq(&[&der::oid(MESSAGE_DIGEST), &der::set(&[&der::octets(&digest)])]);
    // The signature covers the attributes as a SET; in the SignerInfo they carry the tag [0] instead.
    let attrs = der::set(&[&ct_attr, &md_attr]);
    let signature = id.sign(&attrs);
    let attrs_in_signer = der::tlv(0xa0, der::content_of(&attrs));
    let (issuer, serial) = id.issuer_and_serial()?;
    let signer = der::seq(&[
        &der::int_small(1),
        &der::seq(&[&issuer, &serial]),
        &der::alg(der::SHA256),
        &attrs_in_signer,
        &der::alg(der::RSA_ENCRYPTION),
        &der::octets(&signature),
    ]);
    let encap = match econtent {
        Some(c) => der::seq(&[&der::oid(content_type), &der::tlv(0xa0, c)]),
        None => der::seq(&[&der::oid(content_type)]),
    };
    let signed = der::seq(&[&der::int_small(1), &der::set(&[&der::alg(der::SHA256)]), &encap, &der::tlv(0xa0, &id.cert), &der::set(&[&signer])]);
    Ok(der::seq(&[&der::oid(SIGNED_DATA), &der::tlv(0xa0, &signed)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;

    pub fn test_identity() -> &'static Identity {
        static ID: std::sync::OnceLock<Identity> = std::sync::OnceLock::new();
        ID.get_or_init(|| Identity::generate("forge test").unwrap())
    }

    #[test]
    fn the_signature_covers_the_attributes_and_the_attributes_cover_the_data() {
        let id = test_identity();
        let p7 = signed_data(id, ID_DATA, None, b"hello").unwrap();
        // Walk to the SignerInfo: ContentInfo > [0] > SignedData > ... > SET > SignerInfo.
        let (_, ci, _) = der::next(&p7).unwrap();
        let (_, _oid, rest) = der::next(ci).unwrap();
        let (_, wrapped, _) = der::next(rest).unwrap();
        let (_, sd, _) = der::next(wrapped).unwrap();
        let mut r = sd;
        let mut last = &[][..];
        while let Some((tag, content, rest)) = der::next(r) {
            last = if tag == 0x31 { content } else { last };
            r = rest;
        }
        let (_, signer, _) = der::next(last).unwrap();
        let mut parts = Vec::new();
        let mut r = signer;
        while let Some((t, c, rest)) = der::next(r) {
            parts.push((t, c.to_vec()));
            r = rest;
        }
        let attrs = der::tlv(0x31, &parts[3].1);
        let sig_value = &parts[5].1[..];
        let verifier = VerifyingKey::<Sha256>::new(id.key.to_public_key());
        verifier.verify(&attrs, &Signature::try_from(sig_value).unwrap()).expect("the attributes are signed by the key");
        assert!(attrs.windows(32).any(|w| w == Sha256::digest(b"hello").as_slice()), "the attributes carry the data's digest");
    }
}
