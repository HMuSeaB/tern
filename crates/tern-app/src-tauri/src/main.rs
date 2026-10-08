use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(tern_app_lib::AppState::default())
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("tauri.conf.json 里应定义 label 为 main 的窗口");
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
            // 切换默认供应商
            tern_app_lib::server::select_provider,
            // 拉供应商的模型列表
            tern_app_lib::server::fetch_provider_models,
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
            // 把 Claude Code 的流量接到 tern
            tern_app_lib::wire::wire_status,
            tern_app_lib::wire::wire_enable,
            tern_app_lib::wire::wire_disable,
            tern_app_lib::wire::wire_probe,
            // 供应商分组：自定义文件夹
            tern_app_lib::folders::folders_list,
            tern_app_lib::folders::folders_create,
            tern_app_lib::folders::folders_rename,
            tern_app_lib::folders::folders_delete,
            tern_app_lib::folders::folders_assign,
            tern_app_lib::folders::folders_set_expanded,
            // 供应商分组：按请求地址的域名根自动归组
            tern_app_lib::folders::folders_group_by_domain,
        ])
        .run(tauri::generate_context!())
        .expect("启动 tern 面板失败");
}
