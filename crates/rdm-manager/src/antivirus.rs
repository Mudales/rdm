//! Post-download antivirus hook: runs a user-configured scanner on each
//! finished file. The command is split into arguments here and executed
//! directly (no shell), so file names can never inject commands.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;

const TIMEOUT: Duration = Duration::from_secs(600);

/// Splits a command line on whitespace, honoring "double" and 'single'
/// quotes. Backslashes are literal (Windows paths).
pub(crate) fn split_command(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in cmd.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if started || !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// The program and arguments for scanning `file`.
pub(crate) fn command_for(template: &str, file: &Path) -> Option<(String, Vec<String>)> {
    let mut parts = split_command(template);
    if parts.is_empty() {
        return None;
    }
    let path = file.to_string_lossy();
    let mut used = false;
    for p in parts.iter_mut().skip(1) {
        if p.contains("{file}") {
            *p = p.replace("{file}", &path);
            used = true;
        }
    }
    if !used {
        parts.push(path.into_owned());
    }
    let program = parts.remove(0);
    Some((program, parts))
}

/// Outcome of a scan, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScanResult {
    Clean,
    Flagged(String),
    Failed(String),
}

impl ScanResult {
    pub fn describe(&self) -> String {
        match self {
            ScanResult::Clean => "Scanned: no problems reported".into(),
            ScanResult::Flagged(detail) => format!("Scanner reported a problem: {detail}"),
            ScanResult::Failed(why) => format!("Scan did not run: {why}"),
        }
    }
}

pub(crate) async fn scan(template: &str, file: &Path) -> ScanResult {
    let Some((program, args)) = command_for(template, file) else {
        return ScanResult::Failed("no command configured".into());
    };
    let child = rdm_media::background_command(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return ScanResult::Failed(format!("{program}: {e}")),
    };
    let mut out = String::new();
    let mut err = String::new();
    let (mut so, mut se) = (child.stdout.take(), child.stderr.take());
    let run = async {
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_string(&mut out).await;
        }
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_string(&mut err).await;
        }
        child.wait().await
    };
    match tokio::time::timeout(TIMEOUT, run).await {
        Err(_) => ScanResult::Failed("the scanner did not finish within 10 minutes".into()),
        Ok(Err(e)) => ScanResult::Failed(e.to_string()),
        Ok(Ok(status)) if status.success() => ScanResult::Clean,
        Ok(Ok(status)) => {
            let text = format!("{out}\n{err}");
            let tail: Vec<&str> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(3)
                .collect();
            let code = status
                .code()
                .map_or_else(|| "killed".into(), |c| format!("exit {c}"));
            let detail = tail.into_iter().rev().collect::<Vec<_>>().join(" / ");
            ScanResult::Flagged(if detail.is_empty() {
                code
            } else {
                format!("{code}: {detail}")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_and_placeholders() {
        assert_eq!(
            split_command(r#"clamscan --no-summary {file}"#),
            ["clamscan", "--no-summary", "{file}"]
        );
        assert_eq!(
            split_command(
                r#""C:\Program Files\Windows Defender\MpCmdRun.exe" -Scan -File "{file}""#
            ),
            [
                r"C:\Program Files\Windows Defender\MpCmdRun.exe",
                "-Scan",
                "-File",
                "{file}"
            ]
        );
        assert_eq!(split_command("a '' b"), ["a", "", "b"]);
        assert!(split_command("   ").is_empty());

        let f = Path::new("/tmp/My File; rm -rf ~.zip");
        let (prog, args) = command_for("scan --file={file} -q", f).unwrap();
        assert_eq!(prog, "scan");
        assert_eq!(args, ["--file=/tmp/My File; rm -rf ~.zip", "-q"]);
        let (_, args) = command_for("scan -q", f).unwrap();
        assert_eq!(args.last().unwrap(), "/tmp/My File; rm -rf ~.zip");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scanner_exit_codes() {
        let f = Path::new("/tmp/whatever.bin");
        assert_eq!(scan("true", f).await, ScanResult::Clean);
        match scan("sh -c 'echo FOUND: Eicar >&2; exit 1'", f).await {
            ScanResult::Flagged(d) => assert!(d.contains("exit 1") && d.contains("Eicar"), "{d}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            scan("/nonexistent/scanner", f).await,
            ScanResult::Failed(_)
        ));
    }
}
