//! Content hashing helpers used by `build.finished.artifacts[]` (contract §2)
//! and by artifact-change detection (`artifact.changed`).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{PrincessError, Result};

/// SHA-256 of a byte slice, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

/// SHA-256 of a file, streamed in 64 KiB blocks (a kernel image may be large;
/// the engine never reads a binary fully into memory to hash it).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|err| {
        PrincessError::not_found(format!("cannot open {} for hashing: {err}", path.display()))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            PrincessError::internal(format!("cannot read {} for hashing: {err}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Size and SHA-256 of a file in one pass over its metadata + contents.
pub fn file_size_and_hash(path: &Path) -> Result<(u64, String)> {
    let size = std::fs::metadata(path)
        .map_err(|err| PrincessError::not_found(format!("cannot stat {}: {err}", path.display())))?
        .len();
    Ok((size, sha256_file(path)?))
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_test_vectors() {
        // FIPS 180-4 / RFC 6234 vectors, so a hashing regression cannot pass.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }

    #[test]
    fn file_hashing_matches_in_memory_hashing() {
        let path = std::env::temp_dir().join(format!("princesside-hash-{}", std::process::id()));
        let payload = vec![b'P'; 200 * 1024]; // crosses the 64 KiB block boundary
        std::fs::write(&path, &payload).unwrap();
        let (size, hash) = file_size_and_hash(&path).unwrap();
        assert_eq!(size, payload.len() as u64);
        assert_eq!(hash, sha256_hex(&payload));
        std::fs::remove_file(&path).unwrap();

        let err = sha256_file(Path::new("/definitely/not/a/file")).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::NotFound);
    }
}
