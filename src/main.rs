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
    init_logging();

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
                platform::open_browser(&url);
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
            platform::open_browser(&url);
        }
        tray::run(url, move || quit())?;
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

fn init_logging() {
    let filter = tracing_subscriber::filter::LevelFilter::INFO;
    if cfg!(all(windows, not(debug_assertions))) {
        let dir = config::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(file) = std::fs::File::create(dir.join("log.txt")) {
            tracing_subscriber::fmt()
                .with_max_level(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
            return;
        }
    }
    tracing_subscriber::fmt().with_max_level(filter).init();
}
