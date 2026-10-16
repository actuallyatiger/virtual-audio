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
mod update;

use config::{Channel, Config, DeviceRef, SourceConfig, SourceKind};

/// This build's version (from Cargo.toml, which releases bump).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Whether this is a release build: the release workflow sets VAM_RELEASE.
/// Anything else is a test build, which never updates itself.
pub const RELEASE: bool = option_env!("VAM_RELEASE").is_some();
use std::sync::Arc;

/// This build: its version, plus "-dev" and the commit it was built from
/// (CI sets VAM_COMMIT) for a test build, e.g. "0.2.1-dev.010e2aa".
pub fn build() -> &'static str {
    static BUILD: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| match (RELEASE, option_env!("VAM_COMMIT")) {
            (true, _) => VERSION.to_owned(),
            (false, Some(commit)) => format!("{VERSION}-dev.{commit}"),
            (false, None) => format!("{VERSION}-dev"),
        });
    &BUILD
}

/// Whether an installed copy of build `installed` should give way to this
/// one (see [`replaces_as`]).
pub fn replaces(installed: &str) -> bool {
    replaces_as(build(), RELEASE, installed)
}

/// Whether `this` build replaces an installed `installed` one: a release
/// replaces older versions and test builds (which count as older); a test
/// build replaces anything but itself, so whichever was opened last runs.
fn replaces_as(this: &str, release: bool, installed: &str) -> bool {
    installed != this && (!release || is_older(installed, this))
}

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
    ask_to_quit(port)?;
    wait_for_port(port, 10).map_err(|_| anyhow::anyhow!("the running copy didn't close"))
}

/// Asks the copy running on `port` to quit. Waits for the answer: the
/// server drops a request whose connection closes before it's answered.
fn ask_to_quit(port: u16) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    write!(
        stream,
        "POST /api/quit HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response.lines().next().unwrap_or_default();
    anyhow::ensure!(
        status.starts_with("HTTP/1.1 2"),
        "the running copy answered {status:?}"
    );
    Ok(())
}

/// Waits up to `seconds` for a mixer to answer on `port`.
fn wait_for_mixer(port: u16, seconds: u32) -> bool {
    (0..seconds * 5).any(|_| {
        running_version(port).is_some() || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            false
        }
    })
}

/// The installed copy Start with Windows runs, if it should run in place of
/// this one. A copy for this account is installed, or updated, first; the
/// administrator copy too, if the user agrees to Windows' prompt (opened by
/// hand, not in the `background`).
fn installed_to_run(admin: bool, background: bool) -> Option<platform::Installed> {
    let mut outdated = platform::admin_copy_version().is_none_or(|v| replaces(&v));
    // Otherwise this copy (often a download) keeps running in its place,
    // and its file can't be deleted.
    if admin
        && outdated
        && !background
        && platform::this_copy() != Some(platform::Installed::Admin)
        && platform::ask(&format!(
            "Start with Windows (as administrator) runs another version. Make it this one \
             ({})? Windows will ask for permission.",
            build()
        ))
    {
        match platform::set_admin_autostart(true) {
            Ok(()) => outdated = false,
            Err(e) => tracing::warn!("couldn't update the administrator copy: {e:#}"),
        }
    }
    let autostart = admin || platform::autostart_enabled();
    let copy = platform::hand_over_to(
        autostart,
        admin,
        platform::this_copy(),
        outdated,
        platform::is_elevated(),
    )?;
    if copy == platform::Installed::User
        && let Err(e) = platform::set_autostart(true)
    {
        tracing::warn!("couldn't install the copy Start with Windows runs: {e:#}");
        return None;
    }
    Some(copy)
}

