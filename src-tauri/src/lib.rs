#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod commands;
mod config;
mod grammar;
mod hotkey;
mod model;
mod native;
mod nlu;
mod nlu_model;
mod plugins;
mod shell;
mod vfs;

use commands::AppState;
use parking_lot::Mutex;
use std::path::PathBuf;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

/// 自定义 URL 协议前缀（`tinker://…`）。
pub const URL_SCHEME: &str = "tinker";

fn parse_url_args(argv: &[String]) -> Option<String> {
    let prefix = format!("{URL_SCHEME}://");
    argv.iter().find(|a| a.starts_with(&prefix)).cloned()
}

/// 改名前的应用标识。首次以新标识启动时，把旧目录里的索引与配置复制过来，
/// 避免用户被迫重新建库。
const LEGACY_APP_DIR: &str = "com.snail.quicksearch.rust";

fn migrate_legacy_data(data_dir: &std::path::Path) {
    // 新目录已经有内容就不动它
    if data_dir.exists() {
        let has_entries = std::fs::read_dir(data_dir)
            .map(|mut it| it.next().is_some())
            .unwrap_or(true);
        if has_entries {
            return;
        }
    }
    let Some(parent) = data_dir.parent() else {
        return;
    };
    let old = parent.join(LEGACY_APP_DIR);
    if !old.is_dir() {
        return;
    }
    match copy_dir(&old, data_dir) {
        Ok(()) => eprintln!(
            "[boot] 已迁移旧数据目录: {} -> {}",
            old.display(),
            data_dir.display()
        ),
        Err(e) => eprintln!("[boot] 迁移旧数据目录失败: {e}"),
    }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// 取 `--wait-pid <pid>` 的值：提权重启时由旧实例传进来。
fn parse_wait_pid(argv: &[String]) -> Option<u32> {
    let i = argv.iter().position(|a| a == "--wait-pid")?;
    argv.get(i + 1)?.parse().ok()
}

/// 等旧实例真正退出。WebView2 的用户数据目录是独占的，
/// 旧进程还在（或它的 msedgewebview2 子进程还没释放）时新建窗口会直接失败。
#[cfg(windows)]
fn wait_for_parent_exit(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            // 最多等 15 秒，超时也继续启动，免得永久卡住
            let _ = WaitForSingleObject(h, 15_000);
            let _ = CloseHandle(h);
        }
    }
    // 进程退出后，WebView2 的子进程可能还占着用户数据目录一小会儿
    std::thread::sleep(std::time::Duration::from_millis(700));
}

/// 默认窗口大小（逻辑像素）。首次启动、或者上次存下来的尺寸不合理时用它。
const DEFAULT_WIN_W: f64 = 980.0;
const DEFAULT_WIN_H: f64 = 620.0;
const MIN_WIN_W: f64 = 640.0;
const MIN_WIN_H: f64 = 380.0;

/// 还原上次的窗口大小与位置，并夹到当前屏幕内。
///
/// 老版本把 `Resized` 事件里的物理像素当逻辑像素存过（缩放屏上会存出
/// 4256×2687 这种数），启动时再按逻辑像素还原就会撑满甚至超出屏幕，
/// 界面只露出一角。这里的策略是：存下来的尺寸比屏幕还大，就当成脏数据，
/// 回到默认的小窗口并居中。
fn restore_geometry(w: &tauri::WebviewWindow, win: &mut crate::config::WindowCfg) -> bool {
    let scale = w.scale_factor().unwrap_or(1.0);
    // 当前显示器的工作区（逻辑像素）
    let (aw, ah) = match w.current_monitor() {
        Ok(Some(m)) => {
            let s = m.size().to_logical::<f64>(m.scale_factor());
            (s.width, s.height)
        }
        _ => (1280.0, 800.0),
    };

    let want_w = if win.width > 0 {
        win.width as f64
    } else {
        DEFAULT_WIN_W
    };
    let want_h = if win.height > 0 {
        win.height as f64
    } else {
        DEFAULT_WIN_H
    };

    // 超出屏幕可用宽高的 92% 就当脏数据，回默认大小
    let dirty = want_w > aw * 0.92 || want_h > ah * 0.92;
    let (wpx, hpx) = if dirty {
        (DEFAULT_WIN_W.min(aw * 0.9), DEFAULT_WIN_H.min(ah * 0.9))
    } else {
        (want_w, want_h)
    };
    let wpx = wpx.max(MIN_WIN_W).min(aw);
    let hpx = hpx.max(MIN_WIN_H).min(ah);
    let _ = w.set_size(tauri::LogicalSize::new(wpx, hpx));

    // 位置同样是物理像素存的，越界就居中
    let target = match (win.x, win.y) {
        (Some(x), Some(y)) if !dirty => {
            let lx = x as f64 / scale;
            let ly = y as f64 / scale;
            if lx < -8.0 || ly < -8.0 || lx + wpx > aw + 8.0 || ly + hpx > ah + 8.0 {
                None
            } else {
                Some((lx.max(0.0), ly.max(0.0)))
            }
        }
        _ => None,
    };
    match target {
        Some((x, y)) => {
            let _ = w.set_position(tauri::LogicalPosition::new(x, y));
        }
        None => {
            let _ = w.center();
        }
    }

    // 纠正过的尺寸写回配置，下次启动就不用再纠一次
    if dirty {
        win.width = wpx.round() as u32;
        win.height = hpx.round() as u32;
        win.x = None;
        win.y = None;
    }
    dirty
}

