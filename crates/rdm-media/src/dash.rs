//! MPEG-DASH MPD parsing and segment resolution.
//!
//! Supports static (on-demand) single-period MPDs with `SegmentTemplate`
//! (`$Number$` or `$Time$`, with or without `SegmentTimeline`),
//! `SegmentList`, and `SegmentBase` / plain `BaseURL` tracks. Templates and
//! base URLs inherit from Period → AdaptationSet → Representation.

use std::collections::HashMap;

use rdm_core::Url;
use roxmltree::{Document, Node};

use crate::error::{Error, Result};
use crate::model::{
    AudioTrack, Container, Manifest, SegmentRef, StreamKind, Track, Variant, audio_label,
    variant_label,
};

/// A parsed MPD: what to offer, and every representation's track.
#[derive(Debug, Clone)]
pub(crate) struct Mpd {
    pub manifest: Manifest,
    pub tracks: HashMap<String, Track>,
}

fn err(reason: impl Into<String>) -> Error {
    Error::parse("DASH manifest", reason)
}

fn kids<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn kid<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> Option<Node<'a, 'i>> {
    kids(node, name).next()
}

/// `PT1H2M3.5S`, `P1DT2H` → seconds. Years and months are approximated.
pub(crate) fn iso_duration(s: &str) -> Option<f64> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut total = 0.0;
    let mut num = String::new();
    for (part, is_time) in [(date, false), (time, true)] {
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
                continue;
            }
            let v: f64 = num.parse().ok()?;
            num.clear();
            total += v * match (c, is_time) {
                ('Y', false) => 365.0 * 86400.0,
                ('M', false) => 30.0 * 86400.0,
                ('W', false) => 7.0 * 86400.0,
                ('D', false) => 86400.0,
                ('H', true) => 3600.0,
                ('M', true) => 60.0,
                ('S', true) => 1.0,
                _ => return None,
            };
        }
        if !num.is_empty() {
            return None;
        }
    }
    Some(total)
}

/// Expands `$RepresentationID$`, `$Number%05d$`, `$Time$`, `$Bandwidth$`
/// and `$$` in a segment template.
pub(crate) fn expand_template(
    t: &str,
    id: &str,
    number: u64,
    time: u64,
    bandwidth: u64,
) -> Result<String> {
    let mut out = String::with_capacity(t.len() + 16);
    let mut rest = t;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('$')
            .ok_or_else(|| err(format!("unterminated `$` in template `{t}`")))?;
        let token = &after[..end];
        rest = &after[end + 1..];
        if token.is_empty() {
            out.push('$');
            continue;
        }
        let (name, width) = match token.split_once('%') {
            Some((n, fmt)) => {
                let w = fmt
                    .strip_prefix('0')
                    .and_then(|f| f.strip_suffix('d'))
                    .and_then(|w| w.parse::<usize>().ok())
                    .ok_or_else(|| err(format!("unsupported format `{token}`")))?;
                (n, w)
            }
            None => (token, 0),
        };
        let value = match name {
            "RepresentationID" => {
                out.push_str(id);
                continue;
            }
            "Number" => number,
            "Time" => time,
            "Bandwidth" => bandwidth,
            other => return Err(err(format!("unknown template identifier `${other}$`"))),
        };
        out.push_str(&format!("{value:0width$}"));
    }
    out.push_str(rest);
    Ok(out)
}

fn base_of(node: Node<'_, '_>, parent: &Url) -> Result<Url> {
    match kid(node, "BaseURL").and_then(|b| b.text()) {
        Some(text) => parent
            .join(text.trim())
            .map_err(|e| err(format!("bad BaseURL `{text}`: {e}"))),
        None => Ok(parent.clone()),
    }
}

/// SegmentTemplate attributes merged across levels (most specific wins).
#[derive(Debug, Default, Clone)]
struct Template<'a, 'i> {
    attrs: HashMap<&'a str, &'a str>,
    timeline: Option<Node<'a, 'i>>,
    present: bool,
}

impl<'a, 'i> Template<'a, 'i> {
    fn overlay(&mut self, node: Option<Node<'a, 'i>>) {
        let Some(n) = node.and_then(|n| kid(n, "SegmentTemplate")) else {
            return;
        };
        self.present = true;
        for a in n.attributes() {
            self.attrs.insert(a.name(), a.value());
        }
        if let Some(tl) = kid(n, "SegmentTimeline") {
            self.timeline = Some(tl);
        }
    }

