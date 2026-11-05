//! 全局呼出快捷键（WH_KEYBOARD_LL 低层键盘钩子）。
//!
//! 支持两类绑定，配置串由设置面板录制生成，运行时用 `apply()` 热更新、无需重启：
//!   * 双击修饰键：`DoubleCtrl` / `DoubleAlt` / `DoubleShift` / `DoubleWin`（也接受 `Ctrl+Ctrl` 写法）
//!   * 组合键：`Ctrl+Alt+S` / `Alt+Space` / `Win+Q` ……（必须带至少一个修饰键，避免裸键拦下全局输入）
//!
//! 钩子跑在独立线程的 GetMessage 循环上。回调里**绝不做窗口操作**：
//! tao 的窗口 API 要同步等主线程事件循环，一旦主线程正忙，回调就会超时
//! （Windows 的 LowLevelHooksTimeout 到点会把钩子悄悄摘掉）。回调只往通道里
//! 丢一个信号，真正的窗口操作由另一个线程去做。
//!
//! 窗口显隐一律走 tao、且放到主线程按序执行：本窗口是 `decorations: false`，
//! 如果只用原生 `SW_SHOW` 把它显示出来，tao 内部的 `WindowFlags::VISIBLE`
//! 仍停留在 false，之后任何窗口操作（`maximize()` 等）的 `apply_diff` 都会
//! 补一刀 `SW_HIDE` 并重设窗口样式 —— 表现就是「窗口连标题栏一起不见了」。
//! 原生 Win32 在这里只负责立即还原最小化与突破前台锁置前。

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// 钩子线程 → 窗口操作线程的信号通道
static TX: OnceCell<Sender<()>> = OnceCell::new();
static ENABLED: AtomicBool = AtomicBool::new(true);
/// 当前绑定（编码见 parse_binding），0 = 未绑定
static BINDING: AtomicU64 = AtomicU64::new(0);
/// 钩子是否真的装上了（杀软/权限可能让 SetWindowsHookExW 失败，界面要能看到）
static HOOK_OK: AtomicBool = AtomicBool::new(false);
/// 双击模式：上次按下目标键之后是否夹杂了别的键
static OTHER_KEY: AtomicBool = AtomicBool::new(false);
static DOUBLE_LAST: Mutex<Option<Instant>> = Mutex::new(None);
/// 组合键模式：当前按住的修饰键位掩码
static MODS: AtomicU32 = AtomicU32::new(0);
/// 组合键模式：当前按住的主键 vk（用来过滤长按自动重复）
static PRESSED_KEY: AtomicU32 = AtomicU32::new(0);
/// 钩子所在线程 id，用于给它的消息循环投递「重装」请求
static HOOK_TID: AtomicU32 = AtomicU32::new(0);
/// 仅在调试版、且设了 TINKER_HOTKEY_TEST 时才放行注入的按键，方便自动化验证
static ALLOW_INJECTED: AtomicBool = AtomicBool::new(false);

const DOUBLE_WINDOW: Duration = Duration::from_millis(420);
const LLKHF_INJECTED: u32 = 0x10;
/// 通知钩子线程补挂一次（杀软、系统超时等都可能让低层钩子静默失效）。
const WM_APP_REINSTALL_HOOK: u32 = 0x8001;

// ---- 修饰键位 / 双击族 ----
const M_CTRL: u32 = 1;
const M_ALT: u32 = 2;
const M_SHIFT: u32 = 4;
const M_WIN: u32 = 8;
const F_CTRL: u64 = 1;
const F_ALT: u64 = 2;
const F_SHIFT: u64 = 3;
const F_WIN: u64 = 4;
/// BINDING 最高位 = 双击模式；低 8 位存族 id。
const DOUBLE_FLAG: u64 = 1 << 63;
/// 组合键模式：低 16 位主键 vk，16..19 位修饰键掩码。
const MODS_SHIFT: u32 = 16;

// 低层键盘钩子报告的是物理键：左右 Ctrl 是 0xA2/0xA3，不是通用 VK_CONTROL(0x11)。
// 三种写法都要认，否则真机上热键会完全失灵。
const VK_SHIFT_G: u32 = 0x10;
const VK_CONTROL_G: u32 = 0x11;
const VK_MENU_G: u32 = 0x12;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

/// 虚拟键 → (修饰键掩码, 双击族 id)
fn modifier(vk: u32) -> Option<(u32, u64)> {
    match vk {
        VK_SHIFT_G | VK_LSHIFT | VK_RSHIFT => Some((M_SHIFT, F_SHIFT)),
        VK_CONTROL_G | VK_LCONTROL | VK_RCONTROL => Some((M_CTRL, F_CTRL)),
        VK_MENU_G | VK_LMENU | VK_RMENU => Some((M_ALT, F_ALT)),
        VK_LWIN | VK_RWIN => Some((M_WIN, F_WIN)),
        _ => None,
    }
}

fn mod_name(s: &str) -> Option<(u32, u64)> {
    match s.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some((M_CTRL, F_CTRL)),
        "alt" => Some((M_ALT, F_ALT)),
        "shift" => Some((M_SHIFT, F_SHIFT)),
        "win" | "meta" | "super" => Some((M_WIN, F_WIN)),
        _ => None,
    }
}

