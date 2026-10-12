//! `rdm` — headless command-line front end for the rdm download engine.

mod render;
mod units;

use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Args, Parser, Subcommand};
use rdm_core::{
    Credentials, Destination, DownloadSpec, Engine, EngineConfig, Outcome, ProxyConfig, Request,
    RetryPolicy,
};
use tracing_subscriber::EnvFilter;

/// Rust Download Manager (headless).
///
/// Downloads use several connections and resume automatically: run the same
/// command again after an interruption (Ctrl-C, crash, reboot) to continue.
#[derive(Debug, Parser)]
#[command(name = "rdm", version, about, long_about)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Print only errors and the final result.
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Show debug logs (-v) or trace logs (-vv).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show what the server reports about a URL without downloading it.
    Probe {
        url: String,
        #[command(flatten)]
        net: NetArgs,
    },
    /// Download a URL. If a partial download exists, it is resumed.
    Get {
        url: String,
        /// Output file, or an existing directory to save into
        /// [default: current directory, server-suggested name].
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        download: DownloadArgs,
        #[command(flatten)]
        net: NetArgs,
    },
    /// Resume a partial download from its file or its `.rdm` metadata.
    Resume {
        path: PathBuf,
        #[command(flatten)]
        download: DownloadArgs,
        #[command(flatten)]
        net: NetArgs,
    },
}

#[derive(Debug, Args)]
struct DownloadArgs {
    /// Number of connections to start with (1-32).
    #[arg(short = 'c', long, default_value_t = 8,
          value_parser = clap::value_parser!(u16).range(1..=32))]
    connections: u16,

    /// Speed limit in bytes per second, e.g. 500K or 2M (K = 1024).
    #[arg(short, long, value_parser = units::parse_rate)]
    limit: Option<NonZeroU64>,

    /// Never split a range into pieces smaller than this.
    #[arg(long, default_value = "1M", value_parser = units::parse_size)]
    min_split: u64,

    /// Ask for at most this many bytes per request, e.g. 10M. Some servers
    /// (YouTube's, for one) slow long requests down but not short ones.
    #[arg(long, value_parser = units::parse_size)]
    request_size: Option<u64>,

    /// Consecutive failures per connection before giving up.
    #[arg(long, default_value_t = 8)]
    retries: u32,

    /// Replace an existing file that is not a partial rdm download.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Debug, Args)]
struct NetArgs {
    /// Extra request header, e.g. -H 'Accept-Language: en' (repeatable).
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    headers: Vec<String>,

    /// Referer header; many hosts require the page the link came from.
    #[arg(short = 'e', long)]
    referer: Option<String>,

    /// User-Agent header.
    #[arg(short = 'A', long)]
    user_agent: Option<String>,

    /// Cookies, as a Cookie header value: 'session=abc; theme=dark'.
    #[arg(short = 'b', long)]
    cookie: Option<String>,

    /// HTTP Basic auth as USER or USER:PASSWORD. With just USER, the
    /// password is read from the RDM_PASSWORD environment variable.
    #[arg(short, long, value_name = "USER[:PASSWORD]")]
    user: Option<String>,

    /// Proxy URL (http://, https://, socks5://, socks5h://). Credentials
    /// may be embedded: socks5://user:pass@host:1080.
    #[arg(long, conflicts_with = "no_proxy")]
    proxy: Option<String>,

