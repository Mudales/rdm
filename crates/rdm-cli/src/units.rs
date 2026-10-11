//! Parsing and formatting of sizes, rates and durations.

use std::num::NonZeroU64;
use std::time::Duration;

/// Parses `512`, `64K`, `1.5M`, `2G`, `10MiB`, `10MB` (all binary: K = 1024).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().map_err(|_| format!("invalid size `{s}`"))?;
    let unit = unit.trim().to_ascii_lowercase();
    let unit = unit.strip_suffix("/s").unwrap_or(&unit);
    let mult: u64 = match unit {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return Err(format!("unknown unit in `{s}` (use K, M or G)")),
    };
    if !num.is_finite() || num < 0.0 {
        return Err(format!("invalid size `{s}`"));
    }
    Ok((num * mult as f64) as u64)
}

pub fn parse_rate(s: &str) -> Result<NonZeroU64, String> {
    NonZeroU64::new(parse_size(s)?).ok_or_else(|| "speed limit must be above zero".into())
}

/// `1.50 MiB`
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.2} {}", UNITS.get(unit).copied().unwrap_or("B"))
    }
}

/// `1:02:03`, `2:05`, `45s`
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, (s % 3600) / 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("512"), Ok(512));
        assert_eq!(parse_size("64K"), Ok(65536));
        assert_eq!(parse_size("1.5M"), Ok(1_572_864));
        assert_eq!(parse_size("2MiB"), Ok(2 << 20));
        assert_eq!(parse_size("1g"), Ok(1 << 30));
        assert_eq!(parse_size("500K/s"), Ok(512_000));
        assert!(parse_size("3x").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_rate("0").is_err());
    }

    #[test]
    fn formatting() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.50 KiB");
        assert_eq!(bytes(5 << 30), "5.00 GiB");
        assert_eq!(duration(Duration::from_secs(45)), "45s");
        assert_eq!(duration(Duration::from_secs(125)), "2:05");
        assert_eq!(duration(Duration::from_secs(3723)), "1:02:03");
    }
}
