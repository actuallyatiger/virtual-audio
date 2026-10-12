//! Local web UI + JSON API on 127.0.0.1.

use crate::backend::PLATFORM_SUPPORTED;
use crate::config::{DeviceRef, Hotkey, SourceKind};
use crate::engine::{ChannelPatch, Engine, SoundPatch, SoundboardPatch};
use crate::platform;
use crate::soundboard::MAX_FILE_BYTES;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct App {
    engine: Arc<Engine>,
    port: u16,
    quit: Arc<dyn Fn() + Send + Sync>,
}

type ApiResult = Result<Response, (StatusCode, String)>;

fn bad(e: impl std::fmt::Display) -> (StatusCode, String) {
    // `{:#}` includes anyhow's cause chain.
    (StatusCode::BAD_REQUEST, format!("{e:#}"))
}

fn ok() -> ApiResult {
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub fn router(engine: Arc<Engine>, port: u16, quit: Arc<dyn Fn() + Send + Sync>) -> Router {
    let app = App { engine, port, quit };
    Router::new()
        .route(
            "/",
            get(|| async {
                static_file(
                    "text/html; charset=utf-8",
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                static_file(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                static_file("text/css; charset=utf-8", include_str!("../web/style.css"))
            }),
        )
        .route(
            "/favicon.svg",
            get(|| async { static_file("image/svg+xml", include_str!("../web/favicon.svg")) }),
        )
        .route("/api/state", get(state))
        .route("/api/devices", get(devices))
        .route("/api/apps", get(apps))
        .route("/api/sources", post(add_source))
        .route("/api/sources/order", put(reorder_sources))
        .route(
            "/api/channels/{id}",
            patch(update_channel).delete(remove_source),
        )
        .route("/api/output", put(set_output))
        .route("/api/hotkeys", put(set_hotkeys))
        .route("/api/settings", put(set_settings))
        .route(
            "/api/sounds",
            // Room for the file plus a little slack, so an oversized file
            // gets the engine's friendlier error.
            post(add_sound).layer(DefaultBodyLimit::max(MAX_FILE_BYTES + 1024)),
        )
        .route("/api/sounds/stop", post(stop_sounds))
        .route("/api/soundboard", patch(update_soundboard))
        .route("/api/sounds/{id}", patch(update_sound).delete(remove_sound))
        .route("/api/sounds/{id}/play", post(play_sound))
        .route("/api/quit", post(quit_app))
        .route("/ws", get(ws))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

fn static_file(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// Only this machine's browser may talk to us: rejects DNS-rebinding (Host)
/// and cross-site requests from other web pages (Origin), and forbids other
/// sites from framing the UI (clickjacking it into adding sources, etc.).
async fn guard(State(app): State<App>, headers: HeaderMap, req: Request, next: Next) -> Response {
    let allowed = [
        format!("127.0.0.1:{}", app.port),
        format!("localhost:{}", app.port),
    ];
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| allowed.iter().any(|a| a == h));
    let origin_ok = match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        None => true,
        Some(origin) => allowed.iter().any(|a| origin == format!("http://{a}")),
    };
    let mut response = if host_ok && origin_ok {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "forbidden").into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn full_state(engine: &Engine) -> serde_json::Value {
    let mut v = serde_json::to_value(engine.state()).unwrap();
    v["autostart"] = json!(platform::autostart_enabled());
    v["platform_supported"] = json!(PLATFORM_SUPPORTED);
    v
}

async fn state(State(app): State<App>) -> Json<serde_json::Value> {
    Json(full_state(&app.engine))
}

/// Runs blocking engine/backend work off the async executor. Errors from
/// `f` map to `status` (400 for invalid input, 500 for backend failures).
async fn blocking<T: Send + 'static>(
    app: &App,
    status: StatusCode,
    f: impl FnOnce(&Engine) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, (StatusCode, String)> {
    let engine = app.engine.clone();
    tokio::task::spawn_blocking(move || f(&engine))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (status, format!("{e:#}")))
}

async fn devices(State(app): State<App>) -> ApiResult {
    let (inputs, outputs) = blocking(&app, StatusCode::INTERNAL_SERVER_ERROR, |e| {
        Ok((e.backend().input_devices()?, e.backend().output_devices()?))
    })
    .await?;
    Ok(Json(json!({ "inputs": inputs, "outputs": outputs })).into_response())
}

async fn apps(State(app): State<App>) -> ApiResult {
    let apps = blocking(&app, StatusCode::INTERNAL_SERVER_ERROR, |e| {
        e.backend().audio_apps()
    })
    .await?;
    Ok(Json(apps).into_response())
}

#[derive(Deserialize)]
struct NewSource {
    name: Option<String>,
    #[serde(flatten)]
    kind: SourceKind,
}

async fn add_source(State(app): State<App>, Json(body): Json<NewSource>) -> ApiResult {
    // add_source only fails on invalid input.
    let id = blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.add_source(body.name, body.kind)
    })
    .await?;
    Ok(Json(json!({ "id": id })).into_response())
}

async fn update_channel(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<ChannelPatch>,
) -> ApiResult {
    app.engine.update_channel(&id, body).map_err(bad)?;
    ok()
}

async fn remove_source(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.engine.remove_source(&id).map_err(bad)?;
    ok()
}

#[derive(Deserialize)]
struct Order {
    ids: Vec<String>,
}

async fn reorder_sources(State(app): State<App>, Json(body): Json<Order>) -> ApiResult {
    app.engine.reorder_sources(&body.ids).map_err(bad)?;
    ok()
}

#[derive(Deserialize)]
struct OutputBody {
    device: Option<DeviceRef>,
}

async fn set_output(State(app): State<App>, Json(body): Json<OutputBody>) -> ApiResult {
    app.engine.set_output(body.device);
    ok()
}

async fn set_hotkeys(State(app): State<App>, Json(body): Json<Vec<Hotkey>>) -> ApiResult {
    app.engine.set_hotkeys(body).map_err(bad)?;
    ok()
}

#[derive(Deserialize)]
struct Upload {
    name: Option<String>,
    /// The file's original name, for its extension and the default name.
    #[serde(default)]
    filename: String,
}

/// The request body is the raw file.
async fn add_sound(State(app): State<App>, Query(upload): Query<Upload>, body: Bytes) -> ApiResult {
    let id = blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.add_sound(upload.name, &upload.filename, body.to_vec())
    })
    .await?;
    Ok(Json(json!({ "id": id })).into_response())
}

