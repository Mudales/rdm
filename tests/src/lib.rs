//! Test support for rdm: an axum mock HTTP server with deterministic content
//! and fault injection.
//!
//! The served bytes are a pure function of `(seed, offset)`, so files of any
//! size can be served and verified without keeping them in memory, and a
//! byte written at the wrong offset is detected.
//!
//! The server can honor or refuse byte ranges, cap each connection's speed,
//! and inject faults ([`Faults`]): dropped or stalled connections, latency,
//! error statuses with `Retry-After`, wrong lengths, misaligned ranges,
//! redirects, and cookie / header / Basic-auth / proxy-auth requirements.

mod faults;
mod server;

use std::io::{self, Read};
use std::path::Path;

pub use faults::{ErrorFault, Faults};
pub use server::{MockConfig, MockServer, StaticFile};

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The byte served at `offset` for `seed`.
pub fn content_byte(seed: u64, offset: u64) -> u8 {
    let word = splitmix64((offset >> 3) ^ seed.rotate_left(32));
    (word >> ((offset & 7) * 8)) as u8
}

/// Fills `buf` with the content starting at `offset`.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = content_byte(seed, offset + i as u64);
    }
}

/// The full expected content (for small sizes).
pub fn expected_content(seed: u64, size: u64) -> Vec<u8> {
    let mut v = vec![0u8; size as usize];
    fill(seed, 0, &mut v);
    v
}

/// Streams `path` and compares it with the expected content.
/// `Ok(Err(msg))` describes the first difference.
pub fn verify_file(path: &Path, seed: u64, size: u64) -> io::Result<Result<(), String>> {
    let mut f = std::fs::File::open(path)?;
    let actual = f.metadata()?.len();
    if actual != size {
        return Ok(Err(format!("size is {actual}, expected {size}")));
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut want = vec![0u8; 1 << 20];
    let mut offset = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(Ok(()));
        }
        let (got, exp) = (&buf[..n], &mut want[..n]);
        fill(seed, offset, exp);
        if let Some(i) = got.iter().zip(exp.iter()).position(|(a, b)| a != b) {
            return Ok(Err(format!("first mismatch at byte {}", offset + i as u64)));
        }
        offset += n as u64;
    }
}

/// Parses sizes like `512`, `64K`, `1.5M`, `1G`, `10MiB` (binary units).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().ok()?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return None,
    };
    (num >= 0.0).then_some((num * mult as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_deterministic_and_seeded() {
        let a = expected_content(0, 64);
        assert_eq!(a, expected_content(0, 64));
        assert_ne!(a, expected_content(1, 64));
        let mut mid = vec![0; 10];
        fill(0, 30, &mut mid);
        assert_eq!(&a[30..40], &mid[..]);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("64K"), Some(65536));
        assert_eq!(parse_size("1.5M"), Some(1572864));
        assert_eq!(parse_size("1GiB"), Some(1 << 30));
        assert_eq!(parse_size("3x"), None);
    }
}
