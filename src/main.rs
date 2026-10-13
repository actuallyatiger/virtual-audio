// Release builds on Windows run without a console window; use the tray icon.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod backend;
mod config;
mod decode;
mod dsp;
mod engine;
mod hotkeys;
mod mixer;
mod music;
mod platform;
mod server;
mod soundboard;
#[cfg(windows)]
mod tray;

use config::{Channel, Config, DeviceRef, SourceConfig, SourceKind};

/// This build's version (from Cargo.toml, which releases bump).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
use std::sync::Arc;

/// Whether version `a` is older than `b`. Copies from before versions
/// ("unknown") count as older.
pub fn is_older(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|n| n.parse().ok()).collect() };
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a < b,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// The version of the copy running on `port`, asked over its API. `None` if
/// nothing answers like the mixer.
fn running_version(port: u16) -> Option<String> {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = std::time::Duration::from_secs(2);
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    write!(
        stream,
        "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let body = &response[response.find("\r\n\r\n")? + 4..];
    let state: serde_json::Value = serde_json::from_str(body).ok()?;
    state.get("platform_supported")?;
    Some(state["version"].as_str().unwrap_or("unknown").to_owned())
}

/// Asks the copy running on `port` to quit, and takes the port once it has.
fn take_over(port: u16) -> anyhow::Result<std::net::TcpListener> {
    use std::io::Write;
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    write!(
        stream,
        "POST /api/quit HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    drop(stream);
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            return Ok(listener);
        }
    }
    anyhow::bail!("the running copy didn't close")
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Re-launched elevated to register or remove the sign-in task, then exit
    // (see platform.rs). Before anything else: no port, no logging.
    if let Some(i) = args.iter().position(|a| a == "--admin-autostart") {
        let enabled = args.get(i + 1).is_some_and(|a| a == "on");
        let user = args
            .iter()
            .position(|a| a == "--user")
            .and_then(|j| args.get(j + 1))
            .map_or("", String::as_str);
        return platform::admin_task_helper(enabled, user);
    }

    let background = std::env::args().any(|a| a == "--background");
    let verbose = std::env::args().any(|a| a == "--verbose");

    let loaded = Config::read();
    let port = match &loaded {
        Ok(Some(config)) => config.port,
        _ => config::DEFAULT_PORT,
    };
    let url = format!("http://127.0.0.1:{port}/");

    // The port doubles as a single-instance lock: if it is taken, another
    // copy is running, so just show its UI. This happens before logging is
    // set up so a second launch doesn't rotate the running copy's log.
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(_) if background => return Ok(()),
        Err(_) => match running_version(port) {
            // An older copy (often the administrator one starting at sign-in)
            // would otherwise keep running and hide this one.
            Some(running)
                if is_older(&running, VERSION)
                    && platform::ask(&format!(
                        "An older Virtual Audio Mixer ({running}) is running. \
                         Close it and start this version ({VERSION})?"
                    )) =>
            {
                take_over(port)?
            }
            _ => {
                platform::shell_open(&url);
                return Ok(());
            }
        },
    };
    listener.set_nonblocking(true)?;

    init_logging(verbose);
    tracing::info!(
        "Virtual Audio Mixer {} starting, args: {:?}",
        VERSION,
        std::env::args().skip(1).collect::<Vec<_>>()
    );
    let (mut config, first_run) = match loaded {
        Ok(Some(config)) => (config, false),
        Ok(None) => (Config::default(), true),
        Err(e) => match Config::back_up_unreadable() {
            Ok(backup) => {
                tracing::error!(
                    "config.json is unreadable ({e}); moved it to {} and started fresh",
                    backup.display()
                );
                (Config::default(), true)
            }
            Err(io) => {
                // Starting with defaults would overwrite the only copy.
                tracing::error!(
                    "config.json is unreadable ({e}) and could not be backed up ({io}); \
                     not starting so it isn't overwritten. Fix or remove it and try again."
                );
                anyhow::bail!("unreadable config.json could not be backed up: {io}");
            }
        },
    };

    let backend = backend::create();
    if first_run {
        apply_first_run_defaults(&mut config, &*backend);
    }
    let engine = engine::Engine::new(backend, config);
    hotkeys::spawn(engine.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    #[cfg(windows)]
    {
        let quit: Arc<dyn Fn() + Send + Sync> = Arc::new(tray::quit_handle());
        let router = server::router(engine.clone(), port, quit.clone());
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!("web server stopped: {e}");
            }
        });
        tracing::info!("UI at {url}");
        if !background {
            platform::shell_open(&url);
        }
        if let Err(e) = tray::run(url, move || quit()) {
            tracing::error!("tray icon failed: {e:#}");
        }
    }

    #[cfg(not(windows))]
    {
        let stop = Arc::new(tokio::sync::Notify::new());
        let notify = stop.clone();
        let quit: Arc<dyn Fn() + Send + Sync> = Arc::new(move || notify.notify_one());
        let router = server::router(engine.clone(), port, quit);
        println!("Virtual Audio Mixer (dummy audio backend) at {url}");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            tokio::select! {
                r = axum::serve(listener, router) => r?,
                _ = tokio::signal::ctrl_c() => {}
                _ = stop.notified() => {}
            }
            anyhow::Ok(())
        })?;
    }

    tracing::info!("shutting down");
    engine.shutdown();
    runtime.shutdown_timeout(std::time::Duration::from_millis(500));
    Ok(())
}

