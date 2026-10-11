//! Expected checksums: parsing and file hashing.

use std::io::Read;
use std::path::Path;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Md5 => "md5",
            Algorithm::Sha1 => "sha-1",
            Algorithm::Sha256 => "sha-256",
            Algorithm::Sha512 => "sha-512",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace(['-', '_'], "")
                .as_str()
            {
                "md5" => Algorithm::Md5,
                "sha1" | "sha" => Algorithm::Sha1,
                "sha256" => Algorithm::Sha256,
                "sha512" => Algorithm::Sha512,
                _ => return None,
            },
        )
    }

    fn hex_len(self) -> usize {
        match self {
            Algorithm::Md5 => 32,
            Algorithm::Sha1 => 40,
            Algorithm::Sha256 => 64,
            Algorithm::Sha512 => 128,
        }
    }

    fn from_len(len: usize) -> Option<Self> {
        [
            Algorithm::Md5,
            Algorithm::Sha1,
            Algorithm::Sha256,
            Algorithm::Sha512,
        ]
        .into_iter()
        .find(|a| a.hex_len() == len)
    }
}

/// An expected checksum: `sha-256:abcd…`, `md5=…`, or bare hex (the
/// algorithm follows from the length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    pub algorithm: Algorithm,
    pub hex: String,
}

impl Checksum {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (alg, hex) = match s.split_once([':', '=']) {
            Some((a, h)) => (Some(Algorithm::parse(a)?), h.trim()),
            None => (None, s),
        };
        let hex = hex.to_ascii_lowercase();
        if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let algorithm = match alg {
            Some(a) if a.hex_len() == hex.len() => a,
            Some(_) => return None,
            None => Algorithm::from_len(hex.len())?,
        };
        Some(Self { algorithm, hex })
    }
}

impl std::fmt::Display for Checksum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex)
    }
}

/// Hashes a file (blocking).
pub fn hash_file(path: &Path, algorithm: Algorithm) -> std::io::Result<String> {
    fn run<D: Digest>(path: &Path) -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = D::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
    match algorithm {
        Algorithm::Md5 => run::<Md5>(path),
        Algorithm::Sha1 => run::<Sha1>(path),
        Algorithm::Sha256 => run::<Sha256>(path),
        Algorithm::Sha512 => run::<Sha512>(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let c = Checksum::parse(
            "SHA256:B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9",
        )
        .unwrap();
        assert_eq!(c.algorithm, Algorithm::Sha256);
        assert_eq!(
            c.to_string(),
            "sha-256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(
            Checksum::parse("5eb63bbbe01eeed093cb22bb8f5acdc3")
                .unwrap()
                .algorithm,
            Algorithm::Md5
        );
        assert_eq!(
            Checksum::parse("sha-1=2aae6c35c94fcfb415dbe95f408b9ce91ee846ed")
                .unwrap()
                .algorithm,
            Algorithm::Sha1
        );
        assert_eq!(Checksum::parse("md5:abcd"), None, "length must match");
        assert_eq!(Checksum::parse("crc32:00000000"), None);
        assert_eq!(Checksum::parse("not hex at all"), None);
        assert_eq!(Checksum::parse("abc"), None);
    }

    #[test]
    fn hashing() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("hello.txt");
        std::fs::write(&f, b"hello world").unwrap();
        assert_eq!(
            hash_file(&f, Algorithm::Md5).unwrap(),
            "5eb63bbbe01eeed093cb22bb8f5acdc3"
        );
        assert_eq!(
            hash_file(&f, Algorithm::Sha1).unwrap(),
            "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"
        );
        assert_eq!(
            hash_file(&f, Algorithm::Sha256).unwrap(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(hash_file(&f, Algorithm::Sha512).unwrap().len(), 128);
    }
}