/// Takes the port once the copy that held it has quit (up to `seconds`).
fn wait_for_port(port: u16, seconds: u32) -> std::io::Result<std::net::TcpListener> {
    let mut result = std::net::TcpListener::bind(("127.0.0.1", port));
    for _ in 0..seconds * 10 {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        result = std::net::TcpListener::bind(("127.0.0.1", port));
    }
    result
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
    // Started by an update: the copy it replaces is still letting go of
    // the port.
    let replacing = std::env::args().any(|a| a == "--replacing");
    // Started by another copy as the installed one: never hand over again.
    let handed_over = std::env::args().any(|a| a == "--handed-over");

    let mut loaded = Config::read();
    let port = match &loaded {
        Ok(Some(config)) => config.port,
        _ => config::DEFAULT_PORT,
    };
    let url = format!("http://127.0.0.1:{port}/");

    // The port doubles as a single-instance lock: if it is taken, another
    // copy is running, so just show its UI. This happens before logging is
    // set up so a second launch doesn't rotate the running copy's log.
    let bind = || std::net::TcpListener::bind(("127.0.0.1", port));
    let listener = match bind().or_else(|e| {
        if replacing {
            // The old copy saves and stops its audio first.
            wait_for_port(port, 30)
        } else {
            Err(e)
        }
    }) {
        Ok(l) => l,
        Err(e) if replacing => {
            init_logging(verbose);
            tracing::error!("the previous copy didn't let go of port {port} ({e}); not starting");
            return Ok(());
        }
        Err(_) if background => return Ok(()),
        Err(_) => match running_version(port) {
            // Another build (an older one starting at sign-in, or a test
            // build) would otherwise keep running and hide this one.
            Some(running)
                if running != build()
                    && platform::ask(&format!(
                        "Virtual Audio Mixer {running} is running. \
                         Close it and start this one ({})?",
                        build()
                    )) =>
            {
                match take_over(port) {
                    Ok(listener) => listener,
                    Err(e) => {
                        platform::tell(&format!(
                            "Couldn't close the running Virtual Audio Mixer: {e:#}. \
                             Quit it from its tray icon and open this one again."
                        ));
                        return Ok(());
                    }
                }
            }
            _ => {
                platform::shell_open(&url);
                return Ok(());
            }
        },
    };
    if replacing {
        // As the previous copy saved it on its way out.
        loaded = Config::read();
    }

    // A downloaded update is installed before logging starts, so the
    // previous run's log isn't rotated away by both this copy and the new.
    let update_failed = match update::install_on_start(background) {
        Some(Ok(())) => return Ok(()),
        Some(Err(e)) => Some(e),
        None => None,
    };
    init_logging(verbose);
    tracing::info!(
        "Virtual Audio Mixer {} starting, args: {:?}",
        build(),
        std::env::args().skip(1).collect::<Vec<_>>()
    );
    if let Some(e) = update_failed {
        tracing::error!("installing the update: {e:#}");
    }
    // Start with Windows runs an installed copy. Opening another one (such
    // as the download) brings it up to date and starts it instead.
    let admin = matches!(&loaded, Ok(Some(config)) if config.autostart_admin);
    let listener = match (!handed_over)
        .then(|| installed_to_run(admin, background))
        .flatten()
    {
        None => {
            if !platform::autostart_enabled() {
                platform::remove_user_copy();
            }
            listener
        }
        Some(copy) => {
            drop(listener);
            let started = platform::start_installed(copy).and_then(|()| {
                anyhow::ensure!(wait_for_mixer(port, 15), "it didn't start");
                Ok(())
            });
            match started {
                Ok(()) => {
                    tracing::info!("started the installed copy ({copy:?}) in place of this one");
                    update::clean_up_now();
                    if !background {
                        platform::shell_open(&url);
                    }
                    return Ok(());
                }
                Err(e) => {
                    tracing::error!("starting the installed copy ({copy:?}): {e:#}");
                    match wait_for_port(port, 5) {
                        Ok(listener) => listener,
                        // It started after all.
                        Err(_) => return Ok(()),
                    }
                }
            }
        }
    };
    update::clean_up();
    listener.set_nonblocking(true)?;
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
    engine.updater.start();
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
        if let Err(e) = tray::run(url, move || quit(), engine.updater.clone()) {
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
    // Start with Windows was just turned on: the installed copy takes over
    // now that this one has let go of the port.
    if let Some(copy) = platform::take_hand_over() {
        tracing::info!("starting the installed copy ({copy:?})");
        let started = platform::start_installed(copy).and_then(|()| {
            anyhow::ensure!(wait_for_mixer(port, 15), "it didn't start");
            Ok(())
        });
        if let Err(e) = started {
            tracing::error!("starting the installed copy: {e:#}; starting this one again");
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).arg("--background").spawn();
            }
        }
    }
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
            hear: false,
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
        assert!(is_older("0.2.1-dev.010e2aa", "0.2.1"), "test builds");
    }

    #[test]
    fn the_build_opened_last_replaces_an_installed_copy() {
        // A release: only an older version, or a test build.
        assert!(replaces_as("0.3.0", true, "0.2.1"));
        assert!(replaces_as("0.3.0", true, "0.3.0-dev.abc1234"));
        assert!(replaces_as("0.3.0", true, "unknown"));
        assert!(!replaces_as("0.3.0", true, "0.3.0"));
        assert!(!replaces_as("0.3.0", true, "0.4.0"), "never a downgrade");
        // A test build: anything but itself.
        let dev = "0.3.0-dev.abc1234";
        assert!(replaces_as(dev, false, "0.3.0"));
        assert!(replaces_as(dev, false, "0.4.0"));
        assert!(replaces_as(dev, false, "0.3.0-dev.def5678"));
        assert!(!replaces_as(dev, false, dev));
    }

    #[test]
    fn the_running_copy_hears_the_request_to_quit() {
        // A server like the mixer's, which drops a request whose
        // connection closes before it's answered.
        let quit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let heard = quit.clone();
        let router = axum::Router::new().route(
            "/api/quit",
            axum::routing::post(move || async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                heard.store(true, std::sync::atomic::Ordering::SeqCst);
                axum::http::StatusCode::NO_CONTENT
            }),
        );
        runtime.spawn(async move { axum::serve(listener, router).await });
        ask_to_quit(port).unwrap();
        assert!(quit.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn waits_for_the_port_to_come_free() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(wait_for_port(port, 0).is_err(), "taken");
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        assert!(wait_for_port(port, 5).is_ok(), "taken over once it's free");
        letting_go.join().unwrap();
    }
}
