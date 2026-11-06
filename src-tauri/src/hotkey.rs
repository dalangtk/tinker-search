//! 全局呼出快捷键。
//!
//! 两套机制，按绑定类型二选一，各自用自己最可靠的那条路：
//!
//! * **组合键**（`Ctrl+Q` / `Alt+Space` / `Win+Q` …）交给
//!   `tauri-plugin-global-shortcut`，底层是 Windows 的 `RegisterHotKey`：
//!   由系统做按键匹配，带 `MOD_NOREPEAT`，我们不再自己跟踪修饰键状态，
//!   也就不存在「松键事件丢了 → 状态卡住 → 第一次不响应 / 裸键也能触发」这类问题。
//! * **双击修饰键**（`DoubleCtrl` …）只能用 `WH_KEYBOARD_LL` 低层钩子：
//!   `RegisterHotKey` 表达不了「同一个修饰键连按两次」，这部分必须看键流。
//!
//! 钩子只在双击模式下响应按键（组合键走系统热键，钩子只是原样放行）。
//!
//! 钩子跑在独立线程的 GetMessage 循环上。回调里**绝不做窗口操作**：
//! tao 的窗口 API 要同步等主线程事件循环，一旦主线程正忙，回调就会超时
//! （Windows 的 LowLevelHooksTimeout 到点会把钩子悄悄摘掉）。回调只往通道里
//! 丢一个信号，真正的窗口操作由另一个线程去做。组合键的 handler 本身就在主线程，
//! 直接做即可。
//!
//! 窗口显隐一律走 tao、且放到主线程按序执行：本窗口是 `decorations: false`，
//! 如果只用原生 `SW_SHOW` 把它显示出来，tao 内部的 `WindowFlags::VISIBLE`
//! 仍停留在 false，之后任何窗口操作（`maximize()` 等）的 `apply_diff` 都会
//! 补一刀 `SW_HIDE` 并重设窗口样式 —— 表现就是「窗口连标题栏一起不见了」。
//! 原生 Win32 在这里只负责立即还原最小化与突破前台锁置前。

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// 钩子线程 → 窗口操作线程的信号通道（只有双击模式用得到）
static TX: OnceCell<Sender<()>> = OnceCell::new();
/// 应用句柄：注册组合键时要用
static APP: OnceCell<AppHandle> = OnceCell::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
/// 双击模式：目标修饰键族 id；0 = 当前绑定不走钩子
static HOOK_FAMILY: AtomicU64 = AtomicU64::new(0);
/// 钩子是否真的挂上了（杀软/权限可能让 SetWindowsHookExW 失败，界面要能看到）
static HOOK_OK: AtomicBool = AtomicBool::new(false);
/// 组合键是否注册成功（被别的程序占用时 RegisterHotKey 会失败）
static COMBO_OK: AtomicBool = AtomicBool::new(true);
/// 双击模式：上次按下目标键之后是否夹杂了别的键
static OTHER_KEY: AtomicBool = AtomicBool::new(false);
static DOUBLE_LAST: Mutex<Option<Instant>> = Mutex::new(None);
/// 仅在调试版、且设了 TINKER_HOTKEY_TEST 时才放行注入的按键，方便自动化验证
static ALLOW_INJECTED: AtomicBool = AtomicBool::new(false);
/// 当前绑定与已注册的组合键
static CUR: Mutex<Option<Shortcut>> = Mutex::new(None);
static REGISTERED: Mutex<Option<Shortcut>> = Mutex::new(None);
static DOUBLE_BINDING: AtomicU64 = AtomicU64::new(0);

const DOUBLE_WINDOW: Duration = Duration::from_millis(420);
const LLKHF_INJECTED: u32 = 0x10;

// ---- 双击族 ----
const F_CTRL: u64 = 1;
const F_ALT: u64 = 2;
const F_SHIFT: u64 = 3;
const F_WIN: u64 = 4;

/// 解析出来的绑定
#[derive(Clone, Copy, PartialEq, Debug)]
enum Binding {
    /// 未绑定 / 关闭
    None,
    /// 双击修饰键，参数是族 id
    Double(u64),
    /// 组合键，已被 global-hotkey 解析成 `Shortcut`
    Combo(Shortcut),
}

