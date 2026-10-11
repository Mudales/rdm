//! What a stream offers, and the resolved segment lists to download.

use std::ops::Range;

use rdm_core::Url;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Hls,
    Dash,
}

/// A stream's renditions, for the user to choose from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub kind: StreamKind,
    pub url: String,
    /// Video (or audio-only) renditions, best first.
    pub variants: Vec<Variant>,
    /// Separate audio renditions.
    pub audio: Vec<AudioTrack>,
    /// Total duration in seconds, when known.
    pub duration: Option<f64>,
    pub live: bool,
    /// Set when the stream uses DRM or encryption rdm does not handle.
    pub protection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub id: String,
    pub bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    pub frame_rate: Option<f64>,
    /// HLS: the `AUDIO` group whose tracks go with this variant.
    pub audio_group: Option<String>,
    /// Human description, e.g. "1080p · 5.2 Mbps · avc1".
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrack {
    pub id: String,
    pub group: Option<String>,
    pub name: Option<String>,
    pub language: Option<String>,
    pub codecs: Option<String>,
    pub bandwidth: Option<u64>,
    pub default: bool,
    pub label: String,
}

/// Container of a track's segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MPEG-TS segments: concatenation is a valid .ts file.
    Ts,
    /// Fragmented MP4 (init + fragments): concatenation is a valid .mp4.
    Mp4,
    /// WebM (init + clusters).
    WebM,
    /// Raw elementary audio (e.g. .aac segments).
    Audio(&'static str),
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Ts => "ts",
            Container::Mp4 => "mp4",
            Container::WebM => "webm",
            Container::Audio(ext) => ext,
        }
    }
}

/// AES-128 key reference for a segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRef {
    pub uri: Url,
    pub iv: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRef {
    pub url: Url,
    pub range: Option<Range<u64>>,
    pub key: Option<KeyRef>,
}

/// Everything needed to download one track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub init: Option<SegmentRef>,
    pub segments: Vec<SegmentRef>,
    pub container: Container,
    /// A single resource holding the whole track (DASH `SegmentBase`):
    /// downloaded with the regular segmented engine instead.
    pub whole: Option<Url>,
}

impl Track {
    pub fn units(&self) -> usize {
        if self.whole.is_some() {
            1
        } else {
            self.segments.len() + usize::from(self.init.is_some())
        }
    }
}

pub(crate) fn variant_label(
    height: Option<u32>,
    bandwidth: Option<u64>,
    codecs: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(h) = height {
        parts.push(format!("{h}p"));
    }
    if let Some(b) = bandwidth {
        parts.push(if b >= 1_000_000 {
            format!("{:.1} Mbps", b as f64 / 1e6)
        } else {
            format!("{} kbps", b / 1000)
        });
    }
    if let Some(c) = codecs {
        let short: Vec<&str> = c
            .split(',')
            .map(|c| c.trim().split('.').next().unwrap_or_default())
            .filter(|c| !c.is_empty())
            .collect();
        if !short.is_empty() {
            parts.push(short.join("+"));
        }
    }
    if parts.is_empty() {
        "Default".into()
    } else {
        parts.join(" · ")
    }
}

pub(crate) fn audio_label(
    name: Option<&str>,
    language: Option<&str>,
    bandwidth: Option<u64>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = name {
        parts.push(n.to_owned());
    }
    if let Some(l) = language.filter(|l| Some(*l) != name) {
        parts.push(l.to_owned());
    }
    if let Some(b) = bandwidth {
        parts.push(format!("{} kbps", b / 1000));
    }
    if parts.is_empty() {
        "Audio".into()
    } else {
        parts.join(" · ")
    }
}

/// Container implied by a segment URL's extension.
pub(crate) fn container_for(url: &Url, fallback: Container) -> Container {
    let path = url.path().to_ascii_lowercase();
    let ext = path.rsplit('.').next().unwrap_or_default();
    match ext {
        "ts" | "m2ts" | "mts" => Container::Ts,
        "mp4" | "m4s" | "m4v" | "m4a" | "cmfv" | "cmfa" | "fmp4" => Container::Mp4,
        "webm" => Container::WebM,
        "aac" => Container::Audio("aac"),
        "mp3" => Container::Audio("mp3"),
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(
            variant_label(Some(1080), Some(5_200_000), Some("avc1.640028,mp4a.40.2")),
            "1080p · 5.2 Mbps · avc1+mp4a"
        );
        assert_eq!(variant_label(None, Some(96_000), None), "96 kbps");
        assert_eq!(variant_label(None, None, None), "Default");
        assert_eq!(
            audio_label(Some("English"), Some("en"), Some(128_000)),
            "English · en · 128 kbps"
        );
    }
}
