//! Local web UI + JSON API on 127.0.0.1.

use crate::backend::PLATFORM_SUPPORTED;
use crate::config::{DeviceRef, Hotkey, SourceKind, Theme};
use crate::engine::{ChannelPatch, Engine, MusicControl, MusicPatch, SoundPatch, SoundboardPatch};
use crate::platform;
use crate::soundboard::MAX_FILE_BYTES;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
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
        .route("/", get(index))
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
        .route("/api/sounds/order", put(reorder_sounds))
        .route("/api/soundboard", patch(update_soundboard))
        .route("/api/music", patch(update_music))
        .route("/api/music/items", post(add_music).delete(clear_music))
        .route("/api/music/items/{id}", delete(remove_music))
        .route("/api/music/control", post(music_control))
        .route("/api/music/tracks/remove", post(remove_track))
        .route("/api/music/tracks/restore", post(restore_tracks))
        .route("/api/music/browse", post(browse_music))
        .route("/api/sounds/{id}", patch(update_sound).delete(remove_sound))
        .route("/api/sounds/{id}/play", post(play_sound))
        .route("/api/sounds/{id}/preview", post(preview_sound))
        .route("/api/sounds/{id}/waveform", get(sound_waveform))
        .route("/api/quit", post(quit_app))
        .route("/api/update/check", post(check_for_update))
        .route("/api/update/download", post(download_update))
        .route("/api/update/install", post(install_update))
        .route("/ws", get(ws))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// The page, in the chosen theme from the first paint (a script setting it
/// later would flash the other one).
async fn index(State(app): State<App>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        themed_page(app.engine.theme()),
    )
        .into_response()
}

fn themed_page(theme: Theme) -> std::borrow::Cow<'static, str> {
    let page = include_str!("../web/index.html");
    let theme = match theme {
        Theme::System => return page.into(),
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    page.replacen(
        "<html lang=\"en\">",
        &format!("<html lang=\"en\" data-theme=\"{theme}\">"),
        1,
    )
    .into()
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
    let admin = engine.autostart_admin();
    v["autostart"] = json!(admin || platform::autostart_enabled());
    v["autostart_admin"] = json!(admin);
    v["elevated"] = json!(platform::is_elevated());
    v["platform_supported"] = json!(PLATFORM_SUPPORTED);
    v["theme"] = json!(engine.theme());
    v["version"] = json!(crate::build());
    v["update"] = json!(engine.updater.view());
    v["updates"] = json!(engine.update_mode());
    let copy = platform::admin_copy_version();
    v["admin_copy_outdated"] = json!(admin_copy_outdated(copy.as_deref()));
    v["admin_copy_version"] = json!(copy);
    v
}

/// Whether the copy in Program Files is older than this one, so starting
/// as administrator would run an old version. Never offers a downgrade.
fn admin_copy_outdated(copy: Option<&str>) -> bool {
    copy.is_some_and(crate::replaces)
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
    // A new trim measures the sound's loudness again, which takes a moment.
    blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.update_sound(&id, body)
    })
    .await?;
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

async fn reorder_sounds(State(app): State<App>, Json(body): Json<Order>) -> ApiResult {
    app.engine.reorder_sounds(&body.ids).map_err(bad)?;
    ok()
}

async fn preview_sound(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.engine.preview_sound(&id).map_err(bad)?;
    ok()
}

async fn sound_waveform(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    // Scans the whole decoded sound; keep it off the executor.
    let waveform = blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.sound_waveform(&id)
    })
    .await?;
    Ok(Json(waveform).into_response())
}

#[derive(Deserialize)]
struct MusicPath {
    path: String,
}

/// Scanning a folder can take a while, so these run off the executor.
async fn add_music(State(app): State<App>, Json(body): Json<MusicPath>) -> ApiResult {
    let id = blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.add_music(&body.path)
    })
    .await?;
    Ok(Json(json!({ "id": id })).into_response())
}

async fn remove_music(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    blocking(&app, StatusCode::BAD_REQUEST, move |e| e.remove_music(&id)).await?;
    ok()
}

#[derive(Deserialize)]
struct TrackRef {
    /// Index in the playlist, and its name as the UI showed it.
    track: usize,
    name: String,
}

async fn clear_music(State(app): State<App>) -> ApiResult {
    blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.clear_music();
        Ok(())
    })
    .await?;
    ok()
}

async fn remove_track(State(app): State<App>, Json(body): Json<TrackRef>) -> ApiResult {
    blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.remove_track(body.track, &body.name)
    })
    .await?;
    ok()
}

async fn restore_tracks(State(app): State<App>) -> ApiResult {
    blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        e.restore_tracks();
        Ok(())
    })
    .await?;
    ok()
}

