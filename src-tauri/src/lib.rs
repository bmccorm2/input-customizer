mod caps;
mod config;
mod fn_lock;
mod input_source;
mod lock_tap;

use config::Config;
use lock_tap::LockTap;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, mpsc};
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

struct AppState {
    config: Mutex<Config>,
    config_path: PathBuf,
    tap: LockTap,
}

impl AppState {
    fn config(&self) -> MutexGuard<'_, Config> {
        self.config.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn update(&self, f: impl FnOnce(&mut Config)) -> Result<(), String> {
        let mut cfg = self.config();
        f(&mut cfg);
        config::save(&self.config_path, &cfg).map_err(|e| format!("Saving config failed: {e}"))
    }
}

#[derive(Serialize)]
struct FeatureStatus {
    on: bool,
    ok: bool,
    message: String,
}

#[derive(Serialize)]
struct LockStatus {
    on: bool,
    ok: bool,
    message: String,
    trigger_label: Option<String>,
    accessibility: bool,
    learning: bool,
}

#[derive(Serialize)]
struct ScrollStatus {
    speed: f64,
    ok: bool,
    message: String,
}

#[derive(Serialize)]
struct Status {
    colemak: FeatureStatus,
    caps_backspace: FeatureStatus,
    lock_sleep: LockStatus,
    scroll: ScrollStatus,
}

const SCROLL_MIN: f64 = 0.05;
const SCROLL_MAX: f64 = 5.0;

fn feature(on: bool, active: bool, missing: &str) -> FeatureStatus {
    let (ok, message) = match (on, active) {
        (true, true) => (true, "Active".to_string()),
        (false, false) => (true, "Off".to_string()),
        (true, false) => (false, missing.to_string()),
        (false, true) => (false, "Off, but still active on the system".to_string()),
    };
    FeatureStatus { on, ok, message }
}

/// Runs `f` on the main thread (TIS is not thread-safe) and waits for the result.
fn on_main<R: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    unsafe extern "C" {
        fn pthread_main_np() -> i32;
    }
    if unsafe { pthread_main_np() } != 0 {
        return Ok(f());
    }
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())
}

fn status(app: &AppHandle) -> Result<Status, String> {
    let st = app.state::<AppState>();
    let cfg = st.config().clone();
    let current = on_main(app, input_source::current_name)?;
    let colemak_active = current.as_deref() == Some(input_source::COLEMAK_NAME);
    let colemak_missing = format!(
        "Not selected (current: {})",
        current.as_deref().unwrap_or("unknown")
    );

    let accessibility = lock_tap::accessibility_trusted();
    let (running, learning) = {
        let s = st.tap.state();
        (s.running, s.learning)
    };
    let lock_msg = if !accessibility {
        "Needs Accessibility permission"
    } else if learning {
        "Press the lock key…"
    } else if cfg.lock_trigger.is_none() {
        "Learn the lock key first"
    } else if cfg.lock_sleep && !running {
        "Event tap not running"
    } else if cfg.lock_sleep {
        "Active"
    } else {
        "Off"
    };
    let scroll_msg = if cfg.scroll_speed == 1.0 {
        "System speed"
    } else if !accessibility {
        "Needs Accessibility permission"
    } else if !running {
        "Event tap not running"
    } else {
        "Active"
    };

    Ok(Status {
        colemak: feature(cfg.colemak, colemak_active, &colemak_missing),
        caps_backspace: feature(cfg.caps_backspace, caps::status(), "Mapping not applied"),
        lock_sleep: LockStatus {
            on: cfg.lock_sleep,
            ok: matches!(lock_msg, "Active" | "Off"),
            message: lock_msg.into(),
            trigger_label: cfg.lock_trigger.as_ref().map(lock_tap::describe),
            accessibility,
            learning,
        },
        scroll: ScrollStatus {
            speed: cfg.scroll_speed,
            ok: matches!(scroll_msg, "Active" | "System speed"),
            message: scroll_msg.into(),
        },
    })
}

