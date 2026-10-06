use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(tern_app_lib::AppState::default())
        .setup(|app| {
            // 窗口先隐藏，数据就绪后再显示，避免用户看到一屏空白
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            tern_app_lib::commands::open_db,
            tern_app_lib::commands::panel_summary
        ])
        .run(tauri::generate_context!())
        .expect("启动 tern 面板失败");
}