/// 挂起的外部链接：前端启动完成（配置已就绪）后主动取走再应用。
/// 直接用 emit + 延时睡眠投递会在冷启动慢的时候丢参数。
pub static PENDING_URL: Mutex<Option<String>> = Mutex::new(None);

fn handle_open(app: &tauri::AppHandle, url: Option<String>) {
    hotkey::show_main(app);
    if let Some(u) = url {
        *PENDING_URL.lock() = Some(u.clone());
        let _ = app.emit("tinker:url", serde_json::json!({ "url": u }));
    }
}

pub fn run() {
    let argv: Vec<String> = std::env::args().collect();
    // 提权重启出来的实例：旧实例正在退出、单实例锁可能还没释放，
    // 所以这个实例不注册单实例插件，直接当成新的主实例运行。
    let elevated_boot = argv.iter().any(|a| a == "--elevated");
    // 提权实例：先等旧实例退出，再创建 WebView2（同一用户数据目录不能两个进程同时用）
    #[cfg(windows)]
    if let Some(pid) = parse_wait_pid(&argv) {
        wait_for_parent_exit(pid);
    }

    let mut builder = tauri::Builder::default();
    if !elevated_boot {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_open(app, parse_url_args(&argv));
        }));
    }
    let builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // 全局快捷键插件必须先于 setup 注册：setup 里要用它绑定组合键
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();

            // ---- 配置目录 ----
            let data_dir = handle
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            let cfg_dir = handle
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| data_dir.clone());
            // 应用标识从 com.snail.quicksearch.rust 改成了 com.tinker.search，
            // 首次启动先把旧目录的索引/配置搬过来（新目录已有内容则跳过）。
            migrate_legacy_data(&data_dir);
            let _ = std::fs::create_dir_all(&cfg_dir);
            let _ = std::fs::create_dir_all(&data_dir);
            let cfg_path = cfg_dir.join("config.json");
            let mut cfg = config::AppConfig::load(&cfg_path);

            // ---- 索引内核 ----
            // sscore 在本进程内建库与查询，USN 同步也在本进程，不依赖外部组件。
            let db = data_dir.join("sscore.idx");
            native::init(db);
            // 恢复上次的排序方式；字段名来自旧版本的配置（如「名称」「位置」）时
            // 回退到「相关度」，避免界面显示的排序方式与内核实际状态不一致。
            if !native::sort_fields().iter().any(|f| f == &cfg.sort.field) {
                cfg.sort.field = "相关度".into();
                cfg.sort.asc = false;
            }
            let _ = native::set_sort(&handle, &cfg.sort.field, cfg.sort.asc);
            if let Some(e) = native::load(&handle).err() {
                eprintln!("[boot] 索引加载失败: {e}");
            }

            app.manage(AppState {
                cfg: Mutex::new(cfg.clone()),
                cfg_path,
                data_dir,
            });

            // ---- 主窗口 ----
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_always_on_top(cfg.window.always_on_top);
                let mut win_cfg = cfg.window.clone();
                if restore_geometry(&w, &mut win_cfg) {
                    // 存下来的尺寸不可用（例如老版本把物理像素当逻辑像素存），
                    // 把纠正后的值立刻落盘，免得每次启动都纠一遍
                    if let Some(state) = app.try_state::<AppState>() {
                        state.cfg.lock().window = win_cfg;
                        state.save();
                    }
                }
                // 启动时也走这套：只调 set_focus 的话，前台窗口锁会让窗口
                // 停在别的程序后面，看着像「点了没反应」
                hotkey::bring_to_front(&w);
            }

            // ---- 托盘 ----
            build_tray(&handle);

            // ---- 全局呼出快捷键 ----
            // 组合键走 global-shortcut 插件（RegisterHotKey），双击修饰键才挂低层钩子。
            hotkey::install(handle.clone());
            hotkey::apply_or_fallback(&cfg.window.shortcut, cfg.window.double_ctrl);

            // 协议已注册的话，按当前 exe 路径刷新一次（程序挪位置后不用手动改注册表）
            if cfg.integration.protocol {
                if let Err(e) = shell::register_url_protocol(true) {
                    eprintln!("[boot] 刷新 URL 协议注册失败: {e}");
                }
            }

            // 启动参数里的 URL 协议：挂起来等前端就绪后取走
            let argv: Vec<String> = std::env::args().collect();
            if let Some(u) = parse_url_args(&argv) {
                *PENDING_URL.lock() = Some(u);
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                // 关闭 = 收进托盘，与原生版行为一致。
                // 必须走 tao 的 hide()：它会同步把内部 WindowFlags::VISIBLE
                // 置为 false。若只调原生 SW_HIDE，之后 maximize() 的
                // apply_diff 会按陈旧标志重设样式，窗口和自定义标题栏会一起消失。
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(false) => {
                if let Some(state) = window.try_state::<AppState>() {
                    if state.get().window.hide_on_blur {
                        let _ = window.hide();
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(state) = window.try_state::<AppState>() {
                    let mut cfg = state.cfg.lock();
                    if window.is_maximized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false)
                    {
                        return;
                    }
                    // 窗口事件给的是**物理像素**，配置里存的是逻辑像素：
                    // 不换算的话，在 200% 缩放的屏上会把 1200 存成 2400，
                    // 下次启动再按逻辑像素还原，窗口就直接撑到屏幕外了。
                    let s = size.to_logical::<f64>(window.scale_factor().unwrap_or(1.0));
                    cfg.window.width = s.width.round().max(1.0) as u32;
                    cfg.window.height = s.height.round().max(1.0) as u32;
                }
            }
            WindowEvent::Moved(pos) => {
                if let Some(state) = window.try_state::<AppState>() {
                    let mut cfg = state.cfg.lock();
                    if window.is_maximized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false)
                    {
                        return;
                    }
                    let p = pos.to_logical::<f64>(window.scale_factor().unwrap_or(1.0));
                    cfg.window.x = Some(p.x.round() as i32);
                    cfg.window.y = Some(p.y.round() as i32);
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::engine_ready,
            commands::search,
            commands::cancel_search,
            commands::nl_status,
            commands::nl_parse,
            commands::nl_search,
            commands::plugins_list,
            commands::plugin_market,
            commands::plugin_install,
            commands::plugin_install_url,
            commands::plugin_update,
            commands::plugin_set_enabled,
            commands::plugin_uninstall,
            commands::plugin_set_engine,
            commands::plugin_market_url,
            commands::set_plugin_market_url,
            commands::get_rows,
            commands::get_row,
            commands::list_dir,
            commands::get_icons,
            commands::get_preview_icon,
            commands::get_filters,
            commands::get_sort_fields,
            commands::set_sort,
            commands::set_filter,
            commands::set_match,
            commands::match_settings_json,
            commands::add_boost,
            commands::sql_result,
            commands::home_snapshot,
            commands::known_places,
            commands::engine_prompt,
            commands::index_status,
            commands::mem_stats,
            commands::trim_memory,
            commands::list_drives,
            commands::start_scan,
            commands::stop_scan,
            commands::save_db,
            commands::excluded_dirs,
            commands::add_excluded,
            commands::remove_excluded,
            commands::remove_index_path,
            commands::perf_text,
            commands::engine_last_error,
            commands::set_alias,
            commands::boost_rating,
            commands::open_file,
            commands::resolve_link,
            commands::reveal_file,
            commands::file_properties,
            commands::delete_files,
            commands::rename_file,
            commands::copy_paths,
            commands::copy_text,
            commands::open_dir,
            commands::config_get,
            commands::config_save,
            commands::history_add,
            commands::history_remove,
            commands::history_clear,
            commands::set_always_on_top,
            commands::hide_window,
            commands::minimize_window,
            commands::toggle_maximize,
            commands::app_exit,
            commands::elevate,
            commands::is_admin,
            commands::toggle_hotkey,
            commands::set_hotkey,
            commands::window_outside_click_hide,
            commands::register_url_protocol,
            commands::take_pending_url,
            commands::url_protocol_registered,
            commands::hotkey_hook_ok,
            commands::read_text_file,
        ]);

    let app = builder.build(tauri::generate_context!()).expect("启动失败");
    app.run(|handle, event| {
        if let RunEvent::ExitRequested { .. } = event {
            // 兜底：无论从哪条路径退出，都保证索引落盘
            let h = handle.clone();
            std::thread::spawn(move || {
                commands::shutdown_engine();
                let _ = h;
            })
            .join()
            .ok();
        }
    });
}