fn double_family(name: &str) -> Option<u64> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(F_CTRL),
        "alt" => Some(F_ALT),
        "shift" => Some(F_SHIFT),
        "win" | "meta" | "super" => Some(F_WIN),
        _ => None,
    }
}

/// 「Ctrl / Alt / Shift / Win」在这些地方都统一成 global-hotkey 认识的写法。
/// 注意 `Win` 在 global-hotkey 里叫 `Super`。
fn mod_token(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some("Ctrl"),
        "alt" => Some("Alt"),
        "shift" => Some("Shift"),
        "win" | "meta" | "super" => Some("Super"),
        _ => None,
    }
}

/// 把配置串解析成绑定。返回 None 表示格式非法；空串 / None 解析成 `Binding::None`。
fn parse_binding(spec: &str) -> Option<Binding> {
    let s = spec.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Some(Binding::None);
    }

    // DoubleCtrl / DoubleAlt / …
    let dbl = match s.to_ascii_lowercase().as_str() {
        "doublectrl" | "doubleshift" | "doublealt" | "doublewin" => {
            let name = &s[6..];
            double_family(name).map(Binding::Double)
        }
        _ => None,
    };
    if let Some(b) = dbl {
        return Some(b);
    }

    // 也接受 X+X 写法（Ctrl+Ctrl）
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.len() == 2 && parts[0].eq_ignore_ascii_case(parts[1]) {
        if let Some(f) = double_family(parts[0]) {
            return Some(Binding::Double(f));
        }
    }
    if parts.len() < 2 {
        return None; // 裸键会把该键从整个系统拦下来，拒绝
    }

    let mut mods: Vec<&'static str> = Vec::new();
    let mut key: Option<&str> = None;
    for p in parts {
        match mod_token(p) {
            Some(m) => {
                if !mods.contains(&m) {
                    mods.push(m);
                }
            }
            None => {
                if key.is_some() {
                    return None; // 只认一个主键
                }
                key = Some(p);
            }
        }
    }
    let key = key?;
    if mods.is_empty() {
        return None;
    }
    let rebuilt = format!("{}+{}", mods.join("+"), key);
    rebuilt.parse::<Shortcut>().ok().map(Binding::Combo)
}

/// 应用设置面板里的快捷键配置，立即生效，返回是否成功。
/// 失败分两种：配置串非法（`valid()` 为 false）、注册被占用。
pub fn apply(spec: &str, enabled: bool) -> bool {
    let Some(binding) = parse_binding(spec) else {
        return false;
    };
    install_binding(binding, enabled)
}

/// 配置串本身是否合法（只看语法，不做注册）
pub fn valid(spec: &str) -> bool {
    parse_binding(spec).is_some()
}

/// 给启动与保存配置用：配置串非法才回落到双击 Ctrl；
/// 组合键被别的程序占用时**不回退**，让设置页如实提示「没生效」，
/// 免得界面写着 Ctrl+Q、实际却在听双击 Ctrl。
pub fn apply_or_fallback(spec: &str, enabled: bool) {
    if apply(spec, enabled) {
        return;
    }
    if !valid(spec) {
        let _ = apply("DoubleCtrl", enabled);
    }
}

/// 只开关、不改绑定
pub fn set_enabled(on: bool) {
    let cur = *CUR.lock();
    match cur {
        Some(sc) => {
            install_binding(Binding::Combo(sc), on);
        }
        None => {
            let dbl = DOUBLE_BINDING.load(Ordering::SeqCst);
            install_binding(
                if dbl != 0 {
                    Binding::Double(dbl)
                } else {
                    Binding::None
                },
                on,
            );
        }
    }
}