async fn update_sound(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<SoundPatch>,
) -> ApiResult {
    app.engine.update_sound(&id, body).map_err(bad)?;
    ok()
}

async fn remove_sound(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.engine.remove_sound(&id).map_err(bad)?;
    ok()
}

async fn play_sound(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.engine.play_sound(&id).map_err(bad)?;
    ok()
}

async fn update_soundboard(State(app): State<App>, Json(body): Json<SoundboardPatch>) -> ApiResult {
    // Switching Hear sounds on queries the default output, which can block.
    blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.update_soundboard(body)
    })
    .await?;
    ok()
}

async fn stop_sounds(State(app): State<App>) -> ApiResult {
    app.engine.stop_sounds();
    ok()
}

#[derive(Deserialize)]
struct Settings {
    autostart: Option<bool>,
}

async fn set_settings(Json(body): Json<Settings>) -> ApiResult {
    if let Some(enabled) = body.autostart {
        platform::set_autostart(enabled).map_err(bad)?;
    }
    ok()
}

async fn quit_app(State(app): State<App>) -> ApiResult {
    let quit = app.quit.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        quit();
    });
    ok()
}

async fn ws(State(app): State<App>, upgrade: WebSocketUpgrade) -> Response {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
    let connection = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    upgrade.on_upgrade(move |socket| async move {
        stream_updates(socket, app.engine.clone(), connection).await;
        // However the connection ended (tab closed, refreshed, crashed),
        // stop anything it switched on.
        let engine = app.engine.clone();
        let _ = tokio::task::spawn_blocking(move || engine.set_listening(connection, false)).await;
    })
}

/// Messages the page sends over the WebSocket.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Listen { on: bool },
}

/// Pushes meters at ~30 fps and the full state whenever it changes.
async fn stream_updates(mut socket: WebSocket, engine: Arc<Engine>, connection: u64) {
    let mut changes = engine.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if send(
        &mut socket,
        json!({ "type": "state", "state": full_state(&engine) }),
    )
    .await
    .is_err()
    {
        return;
    }
    loop {
        let msg = tokio::select! {
            _ = tick.tick() => json!({
                "type": "meters",
                "meters": engine.meters(),
                "playing": engine.playing_sounds(),
            }),
            changed = changes.changed() => {
                if changed.is_err() { return; }
                changes.borrow_and_update();
                json!({ "type": "state", "state": full_state(&engine) })
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Listen { on }) => {
                            // Starting it queries devices, which can block. Awaited
                            // so one connection's toggles apply in order.
                            let engine = engine.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                engine.set_listening(connection, on)
                            })
                            .await;
                        }
                        Err(e) => tracing::debug!("ignoring websocket message {text:?}: {e}"),
                    }
                    continue;
                }
                Some(Ok(_)) => continue,
                _ => return,
            },
        };
        if send(&mut socket, msg).await.is_err() {
            return;
        }
    }
}

async fn send(socket: &mut WebSocket, value: serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}
