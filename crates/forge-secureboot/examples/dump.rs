//! Writes a certificate, a signed boot file and a variable payload for checking with other tools (OpenSSL).
use forge_secureboot::{efi, pe, Identity};
fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output directory"));
    let loader = std::fs::read(std::env::args().nth(2).expect("an EFI file")).expect("read the EFI file");
    std::fs::create_dir_all(&out).unwrap();
    let id = Identity::generate("forge dump").unwrap();
    std::fs::write(out.join("cert.der"), &id.cert).unwrap();
    let signed = pe::sign(&loader, &id).unwrap();
    std::fs::write(out.join("signed.efi"), &signed).unwrap();
    let auth = efi::authenticated(&id, "db", efi::DB_GUID, &efi::esl(&id.cert), 1_794_700_800).unwrap();
    std::fs::write(out.join("db.auth"), &auth).unwrap();
    println!("hash {:02x?}", pe::authenticode_hash(&signed).unwrap());
}
