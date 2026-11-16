//! The little DER (ASN.1) this crate needs, written out by hand so every byte is known.

pub fn len(n: usize) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else if n < 0x100 {
        vec![0x81, n as u8]
    } else if n < 0x10000 {
        vec![0x82, (n >> 8) as u8, n as u8]
    } else {
        vec![0x83, (n >> 16) as u8, (n >> 8) as u8, n as u8]
    }
}

/// A tag, the length of `content`, and `content`.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend(len(content.len()));
    v.extend_from_slice(content);
    v
}

pub fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

pub fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &cat(parts))
}

pub fn set(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x31, &cat(parts))
}

pub fn octets(b: &[u8]) -> Vec<u8> {
    tlv(0x04, b)
}

pub fn null() -> Vec<u8> {
    vec![0x05, 0x00]
}

pub fn int_small(n: u8) -> Vec<u8> {
    tlv(0x02, &[n])
}

/// An unsigned big-endian integer (a serial number, a modulus).
pub fn int_unsigned(be: &[u8]) -> Vec<u8> {
    let mut b: Vec<u8> = be.iter().copied().skip_while(|x| *x == 0).collect();
    if b.is_empty() {
        b.push(0);
    }
    if b[0] & 0x80 != 0 {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

pub fn bit_string(b: &[u8]) -> Vec<u8> {
    let mut c = vec![0u8];
    c.extend_from_slice(b);
    tlv(0x03, &c)
}

/// An object identifier from its dotted form.
pub fn oid(dotted: &str) -> Vec<u8> {
    let n: Vec<u64> = dotted.split('.').map(|p| p.parse().expect("OID arc")).collect();
    let mut c = vec![(n[0] * 40 + n[1]) as u8];
    for &arc in &n[2..] {
        let mut chunk = vec![(arc & 0x7f) as u8];
        let mut rest = arc >> 7;
        while rest > 0 {
            chunk.push(((rest & 0x7f) as u8) | 0x80);
            rest >>= 7;
        }
        chunk.reverse();
        c.extend(chunk);
    }
    tlv(0x06, &c)
}

pub fn utf8_string(s: &str) -> Vec<u8> {
    tlv(0x0c, s.as_bytes())
}

/// The content octets of a TLV (its header taken off): what a digest over "the content" covers.
pub fn content_of(tlv: &[u8]) -> &[u8] {
    let l = tlv[1];
    let header = if l < 0x80 { 2 } else { 2 + (l & 0x7f) as usize };
    &tlv[header..]
}

pub const SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub const RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
pub const SHA256_WITH_RSA: &str = "1.2.840.113549.1.1.11";

pub fn alg(oid_dotted: &str) -> Vec<u8> {
    seq(&[&oid(oid_dotted), &null()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_oids_and_integers_encode_like_openssl_does() {
        assert_eq!(len(5), [5]);
        assert_eq!(len(200), [0x81, 200]);
        assert_eq!(len(300), [0x82, 1, 44]);
        assert_eq!(oid("1.2.840.113549.1.1.11"), [0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]);
        assert_eq!(oid("1.3.6.1.4.1.311.2.1.4"), [0x06, 0x0a, 0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x01, 0x04]);
        assert_eq!(int_unsigned(&[0, 0, 0x80]), [0x02, 0x02, 0x00, 0x80], "a high bit gets a leading zero");
        assert_eq!(int_unsigned(&[0, 0]), [0x02, 0x01, 0x00]);
        assert_eq!(content_of(&seq(&[&null()])), [0x05, 0x00]);
    }
}

/// Split one TLV off the front of `b`: (tag, content, the rest).
pub fn next(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let l = *b.get(1)?;
    let (n, header) = if l < 0x80 {
        (l as usize, 2)
    } else {
        let k = (l & 0x7f) as usize;
        let mut n = 0usize;
        for i in 0..k {
            n = (n << 8) | *b.get(2 + i)? as usize;
        }
        (n, 2 + k)
    };
    let end = header.checked_add(n)?;
    Some((tag, b.get(header..end)?, b.get(end..)?))
}
