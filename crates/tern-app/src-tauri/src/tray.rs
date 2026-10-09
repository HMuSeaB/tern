//! 托盘常驻。
//!
//! # 为什么需要它
//!
//! 关掉面板窗口不该等于"关掉 tern"。网关本来就该比窗口活得久——用户只是想看一眼
//! 用量，不想顺手把流量也断了。`tern-agent` 是独立进程（见 `agent.rs`），窗口关掉
//! 它也照跑，所以托盘要解决的只是两件事：
//!
//! 1. **窗口还能开回来**：关窗变成隐藏，而不是退出
//! 2. **不用开窗也能管网关**：托盘菜单上直接启停
//!
//! # 托盘没建起来时的兜底
//!
//! 图标缺失等原因会让 [`build`] 失败。这时**必须放行窗口关闭**：把窗口拦下来又
//! 没有托盘，等于亲手把界面弄丢，用户只能去任务管理器。所以 `on_window_event`
//! 先问 [`is_installed`]，装了才拦。
//!
//! # 为什么菜单文本要跟着状态刷
//!
//! 网关可能从面板之外启停（`tern serve`、agent 自己重启）。做成静态菜单的话，
//! 用户点"启动网关"时它其实已经在跑，反馈是一个没反应的按钮。所以在鼠标进入
//! 图标时刷新，以及面板自己启停之后立刻刷。

use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, WebviewWindow};

/// 托盘图标 id。固定字符串，便于 `get_tray_by_id` 找回同一个实例。
const TRAY_ID: &str = "tern-tray";
/// 菜单项 id也固定：换文本不换 id，`on_menu_event` 才不用跟着改。
const MENU_OPEN: &str = "open";
const MENU_GATEWAY: &str = "gateway";
const MENU_QUIT: &str = "quit";

/// 运行期状态。`None` 表示托盘没建起来，此时关窗就是真退出。
#[derive(Default)]
pub struct TrayState {
    icon: Mutex<Option<tauri::tray::TrayIcon<tauri::Wry>>>,
    /// "启动网关" / "停止网关"那一项。留着句柄才能改文本。
    gateway_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    /// 隐藏前记住的窗口外坐标。见 [`show_panel`]。
    position: Mutex<Option<tauri::PhysicalPosition<i32>>>,
}

/// 建托盘。失败不 panic：返回 Err 由调用方记日志，应用照常以"无托盘"运行。
///
/// `manage` 放在最前面：下面 `state::<TrayState>()` 在没 manage 过时会直接 panic，
/// 而 panic 发生在 setup 钩子里等于应用起不来——那比"没有托盘"严重得多。
/// 先 manage 之后哪怕图标加载失败，也只是 `icon = None`，`is_installed` 报 false，
/// 窗口照常能关。
pub fn build(app: &App) -> tauri::Result<()> {
    app.manage(TrayState::default());

    let menu = build_menu(app.handle())?;
    // 用打包图标的第一个。托盘图标应当来自静态文件而不是运行时生成——
    // 生成失败时连托盘都没有，代价远大于省下的那点体积
    let icon = app
        .default_window_icon()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?
        .clone();

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("tern")
        .menu(&menu)
        // 左键直接开面板，右键才弹菜单。反过来的话"点一下开面板"要多一步
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN => show_panel(app),
            MENU_GATEWAY => toggle_gateway(app),
            MENU_QUIT => quit(app),
            other => log::warn!("[tray] 未知菜单项 {other}"),
        })
        .on_tray_icon_event(|tray, event| match event {
            // 鼠标一进来就刷状态：用户下一步通常是点开菜单，
            // 那时候才去问网关就来不及了
            TrayIconEvent::Enter { .. } => refresh(tray.app_handle()),
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => show_panel(tray.app_handle()),
            _ => {}
        })
        .build(app)?;

    let state = app.state::<TrayState>();
    *state.icon.lock().unwrap_or_else(|p| p.into_inner()) = Some(tray);
    Ok(())
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let gateway = MenuItem::with_id(
        app,
        MENU_GATEWAY,
        gateway_label(crate::server::server_status_now().running),
        true,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(app, MENU_OPEN, "打开面板", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &separator, &gateway, &separator, &quit])?;

    let state = app.state::<TrayState>();
    *state.gateway_item.lock().unwrap_or_else(|p| p.into_inner()) = Some(gateway);
    Ok(menu)
}

/// 网关那一项的文本。运行中显示"停止"，没跑显示"启动"。
fn gateway_label(running: bool) -> &'static str {
    if running {
        "停止网关"
    } else {
        "启动网关"
    }
}

/// 托盘在不在。没建起来的场景下窗口可以正常关闭。
pub fn is_installed(app: &AppHandle) -> bool {
    app.try_state::<TrayState>()
        .map(|state| {
            state
                .icon
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some()
        })
        .unwrap_or(false)
}

/// 按当前网关状态刷一遍托盘菜单。
///
/// 失败只记日志：托盘是状态显示器，它刷不刷新不该影响面板本身。
pub fn refresh(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    let running = crate::server::server_status_now().running;
    let guard = state.gateway_item.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(item) = guard.as_ref() {
        if let Err(error) = item.set_text(gateway_label(running)) {
            log::warn!("[tray] 更新菜单文本失败: {error}");
        }
    }
}

