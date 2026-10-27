//! Misbehaviours the mock server can be told to exhibit.

use std::time::Duration;

/// Fault injection and access requirements. Everything is off by default.
#[derive(Debug, Clone, Default)]
pub struct Faults {
    /// Delay before the response headers of every request.
    pub latency: Option<Duration>,

    /// Answer some requests with an error status instead of the file.
    pub error: Option<ErrorFault>,

    /// Abort the connection after sending this many body bytes.
    pub drop_after: Option<u64>,
    /// Stop sending for the given time after this many body bytes.
    pub stall_after: Option<(u64, Duration)>,
    /// Apply `drop_after` / `stall_after` to only this many responses
    /// (`None`: all of them).
    pub body_fault_limit: Option<u64>,

    /// Claim this `Content-Length` on full (200) responses while sending the
    /// real body.
    pub content_length_override: Option<u64>,
    /// Answer range requests starting this many bytes *before* the requested
    /// offset (with a matching, i.e. honest but wrong, `Content-Range`).
    pub range_shift: u64,
    /// Send this many bytes *past* the end of the requested range while
    /// `Content-Range` still claims the range that was asked for. This is how
    /// a store that applies `Range` to a compressed copy and then decompresses
    /// it behaves: the body is not the slice its headers describe.
    pub range_overrun: u64,
    /// Honor `Range` only for requests whose index (0-based, in arrival
    /// order) is below this. Later requests get the full 200 response.
    pub ranges_until_request: Option<u64>,

    /// `Set-Cookie` value sent with every redirect from `/hop/N/...`.
    pub redirect_cookie: Option<String>,
    /// Required `name=value` pair in the `Cookie` header (else 403).
    pub require_cookie: Option<String>,
    /// Required request headers, compared case-insensitively by name and
    /// exactly by value (else 403).
    pub require_headers: Vec<(String, String)>,
    /// Required HTTP Basic credentials (else 401).
    pub require_basic_auth: Option<(String, String)>,
    /// Required `Proxy-Authorization` Basic credentials on proxied
    /// (absolute-form) requests (else 407).
    pub require_proxy_auth: Option<(String, String)>,
}

/// Respond with `status` to requests number `skip .. skip + count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorFault {
    pub status: u16,
    pub skip: u64,
    pub count: u64,
    /// `Retry-After` in seconds.
    pub retry_after: Option<u64>,
}

impl ErrorFault {
    /// The first `count` requests fail with `status`.
    pub fn first(status: u16, count: u64) -> Self {
        Self {
            status,
            skip: 0,
            count,
            retry_after: None,
        }
    }

    pub fn always(status: u16) -> Self {
        Self::first(status, u64::MAX)
    }

    pub fn skip(mut self, skip: u64) -> Self {
        self.skip = skip;
        self
    }

    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub(crate) fn applies_to(&self, request_index: u64) -> bool {
        request_index >= self.skip && request_index - self.skip < self.count
    }
}

/// `Basic base64(user:pass)`.
pub(crate) fn basic_auth_value(user: &str, pass: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()))
}

/// Standard base64 with padding.
pub fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }

    #[test]
    fn error_window() {
        let f = ErrorFault::first(503, 2).skip(1);
        assert!(!f.applies_to(0));
        assert!(f.applies_to(1) && f.applies_to(2));
        assert!(!f.applies_to(3));
        assert!(ErrorFault::always(500).applies_to(u64::MAX - 1));
    }
}
