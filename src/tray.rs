//! Notification-area icon and the main thread's Win32 message loop.

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, SetTimer, TranslateMessage, WM_QUIT,
    WM_TIMER,
};

/// Returns a function that ends [`run`] from any thread.
pub fn quit_handle() -> impl Fn() + Send + Sync + 'static {
    let thread = unsafe { GetCurrentThreadId() };
    move || unsafe {
        let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
    }
}

/// How often the menu catches up with updates.
const REFRESH_MS: u32 = 30_000;

/// Shows the tray icon and pumps messages until WM_QUIT. Must run on the
/// thread that called [`quit_handle`].
pub fn run(
    url: String,
    quit: impl Fn() + Send + Sync + 'static,
    updater: std::sync::Arc<crate::update::Updater>,
) -> anyhow::Result<()> {
    let title = MenuItem::new(
        format!("Virtual Audio Mixer {}", crate::build()),
        false,
        None,
    );
    // Shown under the title while an update is ready.
    let update = MenuItem::new("", true, None);
    let open = MenuItem::new("Open mixer", true, None);
    let log = MenuItem::new("Open log file", true, None);
    let exit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &title,
        &PredefinedMenuItem::separator(),
        &open,
        &log,
        &PredefinedMenuItem::separator(),
        &exit,
    ])?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_menu_on_left_click(false)
        .with_tooltip("Virtual Audio Mixer")
        .with_icon(icon()?)
        .build()?;

    let (open_id, log_id, exit_id) = (open.id().clone(), log.id().clone(), exit.id().clone());
    let update_id = update.id().clone();
    let menu_url = url.clone();
    let installer = updater.clone();
    let quit = std::sync::Arc::new(quit);
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == update_id {
            // Writing a big exe (with antivirus looking at it) shouldn't
            // freeze the tray.
            let (installer, quit, url) = (installer.clone(), quit.clone(), menu_url.clone());
            std::thread::spawn(move || match installer.install_and_launch() {
                Ok(()) => quit(),
                // The mixer's page says what went wrong.
                Err(_) => crate::platform::shell_open(&url),
            });
        } else if event.id == open_id {
            crate::platform::shell_open(&menu_url);
        } else if event.id == log_id {
            crate::platform::shell_open(&crate::log_path().display().to_string());
        } else if event.id == exit_id {
            quit();
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            crate::platform::shell_open(&url);
        }
    }));

    tracing::info!("tray icon ready");
    // Whether the update item is in the menu.
    let mut offering = false;
    let mut refresh = || {
        let view = updater.view();
        let ready = view.state == "ready";
        if ready {
            let version = view.version.unwrap_or_default();
            update.set_text(format!("Restart to update to {version}"));
        }
        if ready != offering {
            let changed = if ready {
                menu.insert(&update, 1)
            } else {
                menu.remove(&update)
            };
            offering = ready && changed.is_ok();
            let tip = if offering {
                "Virtual Audio Mixer: an update is ready"
            } else {
                "Virtual Audio Mixer"
            };
            let _ = tray.set_tooltip(Some(tip));
        }
    };
    refresh();
    let mut msg = MSG::default();
    unsafe {
        // A thread timer: WM_TIMER arrives in this loop.
        SetTimer(None, 0, REFRESH_MS, None);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER && msg.hwnd.is_invalid() {
                refresh();
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// A small mixer-fader glyph drawn at runtime, so no image assets are needed.
fn icon() -> anyhow::Result<Icon> {
    const N: u32 = 32;
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    let accent = [0x5b, 0x8c, 0xff, 0xff];
    let white = [0xff, 0xff, 0xff, 0xff];
    for y in 0..N {
        for x in 0..N {
            let i = ((y * N + x) * 4) as usize;
            // Rounded square background.
            let (dx, dy) = (
                (x as f32 - 15.5).abs() - 11.0,
                (y as f32 - 15.5).abs() - 11.0,
            );
            let outside = dx.max(0.0).hypot(dy.max(0.0)) - 4.5;
            if outside > 0.5 {
                continue;
            }
            let mut px = accent;
            // Three fader tracks with knobs at different heights.
            for (col, knob) in [(9u32, 19u32), (16, 10), (23, 15)] {
                let on_track = x == col && (7..=25).contains(&y);
                let on_knob = x.abs_diff(col) <= 3 && y.abs_diff(knob) <= 1;
                if on_track || on_knob {
                    px = white;
                }
            }
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    Ok(Icon::from_rgba(rgba, N, N)?)
}
