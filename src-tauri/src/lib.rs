//! Sense Lite 应用入口:托盘菜单、窗口行为、Notify 事件转发。

mod agent;
mod commands;
mod manager;
mod power_events;
mod protocol;

pub use agent::AgentClient;
pub use protocol::Service;

use serde_json::json;
use tauri::{
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter as _, Manager as _, WebviewWindow,
};
use tauri_plugin_opener::init as opener_plugin;

use crate::manager::Manager;

/// 性能模式在托盘菜单里的可勾选项,存状态以便 set 后同步勾选。
struct TrayModeItems {
    items: Vec<CheckMenuItem<tauri::Wry>>,
}

const MODE_LABELS: [&str; 3] = ["静音 (Silent)", "均衡 (Normal)", "性能 (Performance)"];

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(opener_plugin())
        .setup(|app| {
            let manager = Manager::spawn_all();

            // 服务端 Notify → 前端事件
            for service in Service::ALL {
                let mut rx = manager.client(service).subscribe_notify();
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(v) => {
                                let _ = handle.emit("agent-notify", json!({
                                    "port": service.port(),
                                    "service": service.name(),
                                    "packet": v,
                                }));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                });
            }

            build_tray(app, &manager)?;
            app.manage(manager);

            // 息屏恢复后自动重放蓝光过滤:
            // 显卡颜色管线在息屏时被重置,Agent 不会自行重新应用 gamma,
            // 因此监听显示器点亮事件,把当前护眼档位重新下发一次。
            #[cfg(windows)]
            {
                let m = app.state::<Manager>().inner().clone();
                if let Ok(mut rx) = power_events::spawn_monitor_on_events() {
                    tauri::async_runtime::spawn(async move {
                        while rx.recv().await.is_some() {
                            // 等显示管线就绪,避免过早下发
                            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                            if let Ok(pkt) = m.qa.get("BluelightShield").await {
                                let v = pkt
                                    .get("Result")
                                    .and_then(|r| r.get("Value"))
                                    .and_then(|v| v.as_i64());
                                if let Some(v) = v {
                                    if v > 0 {
                                        let _ =
                                            m.qa.set("BluelightShield", json!(v)).await;
                                    }
                                }
                            }
                        }
                    });
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭按钮 → 隐藏到托盘,不退出
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_data,
            commands::set_data,
            commands::agent_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn build_tray(app: &tauri::App, manager: &Manager) -> tauri::Result<()> {
    let mut mode_items = Vec::new();
    for (i, label) in MODE_LABELS.iter().enumerate() {
        mode_items.push(CheckMenuItem::with_id(
            app,
            format!("mode-{i}"),
            label,
            true,
            false,
            None::<&str>,
        )?);
    }

    let mode_menu_items: Vec<&dyn IsMenuItem<tauri::Wry>> =
        mode_items.iter().map(|i| i as &dyn IsMenuItem<tauri::Wry>).collect();
    let mode_menu = tauri::menu::Submenu::with_id_and_items(
        app,
        "performance",
        "性能模式",
        true,
        &mode_menu_items,
    )?;
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&mode_menu, &sep, &show, &quit])?;

    // 当前性能模式回填托盘勾选
    let m = manager.clone();
    let app_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        if let Ok(packet) = m.qa.get("SystemUsageControl").await {
            if let Some(mode) = packet
                .get("Result")
                .and_then(|r| r.get("Value"))
                .and_then(|v| v.as_i64())
            {
                sync_mode_checks(&app_handle, mode);
            }
        }
    });

    app.manage(TrayModeItems { items: mode_items });

    TrayIconBuilder::with_id("sense-lite-tray")
        .icon(app.default_window_icon().expect("default icon").clone())
        .tooltip("Sense Lite")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            match id {
                "show" => show_main(app),
                "quit" => app.exit(0),
                _ if id.starts_with("mode-") => {
                    if let Ok(mode) = id.trim_start_matches("mode-").parse::<i64>() {
                        let h = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let m: tauri::State<Manager> = h.state();
                            let result = m.qa.set("SystemUsageControl", json!(mode)).await;
                            let _ = h.emit(
                                "mode-changed",
                                json!({ "mode": mode, "ok": result.is_ok() }),
                            );
                            if result.is_ok() {
                                sync_mode_checks(&h, mode);
                            }
                        });
                    }
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击托盘图标 → 切换主窗口
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn sync_mode_checks(app: &tauri::AppHandle, mode: i64) {
    let Some(items) = app.try_state::<TrayModeItems>() else {
        return;
    };
    for (i, item) in items.items.iter().enumerate() {
        let _ = item.set_checked(i as i64 == mode);
    }
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = main_window(app) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_main(app: &tauri::AppHandle) {
    if let Some(w) = main_window(app) {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }
}

fn main_window(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}
