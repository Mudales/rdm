//! `rdm-media` — streaming-media downloads for rdm.
//!
//! * Parses HLS master/media playlists and DASH MPDs and lists their
//!   renditions ([`MediaEngine::inspect`]).
//! * Downloads the chosen rendition's segments concurrently through
//!   `rdm-core`'s network stack (same cookies, speed limits and per-host
//!   caps), keeping them in a parts folder so a stopped download resumes.
//! * Decrypts standard AES-128 HLS. DRM-protected streams (SAMPLE-AES,
//!   FairPlay, Widevine, PlayReady, CENC) are refused before anything is
//!   downloaded: rdm reports "protected content" and stops.
//! * Joins single-track streams directly (MPEG-TS, fragmented MP4) and uses
//!   ffmpeg only to merge separate audio/video, convert TS to MP4, or
//!   extract audio.

mod crypto;
mod dash;
mod error;
mod ffmpeg;
mod hls;
mod job;
mod model;
mod process;
mod tools;
mod ytdlp;

pub use error::{Error, Result};
pub use ffmpeg::{
    FFMPEG_ENV, Ffmpeg, FfmpegConfig, FfmpegSource, ffmpeg_source, install_ffmpeg, install_hint,
};
pub use job::{
    MediaControl, MediaEngine, MediaEvent, MediaHandle, MediaJob, MediaOutcome, MediaProgress,
    OutputFormat, Phase,
};
pub use model::{AudioTrack, Manifest, StreamKind, Variant};
pub use process::{background_command, background_std_command};
pub use tools::{ToolSource, deno_source, install_tool, latest_release, ytdlp_source};
pub use ytdlp::{DENO_ENV, PlaylistEntry, SiteChoice, SiteInfo, YTDLP_ENV, YtDlp};

/// Whether a URL or content type looks like a streaming playlist.
pub fn looks_like_stream(url: &str, content_type: Option<&str>) -> bool {
    let ct = content_type.unwrap_or_default().to_ascii_lowercase();
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    ct.contains("mpegurl")
        || ct.contains("dash+xml")
        || path.ends_with(".m3u8")
        || path.ends_with(".mpd")
}