fn build_tray(app: &tauri::AppHandle) {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>);
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>);
    let save = MenuItem::with_id(app, "save", "保存索引", true, None::<&str>);
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>);
    let (Ok(show), Ok(settings), Ok(save), Ok(quit)) = (show, settings, save, quit) else {
        return;
    };
    let sep = PredefinedMenuItem::separator(app).ok();
    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&show, &settings];
    if let Some(s) = &sep {
        items.push(s);
    }
    items.push(&save);
    items.push(&quit);
    let Ok(menu) = Menu::with_items(app, &items) else {
        return;
    };
    let icon = app.default_window_icon().cloned();
    let mut b = TrayIconBuilder::with_id("main-tray")
        .tooltip("tinker搜索")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "show" => hotkey::show_main(app),
            "settings" => {
                // 先把窗口叫回来，再让前端把设置面板打开
                hotkey::show_main(app);
                let _ = app.emit("ui:open-settings", serde_json::json!({ "tab": "search" }));
            }
            "save" => {
                std::thread::spawn(|| {
                    let _ = commands::save_index();
                });
            }
            "quit" => {
                std::thread::spawn(commands::shutdown_engine);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                let h = tray.app_handle().clone();
                hotkey::show_main(&h);
            }
        });
    if let Some(i) = icon {
        b = b.icon(i);
    }
    let _ = b.build(app);
}
