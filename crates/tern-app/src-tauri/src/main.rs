use tauri::Manager;

use tern_app_lib::{tray, AppState};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            // 托盘先建：它决定了"关窗"是隐藏还是退出。建失败也不阻断启动——
            // 没有托盘时退化成普通窗口应用，路由和面板照常能用
            if let Err(error) = tray::build(app) {
                log::error!("[tern-app] 托盘建不起来，关窗将直接退出: {error}");
            }

            let window = app
                .get_webview_window("main")
                .expect("tauri.conf.json 里应定义 label 为 main 的窗口");
            let _ = window.show();
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if tray::should_prevent_close(app) {
                    // 拦下来藏到托盘：网关照跑（agent 是独立进程），用量继续记。
                    // 窗口坐标在隐藏前记一份，托盘唤回时先验它还在不在屏上
                    api.prevent_close();
                    if let Some(panel) = app.get_webview_window("main") {
                        tray::remember_and_hide(&panel);
                    }
                    log::info!("[tern-app] 面板已隐藏到托盘");
                }
                // 没有托盘，或用户从托盘点了"退出"：放行，让进程正常收尾
            }
        })
        .invoke_handler(tauri::generate_handler![
            // 面板
            tern_app_lib::commands::open_db,
            tern_app_lib::commands::panel_summary,
            // 面板二级视图：趋势 / 占比 / 会话 / 模型流向
            tern_app_lib::commands::panel_trend,
            tern_app_lib::commands::panel_breakdown,
            tern_app_lib::commands::panel_sessions,
            tern_app_lib::commands::panel_model_flow,
            // 网关启停
            tern_app_lib::server::server_start,
            tern_app_lib::server::server_stop,
            tern_app_lib::server::server_status,
            tern_app_lib::server::config_summary,
            // 切换默认供应商
            tern_app_lib::server::select_provider,
            // 拉供应商的模型列表
            tern_app_lib::server::fetch_provider_models,
            // 选模型：写进 ~/.claude/settings.json 的四个档位键
            tern_app_lib::model::claude_model,
            tern_app_lib::model::set_claude_model,
            tern_app_lib::model::clear_claude_model,
            tern_app_lib::server::open_config_dir,
            // 供应商增删改与连通性测试
            tern_app_lib::providers::provider_save,
            tern_app_lib::providers::provider_remove,
            tern_app_lib::providers::provider_detail,
            tern_app_lib::providers::provider_probe,
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