    fn num(&self, key: &str) -> Result<Option<u64>> {
        self.attrs
            .get(key)
            .map(|v| {
                v.trim()
                    .parse()
                    .map_err(|_| err(format!("bad {key} `{v}`")))
            })
            .transpose()
    }
}

fn parse_range(s: &str) -> Result<std::ops::Range<u64>> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| err(format!("bad byte range `{s}`")))?;
    let a: u64 = a
        .trim()
        .parse()
        .map_err(|_| err(format!("bad byte range `{s}`")))?;
    let b: u64 = b
        .trim()
        .parse()
        .map_err(|_| err(format!("bad byte range `{s}`")))?;
    if b < a {
        return Err(err(format!("bad byte range `{s}`")));
    }
    Ok(a..b + 1)
}

struct RepCtx<'a> {
    id: &'a str,
    bandwidth: u64,
    base: Url,
    period_secs: Option<f64>,
    container: Container,
}

fn template_track(t: &Template<'_, '_>, ctx: &RepCtx<'_>) -> Result<Track> {
    let media = t
        .attrs
        .get("media")
        .ok_or_else(|| err("SegmentTemplate without media"))?;
    let timescale = t.num("timescale")?.unwrap_or(1).max(1);
    let start_number = t.num("startNumber")?.unwrap_or(1);
    let url = |template: &str, number: u64, time: u64| -> Result<Url> {
        let path = expand_template(template, ctx.id, number, time, ctx.bandwidth)?;
        ctx.base
            .join(&path)
            .map_err(|e| err(format!("bad segment URL `{path}`: {e}")))
    };
    let init = t
        .attrs
        .get("initialization")
        .map(|i| -> Result<SegmentRef> {
            Ok(SegmentRef {
                url: url(i, start_number, 0)?,
                range: None,
                key: None,
            })
        })
        .transpose()?;

    let period_ticks = ctx
        .period_secs
        .map(|s| (s * timescale as f64).round() as u64);
    let mut segments = Vec::new();
    if let Some(timeline) = t.timeline {
        let mut time = 0u64;
        let mut number = start_number;
        let entries: Vec<_> = kids(timeline, "S").collect();
        for (i, s) in entries.iter().enumerate() {
            let get = |k: &str| s.attribute(k).map(str::trim);
            if let Some(v) = get("t") {
                time = v.parse().map_err(|_| err(format!("bad S@t `{v}`")))?;
            }
            let d: u64 = get("d")
                .ok_or_else(|| err("S without d"))?
                .parse()
                .map_err(|_| err("bad S@d"))?;
            if d == 0 {
                return Err(err("S@d must be positive"));
            }
            let r: i64 = get("r").map_or(Ok(0), |v| {
                v.parse().map_err(|_| err(format!("bad S@r `{v}`")))
            })?;
            let count = if r >= 0 {
                r as u64 + 1
            } else {
                // Repeat until the next S@t or the end of the period.
                let until = entries
                    .get(i + 1)
                    .and_then(|n| n.attribute("t"))
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .or(period_ticks)
                    .ok_or_else(|| err("S@r=-1 needs a known period duration"))?;
                until.saturating_sub(time).div_ceil(d)
            };
            for _ in 0..count {
                segments.push(SegmentRef {
                    url: url(media, number, time)?,
                    range: None,
                    key: None,
                });
                time += d;
                number += 1;
            }
        }
    } else {
        let duration = t
            .num("duration")?
            .filter(|d| *d > 0)
            .ok_or_else(|| err("SegmentTemplate needs a SegmentTimeline or a duration"))?;
        let ticks = period_ticks
            .ok_or_else(|| err("cannot count segments: the period duration is unknown"))?;
        let count = ticks.div_ceil(duration);
        for k in 0..count {
            segments.push(SegmentRef {
                url: url(media, start_number + k, k * duration)?,
                range: None,
                key: None,
            });
        }
    }
    Ok(Track {
        init,
        segments,
        container: ctx.container,
        whole: None,
    })
}

