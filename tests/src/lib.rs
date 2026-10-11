//! Shared test support for rdm integration tests.
//!
//! Phase 2 adds an axum mock server here that can serve or refuse byte
//! ranges, lie about `Content-Length`, drop connections mid-transfer, inject
//! latency and return 429/503. Integration tests live in `tests/tests/`.
