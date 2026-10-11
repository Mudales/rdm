//! Import and export of the download list.
//!
//! Export formats: rdm's JSON (everything needed to recreate a download,
//! never cookies) or a plain list of URLs. Import accepts either, plus any
//! text with one URL per line (`#` comments allowed), so lists from other
//! download managers or a browser work too.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{DownloadRecord, NewDownload, StartMode, Status, StreamChoice};
use crate::settings::Settings;

const FORMAT: &str = "rdm-downloads";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    Json,
    Urls,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub added: usize,
    /// Already in the list (skipped).
    pub duplicates: usize,
    /// Lines or entries that could not be used, with the reason.
    pub invalid: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportFile {
    format: String,
    version: u32,
    downloads: Vec<ExportItem>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportItem {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    queue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    referer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream: Option<StreamChoice>,
    #[serde(default)]
    completed: bool,
}

pub(crate) fn export(
    records: &[DownloadRecord],
    settings: &Settings,
    format: ExportFormat,
) -> String {
    match format {
        ExportFormat::Urls => {
            let mut out: String = records.iter().map(|r| format!("{}\n", r.url)).collect();
            if out.is_empty() {
                out.push('\n');
            }
            out
        }
        ExportFormat::Json => {
            let file = ExportFile {
                format: FORMAT.into(),
                version: VERSION,
                downloads: records
                    .iter()
                    .map(|r| ExportItem {
                        url: r.url.clone(),
                        file_name: Some(r.file_name.clone()),
                        directory: Some(r.directory.clone()),
                        queue: settings.queue(r.queue_id).map(|q| q.name.clone()),
                        referer: r.referer.clone(),
                        user_agent: r.user_agent.clone(),
                        checksum: r.checksum.clone(),
                        stream: r.stream.clone(),
                        completed: r.status == Status::Completed,
                    })
                    .collect(),
            };
            serde_json::to_string_pretty(&file).unwrap_or_default()
        }
    }
}

/// Parses an import into downloads to add, plus reasons for rejected parts.
pub(crate) fn parse_import(
    text: &str,
    settings: &Settings,
    start: StartMode,
) -> (Vec<NewDownload>, Vec<String>) {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<ExportFile>(trimmed) {
            Ok(file) if file.format == FORMAT => {
                let mut items = Vec::new();
                let mut invalid = Vec::new();
                for item in file.downloads {
                    if rdm_core::Request::new(&item.url).is_err() {
                        invalid.push(format!("{}: not an http(s) URL", item.url));
                        continue;
                    }
                    let queue_id = item
                        .queue
                        .as_deref()
                        .and_then(|name| settings.queues.iter().find(|q| q.name == name))
                        .map(|q| q.id);
                    items.push(NewDownload {
                        file_name: item.file_name.or_else(|| name_from_url(&item.url)),
                        url: item.url,
                        directory: item.directory,
                        queue_id,
                        referer: item.referer,
                        user_agent: item.user_agent,
                        checksum: item.checksum,
                        stream: item.stream,
                        start,
                        ..NewDownload::default()
                    });
                }
                (items, invalid)
            }
            Ok(file) => (
                Vec::new(),
                vec![format!("unknown file format `{}`", file.format)],
            ),
            Err(e) => (Vec::new(), vec![format!("not a valid rdm export: {e}")]),
        };
    }

    let mut items = Vec::new();
    let mut invalid = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in trimmed.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Tolerate "url<TAB>anything" exports from other tools.
        let url = line.split_whitespace().next().unwrap_or_default();
        if rdm_core::Request::new(url).is_err() {
            invalid.push(format!("{line}: not an http(s) URL"));
            continue;
        }
        if !seen.insert(url.to_owned()) {
            continue;
        }
        items.push(NewDownload {
            url: url.to_owned(),
            // Named from the URL so a long list imports without probing
            // every server first.
            file_name: name_from_url(url),
            start,
            ..NewDownload::default()
        });
    }
    (items, invalid)
}

fn name_from_url(url: &str) -> Option<String> {
    let u = rdm_core::Url::parse(url).ok()?;
    let last = u.path_segments()?.next_back()?.to_owned();
    if last.is_empty() || rdm_media::looks_like_stream(url, None) {
        return None;
    }
    let decoded = percent_decode(&last);
    Some(rdm_core::sanitize_filename(&decoded))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn url_lists() {
        let s = Settings::with_directory(Path::new("/dl"));
        let text = "# my list\n\nhttps://x.org/a%20b.zip\nhttps://x.org/a%20b.zip\nftp://old/file\nnot a url\nhttps://y.org/v/master.m3u8\thd\n";
        let (items, invalid) = parse_import(text, &s, StartMode::Paused);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].file_name.as_deref(), Some("a b.zip"));
        assert_eq!(items[1].file_name, None, "streams are named when added");
        assert_eq!(invalid.len(), 2);
    }

    #[test]
    fn json_roundtrip_keeps_details_but_never_cookies() {
        let s = Settings::with_directory(Path::new("/dl"));
        let record = DownloadRecord {
            id: 7,
            url: "https://x.org/f.iso".into(),
            file_name: "f.iso".into(),
            directory: "/dl/Archives".into(),
            category_id: Some(4),
            queue_id: 1,
            status: Status::Completed,
            error: None,
            size: Some(10),
            downloaded: 10,
            resumable: Some(true),
            referer: Some("https://x.org/".into()),
            user_agent: None,
            connections: None,
            speed_limit: None,
            added_at: 1,
            completed_at: Some(2),
            position: 0,
            stream: None,
            checksum: Some("md5:5eb63bbbe01eeed093cb22bb8f5acdc3".into()),
            checksum_ok: Some(true),
            scan: None,
        };
        let json = export(&[record], &s, ExportFormat::Json);
        assert!(json.contains("\"queue\": \"Main\""));
        assert!(!json.to_lowercase().contains("cookie"));
        let (items, invalid) = parse_import(&json, &s, StartMode::Queue);
        assert!(invalid.is_empty());
        assert_eq!(items[0].file_name.as_deref(), Some("f.iso"));
        assert_eq!(
            items[0].directory.as_deref(),
            Some(Path::new("/dl/Archives"))
        );
        assert_eq!(items[0].queue_id, Some(1));
        assert_eq!(
            items[0].checksum.as_deref(),
            Some("md5:5eb63bbbe01eeed093cb22bb8f5acdc3")
        );
        assert_eq!(items[0].start, StartMode::Queue);

        assert_eq!(export(&[], &s, ExportFormat::Urls), "\n");
        let (_, bad) = parse_import(
            "{\"format\":\"other\",\"version\":1,\"downloads\":[]}",
            &s,
            StartMode::Paused,
        );
        assert_eq!(bad.len(), 1);
    }
}