fn list_track(list: Node<'_, '_>, ctx: &RepCtx<'_>) -> Result<Track> {
    let resolve = |media: Option<&str>| -> Result<Url> {
        match media {
            Some(m) => ctx
                .base
                .join(m)
                .map_err(|e| err(format!("bad SegmentURL `{m}`: {e}"))),
            None => Ok(ctx.base.clone()),
        }
    };
    let init = kid(list, "Initialization")
        .map(|i| -> Result<SegmentRef> {
            Ok(SegmentRef {
                url: resolve(i.attribute("sourceURL"))?,
                range: i.attribute("range").map(parse_range).transpose()?,
                key: None,
            })
        })
        .transpose()?;
    let segments = kids(list, "SegmentURL")
        .map(|s| {
            Ok(SegmentRef {
                url: resolve(s.attribute("media"))?,
                range: s.attribute("mediaRange").map(parse_range).transpose()?,
                key: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if segments.is_empty() {
        return Err(err("SegmentList without SegmentURL"));
    }
    Ok(Track {
        init,
        segments,
        container: ctx.container,
        whole: None,
    })
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Kind {
    Video,
    Audio,
    Other,
}

fn kind_of(set: Node<'_, '_>, rep: Node<'_, '_>) -> Kind {
    let mime = rep
        .attribute("mimeType")
        .or_else(|| set.attribute("mimeType"))
        .unwrap_or_default();
    let content = set.attribute("contentType").unwrap_or_default();
    if content == "video" || mime.starts_with("video/") {
        Kind::Video
    } else if content == "audio" || mime.starts_with("audio/") {
        Kind::Audio
    } else {
        Kind::Other
    }
}

fn frame_rate(s: &str) -> Option<f64> {
    match s.split_once('/') {
        Some((a, b)) => Some(a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?),
        None => s.trim().parse().ok(),
    }
}

fn protection_in(node: Node<'_, '_>) -> Option<String> {
    kid(node, "ContentProtection").map(|cp| {
        cp.attribute("value")
            .or_else(|| cp.attribute("schemeIdUri"))
            .unwrap_or("ContentProtection")
            .to_owned()
    })
}

/// Parses an MPD fetched from `url`.
pub(crate) fn parse(xml: &str, url: &Url) -> Result<Mpd> {
    let doc = Document::parse(xml).map_err(|e| err(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "MPD" {
        return Err(err("root element is not MPD"));
    }
    let live = root.attribute("type") == Some("dynamic");
    let total = root
        .attribute("mediaPresentationDuration")
        .and_then(iso_duration);
    let periods: Vec<_> = kids(root, "Period").collect();
    let period = match periods.as_slice() {
        [p] => *p,
        [] => return Err(err("no Period")),
        _ if live => periods[0],
        _ => {
            return Err(Error::Unsupported(
                "multi-period DASH streams (for example with ad breaks) are not supported yet"
                    .into(),
            ));
        }
    };
    let mpd_base = base_of(root, url)?;
    let period_base = base_of(period, &mpd_base)?;
    let period_secs = period
        .attribute("duration")
        .and_then(iso_duration)
        .or_else(|| {
            let start = period
                .attribute("start")
                .and_then(iso_duration)
                .unwrap_or(0.0);
            total.map(|t| t - start)
        });

    let mut variants = Vec::new();
    let mut audio = Vec::new();
    let mut tracks = HashMap::new();
    let mut protection = None;

    for (si, set) in kids(period, "AdaptationSet").enumerate() {
        let set_base = base_of(set, &period_base)?;
        protection = protection.or_else(|| protection_in(set));
        for (ri, rep) in kids(set, "Representation").enumerate() {
            let kind = kind_of(set, rep);
            if kind == Kind::Other {
                continue;
            }
            protection = protection.or_else(|| protection_in(rep));
            let id = rep
                .attribute("id")
                .map_or_else(|| format!("s{si}r{ri}"), str::to_owned);
            let attr = |k: &str| rep.attribute(k).or_else(|| set.attribute(k));
            let bandwidth: Option<u64> = rep.attribute("bandwidth").and_then(|b| b.parse().ok());
            let codecs = attr("codecs").map(str::to_owned);
            let mime = attr("mimeType").unwrap_or_default();
            let container = if mime.ends_with("/webm") {
                Container::WebM
            } else if mime.ends_with("/mp2t") {
                Container::Ts
            } else {
                Container::Mp4
            };
            let ctx = RepCtx {
                id: &id,
                bandwidth: bandwidth.unwrap_or(0),
                base: base_of(rep, &set_base)?,
                period_secs,
                container,
            };

            let mut template = Template::default();
            template.overlay(Some(period));
            template.overlay(Some(set));
            template.overlay(Some(rep));
            let list = kid(rep, "SegmentList")
                .or_else(|| kid(set, "SegmentList"))
                .or_else(|| kid(period, "SegmentList"));
            let track = if template.present {
                template_track(&template, &ctx)?
            } else if let Some(list) = list {
                list_track(list, &ctx)?
            } else {
                Track {
                    init: None,
                    segments: Vec::new(),
                    container,
                    whole: Some(ctx.base.clone()),
                }
            };
            tracks.insert(id.clone(), track);

            match kind {
                Kind::Video => {
                    let width = attr("width").and_then(|w| w.parse().ok());
                    let height = attr("height").and_then(|h| h.parse().ok());
                    variants.push(Variant {
                        label: variant_label(height, bandwidth, codecs.as_deref()),
                        id,
                        bandwidth,
                        width,
                        height,
                        codecs,
                        frame_rate: attr("frameRate").and_then(frame_rate),
                        audio_group: None,
                    });
                }
                Kind::Audio => {
                    let language = set.attribute("lang").map(str::to_owned);
                    let name = kid(set, "Label").and_then(|l| l.text()).map(str::to_owned);
                    audio.push(AudioTrack {
                        label: audio_label(name.as_deref(), language.as_deref(), bandwidth),
                        id,
                        group: None,
                        name,
                        language,
                        codecs,
                        bandwidth,
                        default: false,
                    });
                }
                Kind::Other => {}
            }
        }
    }

    // Audio-only streams: offer the audio renditions as the variants.
    if variants.is_empty() {
        variants = audio
            .drain(..)
            .map(|a| Variant {
                id: a.id,
                bandwidth: a.bandwidth,
                width: None,
                height: None,
                codecs: a.codecs,
                frame_rate: None,
                audio_group: None,
                label: a.label,
            })
            .collect();
    }
    if variants.is_empty() {
        return Err(Error::Empty);
    }
    variants.sort_by_key(|v| std::cmp::Reverse(v.bandwidth));
    audio.sort_by_key(|a| std::cmp::Reverse(a.bandwidth));
    if let Some(first) = audio.first_mut() {
        first.default = true;
    }

    Ok(Mpd {
        manifest: Manifest {
            kind: StreamKind::Dash,
            url: url.to_string(),
            variants,
            audio,
            duration: total.or(period_secs),
            live,
            protection,
        },
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://cdn.example.com/vod/manifest.mpd").unwrap()
    }

    #[test]
    fn durations() {
        assert_eq!(iso_duration("PT1H2M3.5S"), Some(3723.5));
        assert_eq!(iso_duration("PT0.5S"), Some(0.5));
        assert_eq!(iso_duration("P1DT1S"), Some(86401.0));
        assert_eq!(iso_duration("PT"), Some(0.0));
        assert_eq!(iso_duration("1H"), None);
        assert_eq!(iso_duration("PT5"), None);
    }

    #[test]
    fn templates() {
        let t = "seg-$RepresentationID$-$Number%05d$-$Time$-$Bandwidth$-$$.m4s";
        assert_eq!(
            expand_template(t, "v1", 42, 90000, 800000).unwrap(),
            "seg-v1-00042-90000-800000-$.m4s"
        );
        assert!(expand_template("$Number", "a", 1, 0, 0).is_err());
        assert!(expand_template("$Foo$", "a", 1, 0, 0).is_err());
    }

    const NUMBER_MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT10S">
  <BaseURL>media/</BaseURL>
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/$Number$.m4s" startNumber="1" duration="4" timescale="1"/>
      <Representation id="720p" bandwidth="3000000" width="1280" height="720" codecs="avc1.64001f" frameRate="30000/1001"/>
      <Representation id="360p" bandwidth="800000" width="640" height="360" codecs="avc1.4d401e"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
      <Label>English</Label>
      <Representation id="aac" bandwidth="128000" codecs="mp4a.40.2">
        <SegmentTemplate initialization="audio/init.mp4" media="audio/$Time$.m4s" timescale="48000">
          <SegmentTimeline><S t="0" d="192000" r="1"/><S d="96000"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
    <AdaptationSet contentType="text" mimeType="text/vtt"><Representation id="subs" bandwidth="1"/></AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn number_and_time_templates() {
        let mpd = parse(NUMBER_MPD, &url()).unwrap();
        let m = &mpd.manifest;
        assert!(!m.live);
        assert_eq!(m.duration, Some(10.0));
        assert_eq!(
            m.variants.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["720p", "360p"]
        );
        let fps = m.variants[0].frame_rate.unwrap();
        assert!((fps - 29.97).abs() < 0.01);
        assert_eq!(m.audio[0].label, "English · en · 128 kbps");
        assert!(!mpd.tracks.contains_key("subs"));

        let v = &mpd.tracks["720p"];
        assert_eq!(
            v.init.as_ref().unwrap().url.as_str(),
            "https://cdn.example.com/vod/media/720p/init.mp4"
        );
        let urls: Vec<_> = v.segments.iter().map(|s| s.url.path().to_owned()).collect();
        assert_eq!(
            urls,
            [
                "/vod/media/720p/1.m4s",
                "/vod/media/720p/2.m4s",
                "/vod/media/720p/3.m4s"
            ]
        );

        let a = &mpd.tracks["aac"];
        let times: Vec<_> = a
            .segments
            .iter()
            .map(|s| s.url.path().rsplit('/').next().unwrap().to_owned())
            .collect();
        assert_eq!(times, ["0.m4s", "192000.m4s", "384000.m4s"]);
    }

    #[test]
    fn repeat_until_period_end_and_lists_and_base() {
        let xml = r#"<MPD type="static" mediaPresentationDuration="PT9S">
  <Period>
    <AdaptationSet mimeType="video/mp4">
      <Representation id="r" bandwidth="1">
        <SegmentTemplate media="s$Number$.m4s" timescale="10"><SegmentTimeline><S t="0" d="20" r="-1"/></SegmentTimeline></SegmentTemplate>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4">
      <Representation id="list" bandwidth="1">
        <BaseURL>https://other.example.com/a.mp4</BaseURL>
        <SegmentList><Initialization range="0-99"/><SegmentURL mediaRange="100-199"/><SegmentURL mediaRange="200-299"/></SegmentList>
      </Representation>
      <Representation id="whole" bandwidth="2"><BaseURL>whole.m4a</BaseURL><SegmentBase indexRange="0-10"/></Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = parse(xml, &url()).unwrap();
        assert_eq!(mpd.tracks["r"].segments.len(), 5, "ceil(90 / 20)");
        let list = &mpd.tracks["list"];
        assert_eq!(list.init.as_ref().unwrap().range, Some(0..100));
        assert_eq!(list.segments[1].range, Some(200..300));
        assert_eq!(
            list.segments[1].url.as_str(),
            "https://other.example.com/a.mp4"
        );
        let whole = &mpd.tracks["whole"];
        assert_eq!(
            whole.whole.as_ref().unwrap().as_str(),
            "https://cdn.example.com/vod/whole.m4a"
        );
    }

    #[test]
    fn drm_live_and_unsupported_are_reported() {
        let drm = r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period><AdaptationSet mimeType="video/mp4">
            <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
            <Representation id="v" bandwidth="1"><SegmentTemplate media="$Number$.m4s" duration="2"/></Representation>
            </AdaptationSet></Period></MPD>"#;
        assert!(parse(drm, &url()).unwrap().manifest.protection.is_some());

        let live = drm.replace("type=\"static\"", "type=\"dynamic\"").replace(
            "<ContentProtection schemeIdUri=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"/>",
            "",
        );
        assert!(parse(&live, &url()).unwrap().manifest.live);

        let two = "<MPD type=\"static\"><Period/><Period/></MPD>";
        assert!(matches!(parse(two, &url()), Err(Error::Unsupported(_))));
        assert!(parse("<html/>", &url()).is_err());
    }
}