fn install_binding(binding: Binding, enabled: bool) -> bool {
    // 同一个组合键被重复应用（保存配置、前端再推一次）时不折腾：
    // 先 unregister 再 register 的间隙可能被别的程序抢注
    if let Binding::Combo(sc) = binding {
        if enabled && *REGISTERED.lock() == Some(sc) {
            *CUR.lock() = Some(sc);
            return true;
        }
    }

    // 先撤销旧的组合键注册：换绑、关闭、切到双击模式都要走这一步
    unregister_combo();
    ENABLED.store(enabled, Ordering::SeqCst);

    match binding {
        Binding::None => {
            *CUR.lock() = None;
            DOUBLE_BINDING.store(0, Ordering::SeqCst);
            HOOK_FAMILY.store(0, Ordering::SeqCst);
            COMBO_OK.store(true, Ordering::SeqCst);
            true
        }
        Binding::Double(fam) => {
            *CUR.lock() = None;
            DOUBLE_BINDING.store(fam, Ordering::SeqCst);
            HOOK_FAMILY.store(fam, Ordering::SeqCst);
            *DOUBLE_LAST.lock() = None;
            OTHER_KEY.store(false, Ordering::SeqCst);
            COMBO_OK.store(true, Ordering::SeqCst);
            // 钩子是否真挂上由 hook_ok() 单独报告（安装是异步的）
            true
        }
        Binding::Combo(sc) => {
            *CUR.lock() = Some(sc);
            DOUBLE_BINDING.store(0, Ordering::SeqCst);
            HOOK_FAMILY.store(0, Ordering::SeqCst);
            if !enabled {
                COMBO_OK.store(true, Ordering::SeqCst);
                return true;
            }
            register_combo(sc)
        }
    }
}

fn register_combo(sc: Shortcut) -> bool {
    let Some(app) = APP.get() else {
        COMBO_OK.store(false, Ordering::SeqCst);
        return false;
    };
    let res = app.global_shortcut().on_shortcut(sc, |app, _sc, ev| {
        // RegisterHotKey 只在按下时给 WM_HOTKEY；Released 分支留作保险
        if ev.state == ShortcutState::Pressed && ENABLED.load(Ordering::SeqCst) {
            show_main(app);
        }
    });
    match res {
        Ok(()) => {
            *REGISTERED.lock() = Some(sc);
            COMBO_OK.store(true, Ordering::SeqCst);
            true
        }
        Err(e) => {
            // 常见原因：被别的程序抢注（如 Win+Q 被系统占用）
            eprintln!("[hotkey] 注册组合键失败: {e}");
            COMBO_OK.store(false, Ordering::SeqCst);
            false
        }
    }
}

fn unregister_combo() {
    let sc = REGISTERED.lock().take();
    if let (Some(sc), Some(app)) = (sc, APP.get()) {
        let _ = app.global_shortcut().unregister(sc);
    }
}

fn fire() {
    if let Some(tx) = TX.get() {
        let _ = tx.send(());
    }
}

/// 双击修饰键判定。只有连续两次按同一族、中间没夹别的键才算。
fn on_double_down(vk: u32, fam: u64) {
    if family_of(vk) != Some(fam) {
        OTHER_KEY.store(true, Ordering::SeqCst);
        return;
    }
    if OTHER_KEY.swap(false, Ordering::SeqCst) {
        *DOUBLE_LAST.lock() = Some(Instant::now());
        return;
    }
    let mut guard = DOUBLE_LAST.lock();
    if guard.map(|t| t.elapsed() <= DOUBLE_WINDOW).unwrap_or(false) {
        *guard = None;
        fire();
    } else {
        *guard = Some(Instant::now());
    }
}

/// 低层键盘钩子报告的是物理键：左右 Ctrl 是 0xA2/0xA3，不是通用 VK_CONTROL(0x11)。
const VK_SHIFT_G: u32 = 0x10;
const VK_CONTROL_G: u32 = 0x11;
const VK_MENU_G: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

fn family_of(vk: u32) -> Option<u64> {
    match vk {
        // Shift 族
        0xA0 | 0xA1 | VK_SHIFT_G => Some(F_SHIFT),
        // Ctrl 族
        0xA2 | 0xA3 | VK_CONTROL_G => Some(F_CTRL),
        // Alt 族
        0xA4 | 0xA5 | VK_MENU_G => Some(F_ALT),
        VK_LWIN | VK_RWIN => Some(F_WIN),
        _ => None,
    }
}