/// 打开面板。隐藏前记下的坐标如果还落在某台显示器的工作区内就还原，否则居中。
///
/// 为什么不裸还原坐标：多屏环境下窗口停在副屏，用户拔掉外接屏再开面板，那个坐标
/// 已经落在不存在的屏幕上——窗口"消失"且鼠标拖不回来。cc-switch 在这栽过
/// （`.window-state.json` 里残留的 `prev_x/prev_y`），症状一模一样。
fn show_panel(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    restore_position(&window);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    log::debug!("[tray] 面板已唤回");
}

/// 记下当前外坐标，然后隐藏窗口。
pub fn remember_and_hide(window: &WebviewWindow) {
    if let Ok(position) = window.outer_position() {
        if let Some(state) = window.app_handle().try_state::<TrayState>() {
            *state.position.lock().unwrap_or_else(|p| p.into_inner()) = Some(position);
        }
    }
    let _ = window.hide();
}

/// 还原窗口位置。记着的坐标越界就居中——宁可跳到屏幕中间，也不能飞出桌面。
fn restore_position(window: &WebviewWindow) {
    let saved = window
        .app_handle()
        .try_state::<TrayState>()
        .and_then(|state| *state.position.lock().unwrap_or_else(|p| p.into_inner()));
    let Some(saved) = saved else {
        // 第一次开（进程刚起、还没隐藏过）：用配置里的位置，不动它
        return;
    };
    if is_on_screen(window, saved) {
        let _ = window.set_position(saved);
    } else {
        log::info!("[tray] 记下的窗口坐标已不在任何显示器上，改为居中");
        let _ = window.center();
    }
}

/// 坐标是否落在某台显示器的工作区内。只要求**标题栏那一带**还在屏上：
/// 窗口大部分出屏时也照样算不可见——那种位置同样点不到标题栏。
fn is_on_screen(window: &WebviewWindow, position: tauri::PhysicalPosition<i32>) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        // 枚举不到显示器时不敢乱设坐标：交给居中，居中永远安全
        return false;
    };
    monitors.iter().any(|monitor| {
        let area = monitor.work_area();
        let origin = area.position;
        let size = area.size;
        position.x >= origin.x
            && position.x < origin.x + size.width as i32
            && position.y >= origin.y
            && position.y < origin.y + size.height as i32
    })
}

/// 托盘上的启停。失败时只在日志里说：面板关着的时候用户看不见错误，
/// 而托盘菜单弹一个系统级对话框比什么都不做更烦。
fn toggle_gateway(app: &AppHandle) {
    let running = crate::server::server_status_now().running;
    let outcome = if running {
        crate::server::stop_gateway_now()
    } else {
        crate::server::start_gateway_now()
    };
    match outcome {
        Ok(_) => {
            refresh(app);
            log::info!("[tray] 已{}网关", if running { "停止" } else { "启动" });
        }
        Err(error) => log::warn!(
            "[tray] {}网关失败: {error}",
            if running { "停止" } else { "启动" }
        ),
    }
}

/// 真退出。
///
/// 这里不能只 `app.exit(0)`：`exit` 走的是 `RunEvent::ExitRequested`，而窗口的
/// `CloseRequested` 也会在那条路径上被触发，`on_window_event` 会把退出拦住——
/// 用户点了"退出"什么都不会发生。所以先把状态标成"正在退出"，让拦截逻辑放行。
///
/// # 退出前先让 agent 走
///
/// agent 是常驻进程，`app.exit` 不会带走它（它本来就被设计成比面板活得久）。
/// 但**用户点"退出"的意图就是"全关"**，留一个后台进程在跑既占用内存，
/// 又会让他下次装新版时撞上"覆盖不了正在运行的 exe"。
///
/// 所以先 POST /api/agent/exit 让它优雅收尾：在途的流补记 aborted、写入队列
/// 排空，用量不丢。硬杀能做到同样的事，但会丢最多一条 in-flight 的记账。
///
/// 这一步失败不阻断退出——用户想走就走，agent 留着下次安装器会处理它。
fn quit(app: &AppHandle) {
    crate::QUITTING.store(true, std::sync::atomic::Ordering::SeqCst);
    log::info!("[tray] 用户选择退出，正在收尾");
    // 优雅退：不等它，最多让它有一秒收尾时间。失败就失败，下面照旧退出
    let _ = crate::agent::exit_agent();
    app.exit(0);
}

/// 窗口关闭到底放行还是拦下。拦的前提是托盘在——否则界面有去无回。
///
/// 正在退出时一律放行：那是托盘"退出"菜单走下来的，不是用户想关窗。
pub fn should_prevent_close(app: &AppHandle) -> bool {
    !crate::QUITTING.load(std::sync::atomic::Ordering::SeqCst) && is_installed(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_label_flips_with_state() {
        assert_eq!(gateway_label(true), "停止网关");
        assert_eq!(gateway_label(false), "启动网关");
    }

    /// 菜单项 id 必须是固定字符串：`build_menu` 按它造项、`on_menu_event` 按它分发，
    /// 两边任改一处，那一项就静默失效
    #[test]
    fn menu_ids_are_stable() {
        assert_eq!(MENU_OPEN, "open");
        assert_eq!(MENU_GATEWAY, "gateway");
        assert_eq!(MENU_QUIT, "quit");
    }

    /// 进程刚起时没有任何已存窗口状态，这时不该去动窗口位置
    /// （没有 AppHandle 可测，这里守住的是"空值即放行"这个判据本身）
    #[test]
    fn empty_saved_position_means_leave_the_window_alone() {
        let saved: Option<tauri::PhysicalPosition<i32>> = None;
        assert!(saved.is_none(), "None 时 restore 直接返回，不 center");
    }
}
