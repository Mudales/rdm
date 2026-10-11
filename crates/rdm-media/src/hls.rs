//! HLS (RFC 8216) master and media playlist parsing.

use rdm_core::Url;

use crate::error::{Error, Result};
use crate::model::{
    AudioTrack, Container, KeyRef, SegmentRef, Track, Variant, audio_label, container_for,
    variant_label,
};

/// A parsed playlist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Playlist {
    Master(Master),
    Media(Box<Media>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Master {
    /// Variants with their media playlist URLs, in playlist order.
    pub variants: Vec<(Variant, Url)>,
    /// Audio renditions; `None` URL means the audio is muxed in the variant.
    pub audio: Vec<(AudioTrack, Option<Url>)>,
    pub protection: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Media {
    pub track: Track,
    pub ended: bool,
    pub duration: f64,
    pub protection: Option<String>,
}

/// Parses a playlist; relative URIs resolve against `base`.
pub(crate) fn parse(text: &str, base: &Url) -> Result<Playlist> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err(Error::parse("HLS playlist", "missing #EXTM3U header"));
    }
    if text.contains("#EXT-X-STREAM-INF") {
        parse_master(text, base).map(Playlist::Master)
    } else {
        parse_media(text, base).map(|m| Playlist::Media(Box::new(m)))
    }
}

/// Splits an attribute list (`A=1,B="x,y",C=0x10`) into unquoted pairs.
pub(crate) fn attributes(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = list.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_ascii_uppercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"').unwrap_or(stripped.len());
            value = stripped[..end].to_owned();
            rest = stripped.get(end + 1..).unwrap_or_default();
            rest = rest
                .trim_start_matches(|c: char| c != ',')
                .trim_start_matches(',');
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_owned();
            rest = rest.get(end + 1..).unwrap_or_default();
        }
        out.push((key, value));
        rest = rest.trim_start();
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn resolve(base: &Url, uri: &str) -> Result<Url> {
    base.join(uri)
        .map_err(|e| Error::parse("HLS playlist", format!("bad URI `{uri}`: {e}")))
}

/// Methods/key formats rdm will not decrypt (DRM).
fn protection_of(attrs: &[(String, String)]) -> Option<String> {
    let method = attr(attrs, "METHOD").unwrap_or("NONE");
    let format = attr(attrs, "KEYFORMAT").unwrap_or("identity");
    match method {
        "NONE" => None,
        "AES-128" if format.eq_ignore_ascii_case("identity") => None,
        "AES-128" => Some(format!("key format {format}")),
        other => Some(format!("{other} encryption")),
    }
}

fn parse_master(text: &str, base: &Url) -> Result<Master> {
    let mut variants = Vec::new();
    let mut audio = Vec::new();
    let mut protection = None;
    let mut pending: Option<Vec<(String, String)>> = None;

    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(attributes(rest));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attributes(rest);
            if attr(&a, "TYPE") != Some("AUDIO") {
                continue;
            }
            let name = attr(&a, "NAME").map(str::to_owned);
            let language = attr(&a, "LANGUAGE").map(str::to_owned);
            let url = attr(&a, "URI").map(|u| resolve(base, u)).transpose()?;
            let track = AudioTrack {
                id: format!("a{}", audio.len()),
                group: attr(&a, "GROUP-ID").map(str::to_owned),
                label: audio_label(name.as_deref(), language.as_deref(), None),
                name,
                language,
                codecs: None,
                bandwidth: None,
                default: attr(&a, "DEFAULT") == Some("YES"),
            };
            audio.push((track, url));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            protection = protection.or(protection_of(&attributes(rest)));
        } else if line.starts_with('#') {
            continue;
        } else if let Some(a) = pending.take() {
            let bandwidth = attr(&a, "BANDWIDTH").and_then(|v| v.parse().ok());
            let (width, height) = attr(&a, "RESOLUTION")
                .and_then(|r| r.split_once(['x', 'X']))
                .map_or((None, None), |(w, h)| (w.parse().ok(), h.parse().ok()));
            let codecs = attr(&a, "CODECS").map(str::to_owned);
            let variant = Variant {
                id: format!("v{}", variants.len()),
                bandwidth,
                width,
                height,
                label: variant_label(height, bandwidth, codecs.as_deref()),
                codecs,
                frame_rate: attr(&a, "FRAME-RATE").and_then(|v| v.parse().ok()),
                audio_group: attr(&a, "AUDIO").map(str::to_owned),
            };
            variants.push((variant, resolve(base, line)?));
        }
    }
    if variants.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Master {
        variants,
        audio,
        protection,
    })
}

