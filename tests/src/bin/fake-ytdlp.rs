//! A stand-in for yt-dlp in tests. It reads `fake-ytdlp.json` next to its
//! own executable (copy the binary into a test's temp folder), so tests can
//! run in parallel:
//!
//! ```json
//! { "info": { ...what `--dump-single-json` prints... },
//!   "error": "ERROR: ...",            // print this and exit 1
//!   "download": { "bytes": 1000, "ext": "mp4" } }
//! ```
//!
//! Every run appends its arguments, as a JSON array, to `fake-ytdlp.args`,
//! and copies any `--cookies` file to `fake-ytdlp.cookies`.

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(PathBuf::from))
    else {
        std::process::exit(3);
    };
    let cfg: serde_json::Value = std::fs::read(dir.join("fake-ytdlp.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("fake-ytdlp.args"))
    {
        let _ = writeln!(log, "{}", serde_json::Value::from(args.clone()));
    }
    let value_after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if let Some(cookies) = value_after("--cookies") {
        let _ = std::fs::copy(cookies, dir.join("fake-ytdlp.cookies"));
    }
    if args.iter().any(|a| a == "--version") {
        println!("2099.01.01");
        return;
    }
    if let Some(err) = cfg.get("error").and_then(|e| e.as_str()) {
        eprintln!("{err}");
        std::process::exit(1);
    }
    if args.iter().any(|a| a == "--dump-single-json") {
        println!("{}", cfg["info"]);
        return;
    }

    // Download mode: write `bytes` bytes (a deterministic pattern) to the
    // `-o` template with `%(ext)s` filled in, reporting progress.
    let total = cfg["download"]["bytes"].as_u64().unwrap_or(1000);
    let ext = cfg["download"]["ext"].as_str().unwrap_or("mp4");
    let Some(template) = value_after("-o") else {
        eprintln!("ERROR: no -o");
        std::process::exit(2);
    };
    let path = template.replace("%(ext)s", ext);
    let Ok(mut out) = std::fs::File::create(&path) else {
        eprintln!("ERROR: cannot write {path}");
        std::process::exit(2);
    };
    let chunk = (total / 4).max(1);
    let mut written = 0;
    while written < total {
        let n = chunk.min(total - written);
        let data: Vec<u8> = (written..written + n).map(|i| (i % 251) as u8).collect();
        if out.write_all(&data).is_err() {
            std::process::exit(2);
        }
        written += n;
        println!(
            "RDM {}",
            serde_json::json!({"status": "downloading", "downloaded_bytes": written, "total_bytes": total, "speed": 1000.0, "eta": 1})
        );
    }
    println!("RDMFILE {path}");
}
