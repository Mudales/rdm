//! `rdm-media` — streaming-media support for rdm.
//!
//! Parses HLS and DASH manifests, downloads their segments through
//! `rdm-core`, and muxes the result with an ffmpeg sidecar process.
//! Implemented in Phase 5. DRM-protected streams are detected and refused,
//! never decrypted.