fn apply_colemak(app: &AppHandle, on: bool) -> Result<(), String> {
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    on_main(app, move || {
        if on {
            input_source::enable_colemak(&resource_dir)
        } else {
            input_source::select_by_id(input_source::ABC_ID)
        }
    })?
}

fn apply_lock(app: &AppHandle, on: bool) -> Result<(), String> {
    let st = app.state::<AppState>();
    if on {
        if st.config().lock_trigger.is_none() {
            return Err("Learn the lock key first".into());
        }
        st.tap.ensure_running()?;
    }
    st.tap.state().enabled = on;
    Ok(())
}

fn apply_scroll(app: &AppHandle, speed: f64) -> Result<(), String> {
    let st = app.state::<AppState>();
    if speed != 1.0 {
        st.tap.ensure_running()?;
    }
    st.tap.state().scroll_speed = speed;
    Ok(())
}

#[tauri::command]
async fn set_scroll_speed(app: AppHandle, speed: f64) -> Result<Status, String> {
    if !speed.is_finite() {
        return Err("Invalid speed".into());
    }
    let speed = speed.clamp(SCROLL_MIN, SCROLL_MAX);
    apply_scroll(&app, speed)?;
    app.state::<AppState>().update(|c| c.scroll_speed = speed)?;
    status(&app)
}

#[tauri::command]
async fn get_status(app: AppHandle) -> Result<Status, String> {
    status(&app)
}

#[tauri::command]
async fn set_colemak(app: AppHandle, on: bool) -> Result<Status, String> {
    apply_colemak(&app, on)?;
    app.state::<AppState>().update(|c| c.colemak = on)?;
    status(&app)
}

#[tauri::command]
async fn set_caps_backspace(app: AppHandle, on: bool) -> Result<Status, String> {
    if on { caps::enable() } else { caps::disable() }?;
    app.state::<AppState>().update(|c| c.caps_backspace = on)?;
    status(&app)
}

#[tauri::command]
async fn set_lock_sleep(app: AppHandle, on: bool) -> Result<Status, String> {
    apply_lock(&app, on)?;
    app.state::<AppState>().update(|c| c.lock_sleep = on)?;
    status(&app)
}

#[tauri::command]
async fn start_learn(app: AppHandle) -> Result<Status, String> {
    let st = app.state::<AppState>();
    st.tap.ensure_running()?;
    st.tap.state().learning = true;
    status(&app)
}

#[tauri::command]
async fn cancel_learn(app: AppHandle) -> Result<Status, String> {
    app.state::<AppState>().tap.state().learning = false;
    status(&app)
}

#[tauri::command]
async fn open_accessibility_settings() -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg(lock_tap::ACCESSIBILITY_URL)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn fn_lock_label(media: bool) -> (&'static str, &'static str) {
    if media {
        ("Media keys", "Hold Fn for F1–F12")
    } else {
        ("F1–F12", "Hold Fn for media keys")
    }
}