/// 主键名 → 虚拟键码。设置面板录制出来的就是这些名字。
fn key_vk(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.len() == 1 {
        let c = s.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    let u = s.to_ascii_uppercase();
    if let Some(n) = u.strip_prefix('F') {
        if let Ok(i) = n.parse::<u32>() {
            if (1..=24).contains(&i) {
                return Some(0x70 + i - 1);
            }
        }
    }
    if let Some(n) = u.strip_prefix("NUMPAD") {
        if let Ok(i) = n.parse::<u32>() {
            if i <= 9 {
                return Some(0x60 + i);
            }
        }
    }
    Some(match u.as_str() {
        "SPACE" | "空格" => 0x20,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "INSERT" => 0x2D,
        "DELETE" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "UP" | "ARROWUP" => 0x26,
        "DOWN" | "ARROWDOWN" => 0x28,
        "LEFT" | "ARROWLEFT" => 0x25,
        "RIGHT" | "ARROWRIGHT" => 0x27,
        "PRINTSCREEN" | "PRTSC" => 0x2C,
        "SCROLLLOCK" => 0x91,
        "PAUSE" => 0x13,
        "-" | "MINUS" => 0xBD,
        "=" | "EQUAL" => 0xBB,
        "[" | "BRACKETLEFT" => 0xDB,
        "]" | "BRACKETRIGHT" => 0xDD,
        ";" | "SEMICOLON" => 0xBA,
        "'" | "QUOTE" => 0xDE,
        "," | "COMMA" => 0xBC,
        "." | "PERIOD" => 0xBE,
        "/" | "SLASH" => 0xBF,
        "\\" | "BACKSLASH" => 0xDC,
        "`" | "BACKQUOTE" => 0xC0,
        _ => return None,
    })
}

/// 把配置串解析成一个可放进 AtomicU64 的绑定。
/// 返回 None 表示格式非法；空串 / None 解析成 0（禁用）。
fn parse_binding(spec: &str) -> Option<u64> {
    let s = spec.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Some(0);
    }

    // DoubleCtrl / DoubleAlt / …
    let dbl: Option<u64> = match s.to_ascii_lowercase().as_str() {
        "doublectrl" | "ctrl+ctrl" => Some(F_CTRL),
        "doublealt" | "alt+alt" => Some(F_ALT),
        "doubleshift" | "shift+shift" => Some(F_SHIFT),
        "doublewin" | "win+win" => Some(F_WIN),
        _ => None,
    };
    if let Some(f) = dbl {
        return Some(DOUBLE_FLAG | f);
    }

    // 也接受任意修饰键的 X+X 写法（Ctrl+Ctrl 上面已覆盖）
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.len() == 2 && parts[0].eq_ignore_ascii_case(parts[1]) {
        if let Some((_, f)) = mod_name(parts[0]) {
            return Some(DOUBLE_FLAG | f);
        }
    }
    if parts.len() < 2 {
        return None;
    }

    let mut mods = 0u32;
    let mut key: Option<u32> = None;
    for p in parts {
        if let Some((m, _)) = mod_name(p) {
            mods |= m;
        } else {
            if key.is_some() {
                return None; // 只认一个主键
            }
            key = key_vk(p);
        }
    }
    let vk = key?;
    // 裸键会把该键从整个系统拦下来，必须带修饰键
    if mods == 0 || modifier(vk).is_some() {
        return None;
    }
    Some((mods as u64) << MODS_SHIFT | vk as u64)
}

/// 应用设置面板里的快捷键配置，立即生效，返回是否解析成功。
pub fn apply(spec: &str, enabled: bool) -> bool {
    let Some(b) = parse_binding(spec) else {
        return false;
    };
    BINDING.store(b, Ordering::SeqCst);
    ENABLED.store(enabled, Ordering::SeqCst);
    reset_state();
    true
}

/// 只开关、不改绑定（兼容旧的 toggle_hotkey 调用）
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    reset_state();
}

fn reset_state() {
    *DOUBLE_LAST.lock() = None;
    OTHER_KEY.store(false, Ordering::SeqCst);
    MODS.store(0, Ordering::SeqCst);
    PRESSED_KEY.store(0, Ordering::SeqCst);
}

