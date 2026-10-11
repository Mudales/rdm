//! `rdm-core` — the rdm download engine.
//!
//! This crate is a standalone library: it has no GUI, browser-extension or
//! ffmpeg dependencies, and every fallible operation returns a `Result`.
//! The engine itself (probing, dynamic segmentation, resume, throttling) is
//! implemented in Phase 1.

/// Version of the engine crate, for diagnostics and the `User-Agent` header.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!VERSION.is_empty());
    }
}