    /// Ignore the system proxy settings.
    #[arg(long)]
    no_proxy: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            render::clear_line();
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "rdm_core=debug,rdm=debug,info",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_env("RDM_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let engine = Engine::new(EngineConfig::default());
    match cli.command {
        Command::Probe { url, net } => {
            let request = build_request(&url, &net)?;
            let info = engine.probe(&request).await.context("probe failed")?;
            render::print_probe(&info);
            Ok(ExitCode::SUCCESS)
        }
        Command::Get {
            url,
            output,
            download,
            net,
        } => {
            let destination = match output {
                None => Destination::Directory(PathBuf::from(".")),
                Some(p) if p.is_dir() || ends_with_separator(&p) => Destination::Directory(p),
                Some(p) => Destination::File(p),
            };
            let request = build_request(&url, &net)?;
            download_with(&engine, request, destination, &download, cli.quiet).await
        }
        Command::Resume {
            path,
            download,
            net,
        } => {
            let partial = rdm_core::inspect_partial(&path)?
                .with_context(|| format!("no partial download found for {}", path.display()))?;
            let file = if path
                .extension()
                .is_some_and(|e| e == rdm_core::SIDECAR_EXTENSION)
            {
                path.with_extension("")
            } else {
                path
            };
            let request = build_request(&partial.url, &net)?;
            download_with(
                &engine,
                request,
                Destination::File(file),
                &download,
                cli.quiet,
            )
            .await
        }
    }
}

async fn download_with(
    engine: &Engine,
    request: Request,
    destination: Destination,
    args: &DownloadArgs,
    quiet: bool,
) -> Result<ExitCode> {
    let mut spec = DownloadSpec::new(request, destination);
    spec.connections = NonZeroUsize::new(args.connections.into()).unwrap_or(NonZeroUsize::MIN);
    spec.min_split = args.min_split.max(64 * 1024);
    spec.max_request_size = args.request_size.and_then(NonZeroU64::new);
    spec.speed_limit = args.limit;
    spec.retry = RetryPolicy {
        max_retries: args.retries,
        ..RetryPolicy::default()
    };
    spec.overwrite = args.overwrite;

    let handle = engine.start(spec)?;
    let renderer = tokio::spawn(render::progress(handle.subscribe(), quiet));

    // First Ctrl-C: stop gracefully (checkpoint, keep the partial file).
    // Second Ctrl-C: exit immediately; the last checkpoint still resumes.
    let control = handle.control();
    let signals = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            render::clear_line();
            eprintln!("Stopping... (press Ctrl-C again to quit immediately)");
            control.stop();
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        }
    });

    let result = handle.wait().await;
    signals.abort();
    let _ = renderer.await;
    match result? {
        Outcome::Completed {
            path,
            size,
            resumed_from,
            elapsed,
        } => {
            if !quiet {
                render::print_completed(&path, size, resumed_from, elapsed);
            }
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Outcome::Stopped { path, downloaded } => {
            eprintln!(
                "Stopped with {} saved. Run the same command again{} to continue.",
                units::bytes(downloaded),
                path.map(|p| format!(" (or `rdm resume {}`)", p.display()))
                    .unwrap_or_default()
            );
            Ok(ExitCode::from(130))
        }
        Outcome::Cancelled => {
            eprintln!("Cancelled.");
            Ok(ExitCode::from(130))
        }
    }
}

fn build_request(url: &str, net: &NetArgs) -> Result<Request> {
    let mut request = Request::new(url)?;
    for h in &net.headers {
        let (name, value) = h
            .split_once(':')
            .with_context(|| format!("header `{h}` must look like `Name: value`"))?;
        request = request.header(name.trim(), value.trim());
    }
    if let Some(r) = &net.referer {
        request = request.referer(r);
    }
    if let Some(ua) = &net.user_agent {
        request = request.user_agent(ua);
    }
    if let Some(c) = &net.cookie {
        request = request.cookies(c);
    }
    if let Some(user) = &net.user {
        let credentials = match user.split_once(':') {
            Some((u, p)) => Credentials::new(u, Some(p.to_owned())),
            None => Credentials::new(user.as_str(), std::env::var("RDM_PASSWORD").ok()),
        };
        request = request.basic_auth(credentials);
    }
    request = request.proxy(match (&net.proxy, net.no_proxy) {
        (Some(url), _) => ProxyConfig::Custom {
            url: url.clone(),
            auth: None,
        },
        (None, true) => ProxyConfig::Direct,
        (None, false) => ProxyConfig::System,
    });
    if let ProxyConfig::Custom { url, .. } = &request.proxy
        && !["http://", "https://", "socks5://", "socks5h://"]
            .iter()
            .any(|s| url.starts_with(s))
    {
        bail!("unsupported proxy `{url}`: use http://, https://, socks5:// or socks5h://");
    }
    Ok(request)
}

fn ends_with_separator(p: &Path) -> bool {
    p.as_os_str()
        .to_string_lossy()
        .ends_with(std::path::is_separator)
}
