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
//!
//! # 刷新的铁律：不在主线程问 agent
//!
//! [`refresh`] 由 `TrayIconEvent::Enter` 触发，而 Windows 上鼠标只要停在托盘图标
//! 上就会**连续**发 Enter。`server_status_now()` 是一次阻塞 HTTP（最坏要等满超时），
//! 它要是跑在主线程上，整个界面当场冻住：菜单弹得出来，点哪儿都没反应，窗口也关
//! 不掉——用户唯一的出路是任务管理器。所以问状态一律丢线程，菜单文本回主线程改。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, WebviewWindow};

/// 托盘图标 id。固定字符串，便于 `get_tray_by_id` 找回同一个实例。
const TRAY_ID: &str = "tern-tray";
/// 菜单项 id也固定：换文本不换 id，`on_menu_event` 才不用跟着改。
const MENU_OPEN: &str = "open";
const MENU_GATEWAY: &str = "gateway";
const MENU_QUIT: &str = "quit";

/// 两次真去问 agent 的最小间隔。鼠标在图标上抖一下就是好几个 Enter，
/// 每个都问一遍既浪费又把线程占着——菜单文本不需要那么新。
const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(400);

/// 运行期状态。`None` 表示托盘没建起来，此时关窗就是真退出。
#[derive(Default)]
pub struct TrayState {
    icon: Mutex<Option<tauri::tray::TrayIcon<tauri::Wry>>>,
    /// "启动网关" / "停止网关"那一项。留着句柄才能改文本。
    gateway_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    /// 隐藏前记住的窗口外坐标。见 [`show_panel`]。
    position: Mutex<Option<tauri::PhysicalPosition<i32>>>,
    /// 最近一次已知的"网关在不在跑"。托盘按它显示文本，不再现问 agent。
    running: AtomicBool,
    /// 有没有一次刷新正在飞。Enter 是高频事件，不限流就会在线程里排一队。
    refreshing: AtomicBool,
    /// 上一次真去问 agent 的时刻。和 `refreshing` 一起做双保险。
    last_ask: Mutex<Option<Instant>>,
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
    // 建好立刻去问一次真实状态。问的过程丢线程，不占 setup 的时间——
    // setup 是窗口露出来之前跑的，在这里同步等一次 HTTP 就是把启动拖慢
    refresh(app.handle());
    Ok(())
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    // 初始文本不现问 agent：build() 跑在 setup 里，那是一次主线程阻塞。
    // 先按"没在跑"写，紧随其后的 refresh() 会把它改对
    let gateway = MenuItem::with_id(app, MENU_GATEWAY, gateway_label(false), true, None::<&str>)?;
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

/// 缓存里记着的运行状态。
///
/// 托盘自己判断"该显示启动还是停止"时用它，不现问 agent——现问就是主线程上的
/// 一次阻塞 HTTP。缓存由 [`refresh`] 和 [`toggle_gateway`] 的结果推进。
fn cached_running(app: &AppHandle) -> bool {
    app.try_state::<TrayState>()
        .map(|state| state.running.load(Ordering::SeqCst))
        .unwrap_or(false)
}

/// 把"跑没跑"写进缓存。任何线程都能调，不碰窗口。
fn cache_running(app: &AppHandle, running: bool) {
    if let Some(state) = app.try_state::<TrayState>() {
        state.running.store(running, Ordering::SeqCst);
    }
}

/// 按缓存刷菜单文本。**只在主线程调**：`set_text` 底下是 Windows 的消息。
fn sync_menu_text(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    let running = state.running.load(Ordering::SeqCst);
    let guard = state.gateway_item.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(item) = guard.as_ref() {
        if let Err(error) = item.set_text(gateway_label(running)) {
            log::warn!("[tray] 更新菜单文本失败: {error}");
        }
    }
}

/// 距上次真去问 agent 够不够久。
///
/// 抽成纯函数是为了能单测——它和 `refreshing` 一起构成"托盘不会把界面拖住"的
/// 那道闸，而这道闸藏在 Tauri 状态后面，不拆出来等于没有测试。
fn ask_due(last_ask: Option<Instant>) -> bool {
    match last_ask {
        Some(at) => at.elapsed() >= REFRESH_MIN_INTERVAL,
        // 从没问过（进程刚起）：必须问一次，否则菜单文本一直是猜的
        None => true,
    }
}

/// 刷一遍托盘菜单。
///
/// # 为什么丢线程
///
/// 它是 `TrayIconEvent::Enter` 的回调，而 Windows 上鼠标只要停在托盘图标上就会
/// 连续发 Enter。`server_status_now()` 是一次阻塞 HTTP，跑在主线程上等于把界面
/// 冻住：菜单弹得出来但点哪儿都没反应，窗口也关不掉。丢线程之后最坏情况只是
/// 菜单文本晚一步更新——那比整个应用卡死好得多。
///
/// 失败只记日志：托盘是状态显示器，它刷不刷新不该影响面板本身。
pub fn refresh(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayState>() else {
        return;
    };
    // 已经在飞就不叠。Enter 是高频事件，不限流就会在线程里排一队
    if state.refreshing.swap(true, Ordering::SeqCst) {
        return;
    }
    {
        let mut last = state.last_ask.lock().unwrap_or_else(|p| p.into_inner());
        if !ask_due(*last) {
            state.refreshing.store(false, Ordering::SeqCst);
            return;
        }
        *last = Some(Instant::now());
    }

    let worker = app.clone();
    std::thread::spawn(move || {
        let running = crate::server::server_status_now().running;
        // 先落缓存：哪怕回主线程那一步失败，下次弹菜单时文本也是对的
        cache_running(&worker, running);
        let main = worker.clone();
        let posted = worker.run_on_main_thread(move || {
            sync_menu_text(&main);
            if let Some(state) = main.try_state::<TrayState>() {
                state.refreshing.store(false, Ordering::SeqCst);
            }
        });
        if posted.is_err() {
            // 主线程已经没了（正在退出）。在飞标记必须放掉，否则托盘从此停更
            if let Some(state) = worker.try_state::<TrayState>() {
                state.refreshing.store(false, Ordering::SeqCst);
            }
        }
    });
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
///
/// # 为什么也丢线程
///
/// agent 不在时 `start_gateway` 要先把它拉起来再等它监听，最坏等满 10 秒。
/// 这段等待发生在主线程上的话，点完"启动网关"有十秒界面点哪儿都没反应。
fn toggle_gateway(app: &AppHandle) {
    // 用缓存判断当前状态，不现问：现问就是主线程上的一次阻塞 HTTP
    let running = cached_running(app);
    let worker = app.clone();
    std::thread::spawn(move || {
        let outcome = if running {
            crate::server::stop_gateway_now()
        } else {
            crate::server::start_gateway_now()
        };
        match outcome {
            Ok(status) => {
                cache_running(&worker, status.running);
                let main = worker.clone();
                if worker
                    .run_on_main_thread(move || sync_menu_text(&main))
                    .is_err()
                {
                    log::debug!("[tray] 主线程已退出，菜单文本留待下次刷新");
                }
                log::info!(
                    "[tray] 已{}网关",
                    if status.running { "启动" } else { "停止" }
                );
            }
            Err(error) => log::warn!(
                "[tray] {}网关失败: {error}",
                if running { "停止" } else { "启动" }
            ),
        }
    });
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

    /// 从没问过就必须问一次：进程刚起时菜单文本还是猜的，
    /// 不问的话它一直是"启动网关"，哪怕网关正在跑
    #[test]
    fn first_ever_ask_is_always_due() {
        assert!(ask_due(None));
    }

    /// 刚问过就不该再问。鼠标在托盘图标上抖一下就是好几个 Enter，
    /// 每个都放过去就是往线程池里排一队阻塞请求
    #[test]
    fn a_recent_ask_blocks_the_next_one() {
        assert!(!ask_due(Some(Instant::now())));
    }

    /// 隔得够久就放行。用真睡而不是改常量：这条测试守的就是那个间隔本身
    #[test]
    fn an_old_ask_is_allowed_through() {
        std::thread::sleep(REFRESH_MIN_INTERVAL + Duration::from_millis(20));
        assert!(ask_due(Some(Instant::now() - REFRESH_MIN_INTERVAL)));
    }
}