/// Starts new users with their default mic as a source and, if a VB-Audio
/// virtual cable is installed, routes the mix into it.
fn apply_first_run_defaults(config: &mut Config, backend: &dyn backend::Backend) {
    if config.sources.is_empty() {
        config.sources.push(SourceConfig {
            id: config::new_id("s"),
            name: "Microphone".into(),
            kind: SourceKind::Device { device: None },
            channel: Channel::default(),
            processing: Default::default(),
        });
    }
    if config.output.is_none()
        && let Ok(outputs) = backend.output_devices()
    {
        config.output = outputs
            .into_iter()
            .find(|d| d.name.to_lowercase().contains("cable input"))
            .map(|d| DeviceRef {
                id: d.id,
                name: d.name,
            });
    }
    let _ = config.save();
}

pub fn log_path() -> std::path::PathBuf {
    config::data_dir().join("log.txt")
}

/// Release builds on Windows have no console, so everything (including
/// panics) goes to `%APPDATA%\VirtualAudioMixer\log.txt`. `--verbose`
/// adds debug detail.
fn init_logging(verbose: bool) {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::prelude::*;
    let level = if verbose {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };
    // The audio decoder logs its own errors for files it can't read; we
    // report those ourselves, more readably.
    let filter = Targets::new()
        .with_default(level)
        .with_target("symphonia", LevelFilter::OFF);
    let layer = tracing_subscriber::fmt::layer()
        .with_thread_names(true)
        .with_target(false);
    let file = cfg!(all(windows, not(debug_assertions)))
        .then(|| {
            let _ = std::fs::create_dir_all(config::data_dir());
            // Keep the previous run's log: after a crash, that is the one
            // that matters, and relaunching would otherwise overwrite it.
            let _ = std::fs::rename(log_path(), log_path().with_extension("previous.txt"));
            std::fs::File::create(log_path()).ok()
        })
        .flatten();
    let registry = tracing_subscriber::registry().with(filter);
    match file {
        Some(file) => registry
            .with(
                layer
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file)),
            )
            .init(),
        None => registry.with(layer).init(),
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!("panic in thread {:?}: {info}", thread.name().unwrap_or("?"));
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_older("0.2.0", "0.10.0"));
        assert!(is_older("0.9.9", "1.0.0"));
        assert!(!is_older("0.3.0", "0.2.0"));
        assert!(!is_older("0.2.0", "0.2.0"));
        assert!(is_older("unknown", "0.1.0"));
    }
}
