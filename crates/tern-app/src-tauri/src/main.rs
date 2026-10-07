use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(tern_app_lib::AppState::default())
        .manage(tern_app_lib::server::ServerState::default())
        .setup(|app| {
            // 关窗即退出：托盘常驻是"待定问题"，先不给用户一个藏在后台的进程。
            // 若以后加托盘，这里要改成 hide 而不是退出。
            let window = app
                .get_webview_window("main")
                .expect("tauri.conf.json 里应定义 label 为 main 的窗口");
            let handle = app.handle().clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Destroyed = event {
                    // 窗口没了就停网关，别留一个占着端口的孤儿进程
                    handle
                        .state::<tern_app_lib::server::ServerState>()
                        .shutdown();
                }
            });
            let _ = window.show();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 面板
            tern_app_lib::commands::open_db,
            tern_app_lib::commands::panel_summary,
            // 网关启停
            tern_app_lib::server::server_start,
            tern_app_lib::server::server_stop,
            tern_app_lib::server::server_status,
            tern_app_lib::server::config_summary,
            tern_app_lib::server::open_config_dir,
            // 首次运行
            tern_app_lib::commands::first_run,
            tern_app_lib::commands::import_preview,
            tern_app_lib::commands::import_from_cc_switch,
            tern_app_lib::commands::write_sample_config,
            // Claude Code 权限的一键放行
            tern_app_lib::permissions::list_permissions,
            tern_app_lib::permissions::allow_permission,
            tern_app_lib::permissions::revoke_permission,
        ])
        .run(tauri::generate_context!())
        .expect("启动 tern 面板失败");
}
