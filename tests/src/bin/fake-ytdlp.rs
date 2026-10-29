//! A stand-in for yt-dlp in tests. It reads `fake-ytdlp.json` next to its
//! own executable (copy the binary into a test's temp folder), so tests can
//! run in parallel:
//!
//! ```json
//! { "info": { ...what `--dump-single-json` prints... },
//!   "error": "ERROR: ...",            // print this and exit 1
//!   "ansi": true,                     // print like yt-dlp on Windows
//!   "download": { "bytes": 1000, "ext": "mp4" } }
//! ```
//!
//! With `"ansi"`, whatever yt-dlp prints for a human is encoded in the
//! system's ANSI code page instead of UTF-8, as the real one does on
//! Windows; `--print-to-file` stays UTF-8, as the real one does too.
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
    let ansi = cfg["ansi"].as_bool().unwrap_or(false);
    if let Some(err) = cfg.get("error").and_then(|e| e.as_str()) {
        write(&mut std::io::stderr(), &format!("{err}\n"), ansi);
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
        // Progress is JSON, which yt-dlp escapes to ASCII; it survives any
        // code page. Only with `--progress` is it printed at all.
        if args.iter().any(|a| a == "--progress") {
            println!(
                "RDM {}",
                serde_json::json!({"status": "downloading", "downloaded_bytes": written, "total_bytes": total, "speed": 1000.0, "eta": 1})
            );
        }
    }
    // `--print-to-file TEMPLATE FILE`, which yt-dlp always writes as UTF-8.
    if let Some(i) = args.iter().position(|a| a == "--print-to-file")
        && let Some(file) = args.get(i + 2)
    {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .and_then(|mut f| writeln!(f, "{path}"));
    }
    write(&mut std::io::stdout(), &format!("RDMFILE {path}\n"), ansi);
}

/// Prints `text`, encoded in an ANSI code page (Hebrew, 1255) rather than
/// UTF-8 when `ansi` is set, dropping what that code page cannot hold:
/// `s.encode(enc, 'ignore')`, as yt-dlp's `write_string` does on Windows.
fn write(out: &mut impl Write, text: &str, ansi: bool) {
    let bytes: Vec<u8> = if ansi {
        text.chars()
            .filter_map(|c| match c {
                c if c.is_ascii() => Some(c as u8),
                '\u{5d0}'..='\u{5ea}' => Some(0xe0 + (c as u32 - 0x5d0) as u8),
                _ => None,
            })
            .collect()
    } else {
        text.as_bytes().to_vec()
    };
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}