fn parse_iv(hex: &str) -> Result<[u8; 16]> {
    let digits = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    if digits.is_empty() || digits.len() > 32 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::parse("HLS playlist", format!("bad IV `{hex}`")));
    }
    let value = u128::from_str_radix(digits, 16)
        .map_err(|e| Error::parse("HLS playlist", format!("bad IV `{hex}`: {e}")))?;
    Ok(value.to_be_bytes())
}

/// `n[@o]`: length and optional offset.
fn parse_byterange(s: &str) -> Result<(u64, Option<u64>)> {
    let bad = || Error::parse("HLS playlist", format!("bad byte range `{s}`"));
    let (len, off) = match s.split_once('@') {
        Some((l, o)) => (l, Some(o.trim().parse().map_err(|_| bad())?)),
        None => (s, None),
    };
    Ok((len.trim().parse().map_err(|_| bad())?, off))
}

fn parse_media(text: &str, base: &Url) -> Result<Media> {
    let mut sequence: u64 = 0;
    let mut key: Option<(Url, Option<[u8; 16]>)> = None;
    let mut protection = None;
    let mut pending_range: Option<(u64, Option<u64>)> = None;
    // End of the previous byte range, per URL, for ranges without offset.
    let mut last_end: Option<(Url, u64)> = None;
    let mut init = None;
    let mut segments = Vec::new();
    let mut ended = false;
    let mut duration = 0.0;

    let key_for = |key: &Option<(Url, Option<[u8; 16]>)>, seq: u64| {
        key.as_ref().map(|(uri, iv)| KeyRef {
            uri: uri.clone(),
            iv: iv.unwrap_or_else(|| u128::from(seq).to_be_bytes()),
        })
    };

    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = v.trim().parse().unwrap_or(0);
        } else if line == "#EXT-X-ENDLIST" || line == "#EXT-X-PLAYLIST-TYPE:VOD" {
            ended = true;
        } else if let Some(v) = line.strip_prefix("#EXTINF:") {
            duration += v
                .split(',')
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
        } else if let Some(v) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(parse_byterange(v)?);
        } else if let Some(v) = line.strip_prefix("#EXT-X-KEY:") {
            let a = attributes(v);
            if let Some(p) = protection_of(&a) {
                protection.get_or_insert(p);
                key = None;
            } else if attr(&a, "METHOD") == Some("AES-128") {
                let uri = attr(&a, "URI")
                    .ok_or_else(|| Error::parse("HLS playlist", "AES-128 key without URI"))?;
                key = Some((
                    resolve(base, uri)?,
                    attr(&a, "IV").map(parse_iv).transpose()?,
                ));
            } else {
                key = None;
            }
        } else if let Some(v) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attributes(v);
            let uri = attr(&a, "URI")
                .ok_or_else(|| Error::parse("HLS playlist", "EXT-X-MAP without URI"))?;
            let range = attr(&a, "BYTERANGE")
                .map(parse_byterange)
                .transpose()?
                .map(|(len, off)| off.unwrap_or(0)..off.unwrap_or(0) + len);
            init = Some(SegmentRef {
                url: resolve(base, uri)?,
                range,
                key: key_for(&key, sequence),
            });
        } else if line.starts_with('#') {
            continue;
        } else {
            let url = resolve(base, line)?;
            let range = pending_range.take().map(|(len, off)| {
                let start = off.unwrap_or_else(|| match &last_end {
                    Some((u, end)) if *u == url => *end,
                    _ => 0,
                });
                start..start + len
            });
            if let Some(r) = &range {
                last_end = Some((url.clone(), r.end));
            }
            segments.push(SegmentRef {
                url,
                range,
                key: key_for(&key, sequence),
            });
            sequence += 1;
        }
    }
    if segments.is_empty() {
        return Err(Error::Empty);
    }
    let fallback = if init.is_some() {
        Container::Mp4
    } else {
        Container::Ts
    };
    let container = segments
        .first()
        .map_or(fallback, |s| container_for(&s.url, fallback));
    Ok(Media {
        track: Track {
            init,
            segments,
            container,
            whole: None,
        },
        ended,
        duration,
        protection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/show/master.m3u8").unwrap()
    }

    #[test]
    fn attribute_lists() {
        let a = attributes(
            r#"BANDWIDTH=1280000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1280x720, NAME="a,b""#,
        );
        assert_eq!(attr(&a, "BANDWIDTH"), Some("1280000"));
        assert_eq!(attr(&a, "CODECS"), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(attr(&a, "RESOLUTION"), Some("1280x720"));
        assert_eq!(attr(&a, "NAME"), Some("a,b"));
    }

    #[test]
    fn master_playlist() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio/en.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Deutsch\",LANGUAGE=\"de\",URI=\"audio/de.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",URI=\"subs/en.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e\",AUDIO=\"aud\"\n\
            360p/index.m3u8\n\
            #EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=90000,URI=\"iframes.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,FRAME-RATE=29.97,AUDIO=\"aud\"\n\
            https://other.example.com/1080p.m3u8\n";
        let Playlist::Master(m) = parse(text, &base()).unwrap() else {
            panic!()
        };
        assert_eq!(m.variants.len(), 2);
        let (v, url) = &m.variants[1];
        assert_eq!(
            (v.id.as_str(), v.height, v.frame_rate),
            ("v1", Some(1080), Some(29.97))
        );
        assert_eq!(url.as_str(), "https://other.example.com/1080p.m3u8");
        assert_eq!(
            m.variants[0].1.as_str(),
            "https://cdn.example.com/show/360p/index.m3u8"
        );
        assert_eq!(m.audio.len(), 2);
        assert!(m.audio[0].0.default);
        assert_eq!(
            m.audio[1].1.as_ref().unwrap().as_str(),
            "https://cdn.example.com/show/audio/de.m3u8"
        );
        assert_eq!(m.protection, None);
    }

    #[test]
    fn media_playlist_with_keys_and_ranges() {
        let text = "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:7\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"keys/k1\"\n\
            #EXTINF:6.0,\nseg7.ts\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"keys/k2\",IV=0x0000000000000000000000000000ABCD\n\
            #EXTINF:6.0,\nseg8.ts\n\
            #EXT-X-KEY:METHOD=NONE\n\
            #EXT-X-BYTERANGE:1000@0\n#EXTINF:4.5,\nall.ts\n\
            #EXT-X-BYTERANGE:500\n#EXTINF:4.5,\nall.ts\n\
            #EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(text, &base()).unwrap() else {
            panic!()
        };
        assert!(m.ended);
        assert_eq!(m.duration, 21.0);
        let s = &m.track.segments;
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].key.as_ref().unwrap().iv, 7u128.to_be_bytes());
        assert_eq!(
            s[0].key.as_ref().unwrap().uri.as_str(),
            "https://cdn.example.com/show/keys/k1"
        );
        assert_eq!(s[1].key.as_ref().unwrap().iv, 0xABCDu128.to_be_bytes());
        assert_eq!(s[2].key, None);
        assert_eq!(s[2].range, Some(0..1000));
        assert_eq!(s[3].range, Some(1000..1500));
        assert_eq!(m.track.container, Container::Ts);
    }

    #[test]
    fn fmp4_init_segment() {
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"\n#EXTINF:2,\nfrag1.m4s\n#EXTINF:2,\nfrag2.m4s\n";
        let Playlist::Media(m) = parse(text, &base()).unwrap() else {
            panic!()
        };
        assert!(!m.ended, "no ENDLIST: live");
        let init = m.track.init.unwrap();
        assert_eq!(init.url.as_str(), "https://cdn.example.com/show/init.mp4");
        assert_eq!(init.range, Some(0..720));
        assert_eq!(m.track.container, Container::Mp4);
    }

    #[test]
    fn drm_is_reported() {
        for key in [
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://abc\",KEYFORMAT=\"com.apple.streamingkeydelivery\"",
            "#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"data:text/plain;base64,AAAA\"",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"k\",KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"",
        ] {
            let text = format!("#EXTM3U\n{key}\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n");
            let Playlist::Media(m) = parse(&text, &base()).unwrap() else {
                panic!()
            };
            assert!(m.protection.is_some(), "{key}");
        }
        let master = "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n";
        let Playlist::Master(m) = parse(master, &base()).unwrap() else {
            panic!()
        };
        assert!(m.protection.is_some());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("<html>", &base()).is_err());
        assert!(parse("#EXTM3U\n#EXT-X-ENDLIST\n", &base()).is_err());
        assert!(
            parse(
                "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0xZZ\n#EXTINF:1,\na.ts\n",
                &base()
            )
            .is_err()
        );
    }
}
