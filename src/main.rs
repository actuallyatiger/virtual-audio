// Release builds on Windows run without a console window; use the tray icon.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod backend;
mod config;
mod engine;
mod hotkeys;
mod mixer;
mod platform;
mod server;
#[cfg(windows)]
mod tray;

use config::{Channel, Config, DeviceRef, SourceConfig, SourceKind};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let background = std::env::args().any(|a| a == "--background");
    let verbose = std::env::args().any(|a| a == "--verbose");
    init_logging(verbose);
    tracing::info!(
        "Virtual Audio Mixer {} starting, args: {:?}",
        env!("CARGO_PKG_VERSION"),
        std::env::args().skip(1).collect::<Vec<_>>()
    );

    let first_run = !config::data_dir().join("config.json").exists();
    let mut config = Config::load();
    let port = config.port;
    let url = format!("http://127.0.0.1:{port}/");

    // The port doubles as a single-instance lock: if it is taken, another
    // copy is running, so just show its UI.
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            tracing::info!("port {port} busy ({e}), assuming already running");
            if !background {
                platform::shell_open(&url);
            }
            return Ok(());
        }
    };
    listener.set_nonblocking(true)?;

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
    use tracing_subscriber::filter::LevelFilter;
    let filter = if verbose {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };
    let builder = tracing_subscriber::fmt()
        .with_max_level(filter)
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
    match file {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => builder.init(),
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!("panic in thread {:?}: {info}", thread.name().unwrap_or("?"));
        default_hook(info);
    }));
}