/// 原生窗口句柄。tauri 的 HWND 来自它自己的 windows 版本，这里只借指针值。
fn native_hwnd(w: &tauri::WebviewWindow) -> Option<HWND> {
    w.hwnd()
        .ok()
        .map(|h| HWND(h.0 as isize as *mut core::ffi::c_void))
}

/// 立即还原最小化并突破前台锁置前。
/// 不改窗口样式、不碰 tao 的显隐状态——那部分交给主线程里的 show/unminimize。
fn restore_and_foreground(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, IsIconic, IsWindowVisible, SetForegroundWindow,
        ShowWindow, SwitchToThisWindow, SW_RESTORE, SW_SHOW,
    };

    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }

        // 前台锁：别的程序正被操作时 SetForegroundWindow 会被忽略。
        // 绝不能用 AttachThreadInput 去绕：它会扰动输入队列，导致系统
        // 静默摘掉低层键盘钩子，热键从此失灵。
        for _ in 0..3 {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            if GetForegroundWindow() == hwnd {
                break;
            }
            SwitchToThisWindow(hwnd, true);
            if GetForegroundWindow() == hwnd {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

/// 把主窗口还原并显示到前台。快捷键只做这一件事，不再隐藏窗口。
///
/// 关键顺序：`show()` 必须在 `unminimize()` 之前。tao 的 apply_diff 在
/// 窗口标志里 VISIBLE=false 时会补一刀 SW_HIDE，若先 unminimize 就会把
/// 刚显示的窗口又藏回去；更糟的是之后 maximize() 也会照着这个陈旧标志
/// 隐藏窗口并重设样式，自定义标题栏跟着一起消失。
/// tao 的 execute_in_thread 在主线程内是同步调用，所以整个闭包跑完时
/// 内部标志与真实窗口状态已经一致。
pub fn bring_to_front(w: &tauri::WebviewWindow) {
    if let Some(hwnd) = native_hwnd(w) {
        // 已在窗口线程（启动时、托盘/单实例/快捷键回调）：直接同步做，
        // 否则 post 到主线程后自己等自己，会白等一次超时。
        let on_window_thread = unsafe {
            let tid = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, None);
            tid != 0 && tid == windows::Win32::System::Threading::GetCurrentThreadId()
        };
        if on_window_thread {
            let _ = w.show();
            let _ = w.unminimize();
            restore_and_foreground(hwnd);
            return;
        }

        let raw = hwnd.0 as isize;
        let w2 = w.clone();
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let posted = w
            .run_on_main_thread(move || {
                let _ = w2.show();
                let _ = w2.unminimize();
                restore_and_foreground(HWND(raw as *mut core::ffi::c_void));
                let _ = tx.send(());
            })
            .is_ok();
        if posted {
            // 等主线程做完再返回，保证后续（比如用户紧接着点最大化）看到的是一致状态
            let _ = rx.recv_timeout(Duration::from_millis(300));
        } else {
            let _ = w.show();
            let _ = w.unminimize();
            restore_and_foreground(hwnd);
        }
        return;
    }

    // 拿不到原生句柄时的回退路径
    if w.is_minimized().unwrap_or(false) {
        let _ = w.unminimize();
    }
    let _ = w.show();
    let _ = w.set_focus();
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        bring_to_front(&w);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && ENABLED.load(Ordering::Relaxed) {
        let msg = wp.0 as u32;
        // 双击只需要按下事件；松键不参与判定，所以这里不看 WM_KEYUP
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            let fam = HOOK_FAMILY.load(Ordering::Relaxed);
            if fam != 0 {
                let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
                let injected = kb.flags.0 & LLKHF_INJECTED != 0;
                if !injected || ALLOW_INJECTED.load(Ordering::Relaxed) {
                    on_double_down(kb.vkCode, fam);
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 快捷键是否在正常工作：
/// 组合键看注册结果，双击看低层钩子有没有挂上。
pub fn hook_ok() -> bool {
    if HOOK_FAMILY.load(Ordering::SeqCst) != 0 {
        HOOK_OK.load(Ordering::SeqCst)
    } else {
        COMBO_OK.load(Ordering::SeqCst)
    }
}

pub fn install(app: AppHandle) {
    if cfg!(debug_assertions) && std::env::var_os("TINKER_HOTKEY_TEST").is_some() {
        ALLOW_INJECTED.store(true, Ordering::SeqCst);
    }
    let _ = APP.set(app.clone());

    // 窗口操作线程：双击模式收到信号再做还原/置前，不阻塞钩子回调
    if TX.get().is_none() {
        let (tx, rx) = mpsc::channel::<()>();
        if TX.set(tx).is_ok() {
            let app = app.clone();
            std::thread::Builder::new()
                .name("hotkey-action".into())
                .spawn(move || {
                    // 连发时只处理最后一次，避免抖动
                    while rx.recv().is_ok() {
                        while rx.try_recv().is_ok() {}
                        show_main(&app);
                    }
                })
                .ok();
        }
    }

    // 低层钩子单独跑一个线程：回调里**绝不做窗口操作**，只往通道里丢信号。
    // 主线程一旦忙起来，回调超时会被系统（LowLevelHooksTimeout）悄悄摘钩。
    std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(|| unsafe {
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(h) => {
                    HOOK_OK.store(true, Ordering::SeqCst);
                    // 钩子随进程存活，这里只是把句柄留着防止提前回收
                    let _ = h;
                }
                Err(_) => {
                    eprintln!("[hotkey] 安装键盘钩子失败");
                    HOOK_OK.store(false, Ordering::SeqCst);
                    return;
                }
            }
            // 低层钩子要求安装它的线程带消息循环，钩子回调就在这个线程里执行
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::{parse_binding, Binding, F_ALT, F_CTRL, F_SHIFT, F_WIN};
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    fn combo(spec: &str) -> (Modifiers, Code) {
        match parse_binding(spec) {
            Some(Binding::Combo(sc)) => (sc.mods, sc.key),
            other => panic!("{spec} 应该解析成组合键，实际是 {other:?}"),
        }
    }

    #[test]
    fn double_modifier_bindings() {
        assert_eq!(parse_binding("DoubleCtrl"), Some(Binding::Double(F_CTRL)));
        assert_eq!(parse_binding("DoubleCtrl"), parse_binding("Ctrl+Ctrl"));
        assert_eq!(parse_binding("DoubleAlt"), Some(Binding::Double(F_ALT)));
        assert_eq!(parse_binding("alt+alt"), Some(Binding::Double(F_ALT)));
        assert_eq!(
            parse_binding("DoubleShift"),
            Some(Binding::Double(F_SHIFT))
        );
        assert_eq!(parse_binding("DoubleWin"), Some(Binding::Double(F_WIN)));
    }

    #[test]
    fn combo_bindings() {
        let (mods, key) = combo("Ctrl+Q");
        assert!(mods.contains(Modifiers::CONTROL));
        assert_eq!(key, Code::KeyQ);

        let (mods, key) = combo("Ctrl+Alt+S");
        assert!(mods.contains(Modifiers::CONTROL | Modifiers::ALT));
        assert_eq!(key, Code::KeyS);

        let (mods, key) = combo("alt+space");
        assert_eq!(mods, Modifiers::ALT);
        assert_eq!(key, Code::Space);

        let (mods, key) = combo("Win+Q");
        assert!(mods.contains(Modifiers::SUPER));
        assert_eq!(key, Code::KeyQ);

        let (_, key) = combo("Ctrl+F11");
        assert_eq!(key, Code::F11);
    }

    #[test]
    fn disabled_and_invalid() {
        assert_eq!(parse_binding(""), Some(Binding::None));
        assert_eq!(parse_binding("None"), Some(Binding::None));
        // 裸键会把该键从整个系统拦下来，必须拒绝
        assert_eq!(parse_binding("F5"), None);
        assert_eq!(parse_binding("Ctrl"), None);
        assert_eq!(parse_binding("Ctrl+K+J"), None);
        assert_eq!(parse_binding("Ctrl+哈哈哈"), None);
    }
}
