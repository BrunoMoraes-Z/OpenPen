//! `.sha256` sidecar files (`sha256sum` format) and SHA-256 verification.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Parses a `sha256sum` line: `<64 hex>  <file name>` (the name, optionally
/// prefixed by `*` for binary mode, may be absent). Returns the lowercase
/// digest. When the line names a file, it must be `expected_name`.
pub fn parse_sha256_file(text: &str, expected_name: &str) -> Result<String, String> {
    let line = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or("the checksum file is empty")?;
    let mut parts = line.split_whitespace();
    let hex = parts.next().unwrap_or_default();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("not a SHA-256 checksum: {line:?}"));
    }
    if let Some(name) = parts.next() {
        let name = name.trim_start_matches('*');
        if !name.eq_ignore_ascii_case(expected_name) {
            return Err(format!(
                "the checksum is for {name}, not for {expected_name}"
            ));
        }
    }
    Ok(hex.to_ascii_lowercase())
}

/// Lowercase hex of a digest.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// SHA-256 of a byte slice, as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

/// SHA-256 of a file, as lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(to_hex(&h.finalize()))
}

/// Constant-shape comparison of two hex digests (case-insensitive).
pub fn digests_match(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| {
            acc | (x.to_ascii_lowercase() ^ y.to_ascii_lowercase())
        }) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn known_digest() {
        assert_eq!(sha256_hex(b"abc"), ABC);
    }

    #[test]
    fn parses_the_contract_format() {
        let text = format!("{ABC}  OpenPen-0.2.0-x64.msi\n");
        assert_eq!(
            parse_sha256_file(&text, "OpenPen-0.2.0-x64.msi").unwrap(),
            ABC
        );
        // Binary-mode marker, CRLF, uppercase, BOM, hash only.
        let text = format!("\u{feff}{} *OpenPen-0.2.0-x64.msi\r\n", ABC.to_uppercase());
        assert_eq!(
            parse_sha256_file(&text, "OpenPen-0.2.0-x64.msi").unwrap(),
            ABC
        );
        assert_eq!(parse_sha256_file(ABC, "x.msi").unwrap(), ABC);
    }

    #[test]
    fn rejects_bad_checksum_files() {
        assert!(parse_sha256_file("", "a.msi").is_err());
        assert!(parse_sha256_file("abc  a.msi", "a.msi").is_err());
        let wrong = format!("{ABC}  OpenPen-0.1.0-x64.msi");
        assert!(parse_sha256_file(&wrong, "OpenPen-0.2.0-x64.msi").is_err());
        let not_hex = format!("{}g  a.msi", &ABC[..63]);
        assert!(parse_sha256_file(&not_hex, "a.msi").is_err());
    }

    #[test]
    fn file_digest_and_compare() {
        let dir = std::env::temp_dir().join("pen-update-sha-test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("abc.bin");
        std::fs::write(&f, b"abc").unwrap();
        let d = sha256_file(&f).unwrap();
        assert!(digests_match(&d, ABC));
        assert!(digests_match(&d, &ABC.to_uppercase()));
        assert!(!digests_match(&d, &sha256_hex(b"abd")));
        assert!(!digests_match(&d, &ABC[..10]));
    }
}
