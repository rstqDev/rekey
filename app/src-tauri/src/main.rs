// Rekey runs as a menu bar / tray app; on Windows this stops a console window
// from appearing behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Rekey — fixes text typed on the wrong keyboard layout.

mod service;
mod settings;

use rekey_core::config::{Config, Sensitivity};
use rekey_core::engine::Engine;
use rekey_core::model::Models;
use service::SharedEngine;
use settings::Settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};

/// Everything the UI can read or change.
struct AppState {
    engine: SharedEngine,
    settings: Mutex<Settings>,
    /// True once the keyboard hook is actually installed. It flips on by
    /// itself when the user grants Accessibility, with no relaunch.
    hook_running: Arc<AtomicBool>,
}

/// A snapshot for the settings window.
#[derive(serde::Serialize)]
struct UiState {
    enabled: bool,
    layouts: Vec<String>,
    sensitivity: String,
    skip_all_caps: bool,
    ai_assist: bool,
    has_api_key: bool,
    launch_at_login: bool,
    excluded_apps: Vec<String>,
    exceptions: Vec<String>,
    corrections: u64,
    undos: u64,
    has_permission: bool,
    hook_running: bool,
    available_layouts: Vec<LayoutInfo>,
    models_loaded: Vec<String>,
    version: String,
}

#[derive(serde::Serialize)]
struct LayoutInfo {
    id: String,
    name: String,
    language: String,
    /// Latin-script layouts share an alphabet with US QWERTY, so there is far
    /// less signal to work with. The UI says so rather than quietly
    /// underperforming.
    limited: bool,
}