/// Briefly shows a centered, click-through overlay with the Fn-lock mode.
fn show_fn_hud(app: &AppHandle, media: bool) {
    static SHOWN: AtomicU64 = AtomicU64::new(0);
    let Some(w) = app.get_webview_window("hud") else {
        return;
    };
    let (title, sub) = fn_lock_label(media);
    let _ = w.eval(format!("showHud({title:?}, {sub:?})"));
    // Top middle of the screen with the mouse, just below the menu bar.
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    if let (Some(m), Ok(size)) = (monitor, w.outer_size()) {
        let area = m.work_area();
        let margin = (12.0 * m.scale_factor()) as i32;
        let x = area.position.x + (area.size.width as i32 - size.width as i32) / 2;
        let _ = w.set_position(tauri::PhysicalPosition::new(x, area.position.y + margin));
    }
    let _ = w.show();
    let _ = w.set_ignore_cursor_events(true);
    let id = SHOWN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        // Only hide if no newer toggle re-showed it.
        if SHOWN.load(Ordering::SeqCst) == id {
            let _ = w.hide();
        }
    });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    let handle = app.handle().clone();
    let config_dir = app.path().app_config_dir()?;
    let config_path = config_dir.join("config.json");
    let cfg = config::load(&config_path);

    // Launch at login: enable once (release builds only, so `tauri dev` doesn't register itself).
    let autostart_marker = config_dir.join("autostart-initialized");
    if !cfg!(debug_assertions) {
        let autolaunch = app.autolaunch();
        if !autostart_marker.exists() {
            autolaunch.enable()?;
            std::fs::create_dir_all(&config_dir)?;
            std::fs::write(&autostart_marker, b"")?;
        } else if autolaunch.is_enabled().unwrap_or(false) {
            // Re-register so the login item points at this copy of the app (e.g. after a move).
            autolaunch.enable()?;
        }
    }

    let learned_handle = handle.clone();
    let tap = LockTap::new(move |trigger| {
        let st = learned_handle.state::<AppState>();
        if let Err(e) = st.update(|c| c.lock_trigger = Some(trigger.clone())) {
            eprintln!("{e}");
        }
        let _ = learned_handle.emit("lock-learned", lock_tap::describe(&trigger));
    });
    tap.state().trigger = cfg.lock_trigger.clone();
    let scroll_speed = cfg.scroll_speed.clamp(SCROLL_MIN, SCROLL_MAX);
    tap.state().scroll_speed = scroll_speed;
    let first_run = !config_path.exists();
    app.manage(AppState {
        config: Mutex::new(cfg.clone()),
        config_path,
        tap,
    });
    if first_run {
        app.state::<AppState>().update(|_| {})?;
    }

    // Apply saved ON settings only.
    if cfg.colemak
        && let Err(e) = apply_colemak(&handle, true)
    {
        eprintln!("colemak: {e}");
    }
    if cfg.caps_backspace {
        std::thread::spawn(|| {
            if let Err(e) = caps::enable() {
                eprintln!("caps: {e}");
            }
        });
    }
    // Don't prompt for Accessibility at launch; the UI shows the missing permission instead.
    if cfg.lock_sleep
        && lock_tap::accessibility_trusted()
        && let Err(e) = apply_lock(&handle, true)
    {
        eprintln!("lock: {e}");
    }
    if scroll_speed != 1.0
        && lock_tap::accessibility_trusted()
        && let Err(e) = apply_scroll(&handle, scroll_speed)
    {
        eprintln!("scroll: {e}");
    }

    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let fn_mode = MenuItem::with_id(app, "fn-mode", "Top row: unknown", false, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .items(&[&fn_mode])
        .separator()
        .items(&[&settings, &quit])
        .build()?;

    WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("hud.html".into()))
        .inner_size(260.0, 110.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .focusable(false)
        .focused(false)
        .visible(false)
        .build()?;
    let fn_handle = handle.clone();
    fn_lock::start(move |media, toggled| {
        let _ = fn_mode.set_text(format!("Top row: {}", fn_lock_label(media).0));
        if toggled {
            show_fn_hud(&fn_handle, media);
        }
    });

    TrayIconBuilder::with_id("main")
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        .tooltip("Keyboard Customizer")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => show_settings(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Quitting restores the stock keyboard. Config is untouched, so the next launch re-applies.
fn revert_for_quit(app: &AppHandle) {
    let cfg = app.state::<AppState>().config().clone();
    if cfg.colemak
        && let Err(e) = apply_colemak(app, false)
    {
        eprintln!("colemak: {e}");
    }
    if cfg.caps_backspace
        && let Err(e) = caps::disable()
    {
        eprintln!("caps: {e}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(setup)
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_colemak,
            set_caps_backspace,
            set_lock_sleep,
            set_scroll_speed,
            start_learn,
            cancel_learn,
            open_accessibility_settings
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Every normal quit (tray Quit, Cmd+Q, logout) ends here.
            if let tauri::RunEvent::Exit = event {
                revert_for_quit(app);
            }
        });
}
