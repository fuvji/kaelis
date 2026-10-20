//! Hash helpers: SHA1 (Mojang / Forge / Modrinth truth) and
//! XXH3 (fast local dedupe + repair scans). SIMD-friendly streaming.
use sha1::{Digest, Sha1};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HashError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Hex SHA1 of a file on disk (streams, 64 KiB chunks).
pub fn sha1_file(path: &std::path::Path) -> Result<String, HashError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(hasher.finalize().as_slice()))
}

/// Hex SHA1 of in-memory bytes.
pub fn sha1_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hex_encode(hasher.finalize().as_slice())
}

/// XXH3-64 of in-memory bytes (local cache keys, repair scans).
pub fn xxh3_bytes(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes)
}

/// XXH3-64 of a file on disk (streams).
pub fn xxh3_file(path: &std::path::Path) -> Result<u64, HashError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest())
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}