/// 请钩子线程重新挂一次钩子。PostThreadMessage 是异步的，不会阻塞调用方。
fn request_hook_reinstall() {
    let tid = HOOK_TID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                tid,
                WM_APP_REINSTALL_HOOK,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn fire() {
    if let Some(tx) = TX.get() {
        let _ = tx.send(());
    }
}

/// 双击修饰键判定。只有连续两次按同一族、中间没夹别的键才算。
fn on_double_down(vk: u32, fam: u64) {
    if !modifier(vk).map(|(_, f)| f == fam).unwrap_or(false) {
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

/// 原生窗口句柄。tauri 的 HWND 来自它自己的 windows 版本，这里只借指针值。
#[cfg(windows)]
fn native_hwnd(w: &tauri::WebviewWindow) -> Option<HWND> {
    w.hwnd()
        .ok()
        .map(|h| HWND(h.0 as isize as *mut core::ffi::c_void))
}

/// 立即还原最小化并突破前台锁置前。
/// 不改窗口样式、不碰 tao 的显隐状态——那部分交给主线程里的 show/unminimize。
#[cfg(windows)]
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
        for _ in 0..2 {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            if GetForegroundWindow() == hwnd {
                break;
            }
            SwitchToThisWindow(hwnd, true);
            if GetForegroundWindow() == hwnd {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // 兜底：万一钩子被其它原因摘掉，请钩子线程补挂一次
        request_hook_reinstall();
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
    #[cfg(windows)]
    if let Some(hwnd) = native_hwnd(w) {
        // 已在窗口线程（启动时、托盘/单实例回调）：直接同步做，
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

    // 非 Windows / 拿不到原生句柄时的回退路径
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
    if code >= 0 {
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if down || up {
            let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let injected = kb.flags.0 & LLKHF_INJECTED != 0;
            if (!injected || ALLOW_INJECTED.load(Ordering::Relaxed))
                && ENABLED.load(Ordering::Relaxed)
            {
                let b = BINDING.load(Ordering::Relaxed);
                if b != 0 {
                    let vk = kb.vkCode;
                    if b & DOUBLE_FLAG != 0 {
                        if down {
                            on_double_down(vk, b & 0xFF);
                        }
                    } else if let Some((m, _)) = modifier(vk) {
                        if down {
                            MODS.fetch_or(m, Ordering::Relaxed);
                        } else {
                            MODS.fetch_and(!m, Ordering::Relaxed);
                        }
                    } else if down {
                        let target_mods = ((b >> MODS_SHIFT) & 0xF) as u32;
                        let target_vk = (b & 0xFFFF) as u32;
                        if vk == target_vk
                            && MODS.load(Ordering::Relaxed) & 0xF == target_mods
                            && PRESSED_KEY.swap(vk, Ordering::Relaxed) != vk
                        {
                            fire();
                        }
                    } else {
                        PRESSED_KEY.store(0, Ordering::Relaxed);
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 低层键盘钩子是否已正常工作
pub fn hook_ok() -> bool {
    HOOK_OK.load(Ordering::SeqCst)
}

pub fn install(app: AppHandle) {
    if cfg!(debug_assertions) && std::env::var_os("TINKER_HOTKEY_TEST").is_some() {
        ALLOW_INJECTED.store(true, Ordering::SeqCst);
    }

    // 窗口操作线程：收到信号再做还原/置前，不阻塞钩子回调
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

    std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(|| unsafe {
            let mut hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0);
            if hook.is_err() {
                eprintln!("[hotkey] 安装键盘钩子失败");
                return;
            }
            HOOK_OK.store(true, Ordering::SeqCst);
            HOOK_TID.store(
                windows::Win32::System::Threading::GetCurrentThreadId(),
                Ordering::SeqCst,
            );
            let mut msg: MSG = std::mem::zeroed();
            // 低层钩子要求线程带消息循环
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_APP_REINSTALL_HOOK {
                    // 摘掉旧钩子再挂新的：窗口操作后系统可能已把它静默失效
                    if let Ok(h) = &hook {
                        let _ = UnhookWindowsHookEx(*h);
                    }
                    hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0);
                    HOOK_OK.store(hook.is_ok(), Ordering::SeqCst);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::parse_binding;

    #[test]
    fn double_modifier_bindings() {
        assert!(parse_binding("DoubleCtrl").unwrap() & (1 << 63) != 0);
        assert_eq!(parse_binding("DoubleCtrl"), parse_binding("Ctrl+Ctrl"));
        assert_eq!(parse_binding("DoubleAlt"), parse_binding("alt+alt"));
        assert!(parse_binding("DoubleShift").unwrap() & (1 << 63) != 0);
        assert!(parse_binding("DoubleWin").unwrap() & (1 << 63) != 0);
    }

    #[test]
    fn combo_bindings() {
        let c = parse_binding("Ctrl+Alt+S").unwrap();
        assert_eq!(c & 0xFFFF, b'S' as u64);
        assert_eq!((c >> 16) & 0xF, 1 | 2);
        let a = parse_binding("alt+space").unwrap();
        assert_eq!(a & 0xFFFF, 0x20);
        assert_eq!((a >> 16) & 0xF, 2);
        assert_eq!(parse_binding("Win+F1").unwrap() & 0xFFFF, 0x70);
    }

    #[test]
    fn disabled_and_invalid() {
        assert_eq!(parse_binding(""), Some(0));
        assert_eq!(parse_binding("None"), Some(0));
        // 裸键会把该键从整个系统拦下来，必须拒绝
        assert_eq!(parse_binding("F5"), None);
        assert_eq!(parse_binding("Ctrl"), None);
        assert_eq!(parse_binding("Ctrl+K+J"), None);
        assert_eq!(parse_binding("Ctrl+哈哈哈"), None);
    }
}
