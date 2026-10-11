//! Notification-area icon and the main thread's Win32 message loop.

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage, WM_QUIT,
};

/// Returns a function that ends [`run`] from any thread.
pub fn quit_handle() -> impl Fn() + Send + Sync + 'static {
    let thread = unsafe { GetCurrentThreadId() };
    move || unsafe {
        let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
    }
}

/// Shows the tray icon and pumps messages until WM_QUIT. Must run on the
/// thread that called [`quit_handle`].
pub fn run(url: String, quit: impl Fn() + Send + Sync + 'static) -> anyhow::Result<()> {
    let open = MenuItem::new("Open mixer", true, None);
    let exit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &exit])?;
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Virtual Audio Mixer")
        .with_icon(icon()?)
        .build()?;

    let (open_id, exit_id) = (open.id().clone(), exit.id().clone());
    let menu_url = url.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            crate::platform::open_browser(&menu_url);
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
            crate::platform::open_browser(&url);
        }
    }));

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
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