fn layout_catalog() -> Vec<LayoutInfo> {
    rekey_core::layout::LAYOUTS
        .iter()
        .map(|d| LayoutInfo {
            id: d.id.to_string(),
            name: d.name.to_string(),
            language: d.lang.to_string(),
            limited: d.latin_overlap && d.id != "us",
        })
        .collect()
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> UiState {
    let engine = state.engine.lock().expect("engine lock");
    let settings = state.settings.lock().expect("settings lock");
    let config = engine.config();
    let stats = engine.stats();

    UiState {
        enabled: config.enabled,
        layouts: config.layouts.clone(),
        sensitivity: match config.sensitivity {
            Sensitivity::Cautious => "cautious",
            Sensitivity::Balanced => "balanced",
            Sensitivity::Eager => "eager",
        }
        .into(),
        skip_all_caps: config.skip_all_caps,
        ai_assist: config.ai_assist,
        has_api_key: !settings.api_key.trim().is_empty(),
        launch_at_login: settings.launch_at_login,
        excluded_apps: config.excluded_apps.clone(),
        exceptions: config.exceptions.clone(),
        corrections: stats.corrections,
        undos: stats.undos,
        has_permission: rekey_hook::platform::has_permission(),
        hook_running: state.hook_running.load(Ordering::Relaxed),
        available_layouts: layout_catalog(),
        models_loaded: {
            let mut langs: Vec<String> = engine
                .detector
                .models
                .langs()
                .iter()
                .map(|s| s.to_string())
                .collect();
            langs.sort();
            langs
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Settings changed in the UI. The whole config is replaced at once, which
/// keeps the UI and the engine from drifting apart field by field.
#[tauri::command]
fn set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: Config,
    api_key: Option<String>,
    launch_at_login: Option<bool>,
) -> Result<(), String> {
    {
        let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
        engine.set_config(config.clone());
    }
    let mut settings = state.settings.lock().map_err(|e| e.to_string())?;
    settings.config = config;
    if let Some(key) = api_key {
        settings.api_key = key;
    }
    if let Some(launch) = launch_at_login {
        settings.launch_at_login = launch;
        apply_launch_at_login(&app, launch);
    }
    settings.save().map_err(|e| e.to_string())
}

fn apply_launch_at_login(app: &AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = result {
        log::warn!("cannot change launch-at-login: {e}");
    }
}

/// Ask macOS for Accessibility access. The grant happens in System Settings and
/// only takes effect after a relaunch, which the UI explains.
#[tauri::command]
fn request_permission() -> bool {
    rekey_hook::platform::request_permission()
}

#[tauri::command]
fn open_accessibility_settings(app: AppHandle) {
    use tauri_plugin_opener::OpenerExt;
    let _ = app.opener().open_url(
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        None::<&str>,
    );
}

/// Try a word against the current settings without typing it anywhere.
/// Backs the "try it" box in the settings window, which is how a new user sees
/// what the app does before granting it access to their keyboard.
#[tauri::command]
fn preview(state: State<'_, AppState>, text: String, layout: String) -> PreviewResult {
    let engine = state.engine.lock().expect("engine lock");
    let mut out = Vec::new();
    let mut changed = false;
    for token in text.split_whitespace() {
        let (prefix, core, suffix) = engine.detector.trim_affixes(token, &layout);
        match engine.detector.evaluate(core, &layout) {
            rekey_core::detect::Verdict::Switch(c) => {
                out.push(format!("{prefix}{}{suffix}", c.converted));
                changed = true;
            }
            _ => out.push(token.to_string()),
        }
    }
    PreviewResult {
        output: out.join(" "),
        changed,
    }
}

#[derive(serde::Serialize)]
struct PreviewResult {
    output: String,
    changed: bool,
}

#[tauri::command]
fn undo(state: State<'_, AppState>) {
    let mut engine = state.engine.lock().expect("engine lock");
    let _ = engine.undo();
}

fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle, engine: SharedEngine) -> tauri::Result<()> {
    let enabled = engine.lock().expect("engine lock").config().enabled;

    let toggle = CheckMenuItem::with_id(app, "toggle", "Correcting", true, enabled, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let undo_item = MenuItem::with_id(app, "undo", "Undo last correction", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Rekey", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &PredefinedMenuItem::separator(app)?,
            &undo_item,
            &settings_item,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let tray_engine = engine.clone();
    TrayIconBuilder::with_id("rekey")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/trayTemplate@2x.png"
        ))?)
        // A template icon is tinted by macOS to match the menu bar, so it stays
        // legible in both light and dark appearances.
        .icon_as_template(true)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "toggle" => {
                let mut engine = tray_engine.lock().expect("engine lock");
                let mut config = engine.config().clone();
                config.enabled = !config.enabled;
                engine.set_config(config.clone());
                if let Some(state) = app.try_state::<AppState>() {
                    if let Ok(mut settings) = state.settings.lock() {
                        settings.config = config;
                        let _ = settings.save();
                    }
                }
            }
            "undo" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let mut engine = state.engine.lock().expect("engine lock");
                    let _ = engine.undo();
                }
            }
            "settings" => show_settings(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show_settings(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_config,
            request_permission,
            open_accessibility_settings,
            preview,
            undo
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let settings = Settings::load();

            let dir = settings::models_dir(&handle);
            let models = Models::load_dir(&dir).unwrap_or_else(|e| {
                log::error!("cannot load language models from {}: {e}", dir.display());
                Models::new()
            });
            if models.is_empty() {
                log::error!(
                    "no language models found in {} — corrections are disabled",
                    dir.display()
                );
            }

            let engine: SharedEngine =
                Arc::new(Mutex::new(Engine::new(models, settings.config.clone())));

            // The hook waits for Accessibility rather than failing, so this
            // only errors if the platform layer itself is unavailable.
            let hook_running = match service::spawn(engine.clone()) {
                Ok(flag) => flag,
                Err(e) => {
                    log::warn!("keyboard hook unavailable: {e}");
                    Arc::new(AtomicBool::new(false))
                }
            };

            build_tray(&handle, engine.clone())?;

            let started = hook_running.load(Ordering::Relaxed);
            app.manage(AppState {
                engine,
                settings: Mutex::new(settings),
                hook_running,
            });

            // First run, or permission still missing: show the window so the
            // user is not left with a silent menu bar icon.
            if !started || !settings::config_path().exists() {
                show_settings(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window hides it; the app lives in the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("cannot start Rekey")
        .run(|_app, event| {
            // No dock icon and no quit-on-last-window-closed: this is a menu
            // bar app that happens to have a settings window.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