async fn music_control(State(app): State<App>, Json(body): Json<MusicControl>) -> ApiResult {
    app.engine.music_control(body).map_err(bad)?;
    ok()
}

async fn update_music(State(app): State<App>, Json(body): Json<MusicPatch>) -> ApiResult {
    // Switching Hear music on queries the default output, which can block.
    blocking(&app, StatusCode::BAD_REQUEST, move |e| e.update_music(body)).await?;
    ok()
}

#[derive(Deserialize)]
struct Browse {
    folders: bool,
}

/// Shows the Windows file picker on the desktop and returns the chosen
/// paths (it waits until the user closes it).
async fn browse_music(State(app): State<App>, Json(body): Json<Browse>) -> ApiResult {
    let paths = blocking(&app, StatusCode::BAD_REQUEST, move |_| {
        platform::browse(body.folders)
    })
    .await?;
    Ok(Json(json!({ "paths": paths })).into_response())
}

async fn stop_sounds(State(app): State<App>) -> ApiResult {
    app.engine.stop_sounds();
    ok()
}

#[derive(Deserialize)]
struct Settings {
    autostart: Option<bool>,
    /// Start with Windows as administrator (the elevated sign-in task).
    autostart_admin: Option<bool>,
    theme: Option<Theme>,
    /// How updates are looked for.
    updates: Option<crate::config::UpdateMode>,
}

async fn set_settings(State(app): State<App>, Json(body): Json<Settings>) -> ApiResult {
    let autostart = body.autostart.is_some() || body.autostart_admin.is_some();
    // Changing the elevated task waits for the user to answer a UAC prompt.
    let hand_over = blocking(&app, StatusCode::BAD_REQUEST, move |e| {
        if !autostart {
            return Ok(None);
        }
        let admin = e.autostart_admin();
        let run_key = platform::autostart_enabled();
        let want = body.autostart.unwrap_or(admin || run_key);
        let want_admin = want && body.autostart_admin.unwrap_or(admin);
        // Asking for it again while it's on updates an outdated copy.
        let update = admin
            && body.autostart_admin == Some(true)
            && admin_copy_outdated(platform::admin_copy_version().as_deref());
        if want_admin != admin || (want_admin && update) {
            platform::set_admin_autostart(want_admin)?;
            e.set_autostart_admin(want_admin);
        }
        // The Run entry is only for the non-administrator mode. Turning it
        // on again installs this copy if it's newer than the installed one.
        let want_run_key = want && !want_admin;
        if want_run_key || run_key {
            platform::set_autostart(want_run_key)?;
        }
        let outdated = platform::admin_copy_version().is_none_or(|v| crate::replaces(&v));
        Ok(platform::hand_over_to(
            want,
            want_admin,
            platform::this_copy(),
            outdated,
            platform::is_elevated(),
        ))
    })
    .await?;
    // After the autostart change, so a cancelled UAC prompt changes nothing.
    if let Some(theme) = body.theme {
        app.engine.set_theme(theme);
    }
    if let Some(mode) = body.updates {
        app.engine.set_update_mode(mode);
    }
    // Run the installed copy now, rather than from the next sign-in.
    if let Some(copy) = hand_over {
        platform::hand_over_on_quit(copy);
        return quit_app(State(app)).await;
    }
    ok()
}

async fn check_for_update(State(app): State<App>) -> ApiResult {
    app.engine.updater.check_now();
    ok()
}

async fn download_update(State(app): State<App>) -> ApiResult {
    app.engine.updater.download_now();
    ok()
}

/// Swaps in the downloaded update and starts it; this copy then quits, and
/// the new one takes over the port.
async fn install_update(State(app): State<App>) -> ApiResult {
    blocking(&app, StatusCode::INTERNAL_SERVER_ERROR, |e| {
        e.updater.install_and_launch()
    })
    .await?;
    quit_app(State(app)).await
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
                "music_position_ms": engine.music_position_ms(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_copy_this_build_replaces_is_outdated() {
        assert!(!admin_copy_outdated(None));
        assert!(!admin_copy_outdated(Some(crate::build())));
        assert!(admin_copy_outdated(Some("unknown")));
        assert!(admin_copy_outdated(Some("0.0.1")));
    }

    #[test]
    fn the_page_comes_in_the_chosen_theme() {
        assert!(themed_page(Theme::System).contains("<html lang=\"en\">"));
        assert!(themed_page(Theme::Dark).contains("<html lang=\"en\" data-theme=\"dark\">"));
        assert!(themed_page(Theme::Light).contains("data-theme=\"light\""));
    }
}
