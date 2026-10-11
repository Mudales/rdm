//! Terminal output: probe report, live progress line, summaries.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use rdm_core::{Event, ProbeInfo, Progress, SegmentInfo, State};
use tokio::sync::broadcast;

use crate::units;

const BAR_WIDTH: usize = 30;
/// When stderr is not a terminal, print a progress line this often.
const PLAIN_INTERVAL: Duration = Duration::from_secs(5);

pub fn print_probe(info: &ProbeInfo) {
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    println!("URL:            {}", info.url);
    if info.final_url != info.url {
        println!("Final URL:      {}", info.final_url);
    }
    println!(
        "Size:           {}",
        info.size.map_or("unknown".into(), |s| format!(
            "{} ({s} bytes)",
            units::bytes(s)
        ))
    );
    println!(
        "Resumable:      {}",
        if info.resumable {
            "yes (byte ranges supported; multi-connection)"
        } else {
            "no (single connection, restarts from zero if interrupted)"
        }
    );
    println!("File name:      {}", info.filename);
    println!("Content-Type:   {}", opt(&info.content_type));
    println!("ETag:           {}", opt(&info.etag));
    println!("Last-Modified:  {}", opt(&info.last_modified));
}

pub fn print_completed(path: &Path, size: u64, resumed_from: u64, elapsed: Duration) {
    clear_line();
    let fetched = size.saturating_sub(resumed_from);
    let secs = elapsed.as_secs_f64().max(0.001);
    eprintln!(
        "Saved {} ({}) in {}, average {}/s{}",
        path.display(),
        units::bytes(size),
        if elapsed < Duration::from_secs(1) {
            format!("{:.2}s", elapsed.as_secs_f64())
        } else {
            units::duration(elapsed)
        },
        units::bytes((fetched as f64 / secs) as u64),
        if resumed_from > 0 {
            format!(", {} of it from an earlier run", units::bytes(resumed_from))
        } else {
            String::new()
        }
    );
}

/// Erases the live progress line, if one is showing.
pub fn clear_line() {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[2K");
    }
}

/// Consumes events until the download reaches a final state.
pub async fn progress(mut rx: broadcast::Receiver<Event>, quiet: bool) {
    let tty = std::io::stderr().is_terminal();
    let mut last_plain = Instant::now();
    loop {
        let event = match rx.recv().await {
            Ok(e) => e,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        match event {
            Event::Started {
                path,
                info,
                resumed_from,
                segmented,
            } if !quiet => {
                eprintln!("Saving to {}", path.display());
                if resumed_from > 0 {
                    eprintln!(
                        "Resuming: {} of {} already downloaded",
                        units::bytes(resumed_from),
                        info.size.map_or("?".into(), units::bytes)
                    );
                }
                if !segmented {
                    eprintln!(
                        "Note: the server does not support byte ranges or did not send a size; \
                         using one connection (not resumable)"
                    );
                }
            }
            Event::Progress(p) if !quiet => {
                if tty {
                    eprint!("\r\x1b[2K{}", progress_line(&p, true));
                    let _ = std::io::stderr().flush();
                } else if last_plain.elapsed() >= PLAIN_INTERVAL {
                    last_plain = Instant::now();
                    eprintln!("{}", progress_line(&p, false));
                }
            }
            Event::Retrying {
                attempt,
                delay,
                reason,
            } if !quiet => {
                clear_line();
                eprintln!("retry #{attempt} in {:.1}s: {reason}", delay.as_secs_f64());
            }
            Event::Warning(w) => {
                clear_line();
                eprintln!("warning: {w}");
            }
            Event::State(
                State::Completed | State::Stopped | State::Cancelled | State::Failed(_),
            ) => {
                clear_line();
                return;
            }
            _ => {}
        }
    }
}

fn progress_line(p: &Progress, with_bar: bool) -> String {
    let pct = p
        .total
        .filter(|&t| t > 0)
        .map(|t| format!("{:5.1}%", p.downloaded as f64 * 100.0 / t as f64))
        .unwrap_or_else(|| "    ?%".into());
    let bar = match (with_bar, p.total) {
        (true, Some(total)) if total > 0 => format!(" [{}]", segment_bar(&p.segments, total)),
        _ => String::new(),
    };
    format!(
        "{pct}{bar} {} / {}  {}/s  ETA {}  {} conn",
        units::bytes(p.downloaded),
        p.total.map_or("?".into(), units::bytes),
        units::bytes(p.speed),
        p.eta.map_or("--".into(), units::duration),
        p.connections,
    )
}

/// One character per slice of the file, shaded by how much of it is done.
/// Everything outside the unfinished segments is complete.
fn segment_bar(segments: &[SegmentInfo], total: u64) -> String {
    const SHADES: [char; 5] = [' ', '░', '▒', '▓', '█'];
    (0..BAR_WIDTH)
        .map(|i| {
            let lo = total * i as u64 / BAR_WIDTH as u64;
            let hi = (total * (i as u64 + 1) / BAR_WIDTH as u64).max(lo + 1);
            let pending: u64 = segments
                .iter()
                .map(|s| s.end.min(hi).saturating_sub(s.position.max(lo)))
                .sum();
            let done = 1.0 - (pending as f64 / (hi - lo) as f64).min(1.0);
            let idx = (done * (SHADES.len() - 1) as f64).round() as usize;
            SHADES.get(idx).copied().unwrap_or(' ')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_shows_pending_ranges() {
        let total = 300;
        let segs = [SegmentInfo {
            start: 0,
            position: 0,
            end: 150,
            active: true,
        }];
        let bar = segment_bar(&segs, total);
        assert_eq!(bar.chars().count(), BAR_WIDTH);
        assert!(bar.starts_with("               ")); // first half pending
        assert!(bar.ends_with("███████████████")); // second half done
    }
}
