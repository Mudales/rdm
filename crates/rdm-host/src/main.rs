//! `rdm-host` — native messaging host for the rdm browser extension.
//!
//! Started by the browser, it reads length-prefixed JSON requests on stdin,
//! relays each one to the running rdm app over the local IPC socket
//! (launching the app if needed), and writes the reply to stdout. Nothing
//! else may be written to stdout: logs go to stderr, which browsers show in
//! their own logs.
//!
//! Run by hand, it registers itself with browsers:
//!
//! ```text
//! rdm-host install [--browser NAME]... [--extension-id ID]... [--firefox-id ID]
//! rdm-host uninstall [--browser NAME]...
//! rdm-host status
//! ```

use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rdm_host::APP_ENV;
use rdm_host::install::{Browser, Installer, Removal};
use rdm_ipc::{Client, Endpoint, Request, Response, read_frame, write_frame};
use tracing_subscriber::EnvFilter;

/// How long to wait for a freshly launched app to start listening.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RDM_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("install") => install(&args[1..]),
        Some("uninstall") => uninstall(&args[1..]),
        Some("status") => status(),
        Some("-h" | "--help" | "help") => {
            println!(
                "rdm-host: native messaging host for the rdm browser extension.\n\n\
                 rdm-host install [--browser NAME]... [--extension-id ID]... [--firefox-id ID]\n\
                 rdm-host uninstall [--browser NAME]...\n\
                 rdm-host status\n\n\
                 Browsers: chrome, chromium, edge, brave, firefox. Without --browser,\n\
                 install targets the browsers found on this system and uninstall all."
            );
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("rdm-host {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        // Browsers pass the caller's origin (Chrome) or the manifest path and
        // extension id (Firefox) as arguments.
        _ => return host(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Host mode
// ---------------------------------------------------------------------------

fn host() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        // `sendNativeMessage` sends one message and closes stdin;
        // `connectNative` keeps the pipe open. Serve until EOF either way.
        loop {
            let response = match read_frame::<_, Request>(&mut stdin).await {
                Ok(Some(request)) => relay(&request).await,
                Ok(None) => return ExitCode::SUCCESS,
                Err(e) => Response::error(format!("bad message from the browser: {e}")),
            };
            if let Err(e) = write_frame(&mut stdout, &response).await {
                tracing::error!("cannot reply to the browser: {e}");
                return ExitCode::FAILURE;
            }
        }
    })
}

async fn relay(request: &Request) -> Response {
    let endpoint = Endpoint::for_current_user();
    let mut client = match Client::connect(&endpoint).await {
        Ok(c) => c,
        Err(_) if !request.launches_app() => return Response::NotRunning,
        Err(_) => match launch_and_connect(&endpoint).await {
            Ok(c) => c,
            Err(e) => return Response::error(format!("{e:#}")),
        },
    };
    client
        .call(request)
        .await
        .unwrap_or_else(|e| Response::error(format!("rdm did not answer: {e}")))
}

async fn launch_and_connect(endpoint: &Endpoint) -> Result<Client> {
    let app = app_path()?;
    tracing::info!("starting {}", app.display());
    let mut cmd = std::process::Command::new(&app);
    // Never let the app inherit the browser's pipes.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("could not start rdm ({})", app.display()))?;

    let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Ok(client) = Client::connect(endpoint).await {
            return Ok(client);
        }
        if tokio::time::Instant::now() > deadline {
            bail!("rdm was started but did not become ready in time");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The app executable: `$RDM_APP`, the path in an `rdm-app-path` file next
/// to this binary (a copy made for an AppImage), or `rdm-gui` next to it.
fn app_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(APP_ENV).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().context("cannot locate rdm-host")?;
    let dir = exe.parent().context("rdm-host has no parent directory")?;
    if let Ok(recorded) = std::fs::read_to_string(dir.join(rdm_host::install::APP_PATH_FILE)) {
        let app = PathBuf::from(recorded.trim());
        if app.is_file() {
            return Ok(app);
        }
    }
    let names: &[&str] = if cfg!(windows) {
        &["rdm-gui.exe", "rdm.exe"]
    } else {
        &["rdm-gui", "rdm"]
    };
    names
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
        .with_context(|| {
            format!(
                "rdm is not installed next to rdm-host (looked in {})",
                dir.display()
            )
        })
}

/// Starts the app outside the browser's process tree, so it keeps running
/// when the browser closes the host.
fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Chrome runs hosts in a job object; break away from it so the app
        // survives the host exiting.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
}

// ---------------------------------------------------------------------------
// Installer commands
// ---------------------------------------------------------------------------

struct InstallArgs {
    browsers: Vec<Browser>,
    installer: Installer,
}

fn parse_install_args(args: &[String]) -> Result<InstallArgs> {
    let host = std::env::current_exe().context("cannot locate rdm-host")?;
    let mut installer = Installer::new(host)?;
    let mut browsers = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--browser" | "-b" => {
                let v = value()?;
                if v == "all" {
                    browsers.extend(Browser::ALL);
                } else {
                    browsers.push(Browser::parse(v)?);
                }
            }
            "--extension-id" => installer.allow_chrome_extension(value()?.clone()),
            "--firefox-id" => installer.set_firefox_extension(value()?.clone()),
            other => bail!("unknown argument `{other}` (see rdm-host --help)"),
        }
    }
    browsers.dedup();
    Ok(InstallArgs {
        browsers,
        installer,
    })
}

fn install(args: &[String]) -> Result<()> {
    let InstallArgs {
        mut browsers,
        installer,
    } = parse_install_args(args)?;
    if browsers.is_empty() {
        browsers = Browser::ALL
            .into_iter()
            .filter(|b| installer.detected(*b))
            .collect();
        if browsers.is_empty() {
            bail!("no supported browser found; pass --browser NAME to install anyway");
        }
    }
    for b in browsers {
        let path = installer.install(b)?;
        println!("{:<9} installed: {}", b.name(), path.display());
    }
    Ok(())
}

fn uninstall(args: &[String]) -> Result<()> {
    let InstallArgs {
        mut browsers,
        installer,
    } = parse_install_args(args)?;
    if browsers.is_empty() {
        browsers = Browser::ALL.to_vec();
    }
    for b in browsers {
        let outcome = match installer.uninstall(b)? {
            Removal::Removed => "removed".to_owned(),
            Removal::NotInstalled => "was not installed".to_owned(),
            Removal::OtherCopy(other) => {
                format!("kept: registered to another copy ({})", other.display())
            }
        };
        println!("{:<9} {outcome}", b.name());
    }
    Ok(())
}

fn status() -> Result<()> {
    let host = std::env::current_exe().context("cannot locate rdm-host")?;
    let installer = Installer::new(host)?;
    for s in installer.status() {
        let state = match (s.installed, s.current) {
            (true, true) => "installed",
            (true, false) => "installed (points to another rdm-host)",
            (false, _) if s.detected => "not installed",
            (false, _) => "not installed (browser not found)",
        };
        println!("{:<9} {state}", s.name);
    }
    let endpoint = Endpoint::for_current_user();
    println!("app socket: {endpoint}");
    Ok(())
}
