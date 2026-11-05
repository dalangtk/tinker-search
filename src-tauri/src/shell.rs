//! Windows 外壳集成：打开 / 定位 / 属性 / 回收站 / 剪贴板 / 提权。

#![allow(non_snake_case)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::{
    ShellExecuteW, SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    FO_DELETE, SHFILEOPSTRUCTW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

pub fn is_admin() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// 以管理员身份重新启动当前程序（UAC）。
/// args 会原样传给新实例，用 `--elevated` 让新实例跳过单实例检查，
/// 避免「提权进程刚起来就被旧实例的单实例锁顶掉」。
pub fn relaunch_elevated(args: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = wide(&exe.to_string_lossy());
    let verb = wide("runas");
    let params = wide(args);
    unsafe {
        let h = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            if args.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(params.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        // ShellExecuteW 返回值 > 32 表示成功
        if h.0 as usize <= 32 {
            return Err("提权被取消或失败".into());
        }
    }
    Ok(())
}

fn shell_open(path: &str) -> Result<(), String> {
    let file = wide(path);
    let verb = wide("open");
    unsafe {
        let h = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if h.0 as usize <= 32 {
            return Err("无法打开该文件（可能没有关联程序）".into());
        }
    }
    Ok(())
}

/// 解析文件系统快捷方式（.lnk）指向的真实路径。
///
/// 提权进程直接 `ShellExecuteW` 打开 .lnk 时，可能因为关联项/UAC 被判成
/// 「没有关联程序」；先拿到目标再决定怎么打开更稳。只认文件系统目标。
pub fn resolve_shortcut(path: &str) -> Option<std::path::PathBuf> {
    use windows::core::Interface;
    use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    let p = std::path::Path::new(path);
    if !p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        return None;
    }
    let link_w = wide(&p.to_string_lossy());

    unsafe {
        // 调用线程可能已经初始化过 COM；RPC_E_CHANGED_MODE 时沿用现有套间。
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let owned = hr.is_ok();
        let target = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let file: IPersistFile = link.cast().ok()?;
            file.Load(PCWSTR(link_w.as_ptr()), STGM_READ).ok()?;
            let mut buf = [0u16; 1024];
            let mut find: WIN32_FIND_DATAW = std::mem::zeroed();
            link.GetPath(&mut buf, &mut find, 0).ok()?;
            let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            if len == 0 {
                return None;
            }
            Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len])))
        })();
        if owned {
            CoUninitialize();
        }
        target.filter(|t| t.exists())
    }
}

pub fn open_path(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let resolved = resolve_shortcut(path);

    // 指向目录的快捷方式：直接打开目标目录，绕开 .lnk 的关联解析
    if let Some(dir) = resolved.as_deref().filter(|t| t.is_dir()) {
        return shell_open(&dir.to_string_lossy());
    }
    if !p.exists() {
        return Err(format!("路径不存在：{path}"));
    }
    match shell_open(path) {
        Ok(()) => Ok(()),
        // 快捷方式本身打不开时，退回它指向的目标
        Err(e) => match resolved {
            Some(t) if t.exists() => shell_open(&t.to_string_lossy()),
            _ => Err(e),
        },
    }
}

pub fn reveal(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let target = if p.is_dir() {
        path.to_string()
    } else {
        format!("/select,\"{path}\"")
    };
    let mut cmd = std::process::Command::new("explorer.exe");
    if p.is_dir() {
        cmd.arg(&target);
    } else {
        cmd.arg("/select,");
        cmd.arg(path);
    }
    // explorer 成功时也常返回非 0，忽略返回码
    let _ = cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn properties(path: &str) -> Result<(), String> {
    let file = wide(path);
    let verb = wide("properties");
    unsafe {
        let h = ShellExecuteW(
            HWND(std::ptr::null_mut()),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if h.0 as usize <= 32 {
            return Err("无法打开属性对话框".into());
        }
    }
    Ok(())
}

/// 删除到回收站（可撤销），不做永久删除
pub fn recycle(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut from: Vec<u16> = Vec::new();
    for p in paths {
        from.extend(OsStr::new(p).encode_wide());
        from.push(0);
    }
    from.push(0); // 双 NUL 结尾

    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE as u32;
    op.pFrom = PCWSTR(from.as_ptr());
    op.fFlags =
        (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16;
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        return Err(format!("删除失败（代码 {rc}）"));
    }
    Ok(())
}

pub fn rename(path: &str, new_name: &str) -> Result<String, String> {
    let p = std::path::Path::new(path);
    let parent = p.parent().ok_or("无法定位父目录")?;
    let target = parent.join(new_name);
    if target.exists() {
        return Err("同名文件已存在".into());
    }
    std::fs::rename(p, &target).map_err(|e| e.to_string())?;
    Ok(target.to_string_lossy().to_string())
}

/// 把文件以 CF_HDROP 形式放进剪贴板，可在资源管理器中直接粘贴
pub fn copy_files_to_clipboard(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // DROPFILES 结构：pFiles(4) + pt(8) + fNC(4) + fWide(4)，指针 8 字节对齐后接宽字符列表
    const HEADER: usize = 20;
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(OsStr::new(p).encode_wide());
        list.push(0);
    }
    list.push(0);
    let bytes = HEADER + list.len() * 2;

    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            return Err("剪贴板内存分配失败".into());
        }
        std::ptr::write_bytes(ptr, 0, bytes);
        // pFiles = 20
        std::ptr::write_unaligned(ptr as *mut u32, HEADER as u32);
        // fWide = TRUE
        std::ptr::write_unaligned(ptr.add(16) as *mut i32, 1);
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, ptr.add(HEADER), list.len() * 2);
        let _ = GlobalUnlock(hmem);

        OpenClipboard(HWND(std::ptr::null_mut())).map_err(|e| e.to_string())?;
        let _ = EmptyClipboard();
        if SetClipboardData(CF_HDROP.0 as u32, HANDLE(hmem.0)).is_err() {
            let _ = CloseClipboard();
            return Err("写入剪贴板失败".into());
        }
        let _ = CloseClipboard();
    }
    Ok(())
}

pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    // 使用 PowerShell 之外的轻量方式：直接走 Win32 CF_UNICODETEXT
    const CF_UNICODETEXT: u32 = 13;
    let mut data: Vec<u16> = OsStr::new(text).encode_wide().collect();
    data.push(0);
    let bytes = data.len() * 2;
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            return Err("剪贴板内存分配失败".into());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, ptr, bytes);
        let _ = GlobalUnlock(hmem);
        OpenClipboard(HWND(std::ptr::null_mut())).map_err(|e| e.to_string())?;
        let _ = EmptyClipboard();
        let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0));
        let _ = CloseClipboard();
    }
    Ok(())
}

/// 打开任意目录（用于「打开配置目录」「打开索引目录」）
pub fn open_dir(path: &str) {
    let _ = std::fs::create_dir_all(path);
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

/// 注册 / 注销 tinker:// 协议（写 HKCU，不需要管理员权限）
/// tinker:// 目前是否已注册（直接看注册表，不信配置）
pub fn url_protocol_registered() -> bool {
    use windows::Win32::System::Registry::{RegOpenKeyExW, RegCloseKey, HKEY_CURRENT_USER, KEY_READ};
    let sub: Vec<u16> = OsStr::new(r"Software\Classes\tinker\shell\open\command")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut key = windows::Win32::System::Registry::HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            KEY_READ,
            &mut key,
        )
    };
    if rc.is_err() {
        return false;
    }
    let _ = unsafe { RegCloseKey(key) };
    true
}

/// 注册/注销 tinker:// 协议（只动 HKCU，不需要管理员）。
/// 直接写注册表而不用 reg.exe：少一个进程、不会闪黑框，也不受外部命令策略影响。
pub fn register_url_protocol(enable: bool) -> Result<(), String> {
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    const ROOT: &str = r"Software\Classes\tinker";

    fn wide_z(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = OsStr::new(s).encode_wide().collect();
        v.push(0);
        v
    }

    /// 打开（不存在则创建）子键
    fn open_key(path: &str) -> Result<HKEY, String> {
        let sub = wide_z(path);
        let mut key = HKEY::default();
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if rc.is_err() {
            return Err(format!("创建注册表项失败：{}（{}）", rc.0, path));
        }
        Ok(key)
    }

    /// 写字符串值
    fn set_str(key: HKEY, name: Option<&str>, value: &str) -> Result<(), String> {
        let name_w = name.map(wide_z);
        let data = wide_z(value);
        let bytes = unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * std::mem::size_of::<u16>())
        };
        let rc = unsafe {
            RegSetValueExW(
                key,
                name_w
                    .as_ref()
                    .map(|v| PCWSTR(v.as_ptr()))
                    .unwrap_or(PCWSTR::null()),
                0,
                REG_SZ,
                Some(bytes),
            )
        };
        rc.ok().map_err(|e| format!("写入注册表值失败：{}", e.code().0))
    }

    if !enable {
        let sub = wide_z(ROOT);
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr())) }
            .ok()
            .map_err(|e| format!("注销协议失败：{}", e.code().0))?;
        return Ok(());
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;

    let k = open_key(ROOT)?;
    let r1 = set_str(k, None, "URL:tinker Protocol");
    let r2 = set_str(k, Some("URL Protocol"), "");
    let _ = unsafe { RegCloseKey(k) };
    r1?;
    r2?;

    // 默认图标：让资源管理器右键菜单/浏览器提示带上程序图标
    let icon = open_key(&format!("{ROOT}\\DefaultIcon"))?;
    let r3 = set_str(icon, None, &format!("\"{}\",0", exe.to_string_lossy()));
    let _ = unsafe { RegCloseKey(icon) };
    r3?;

    let cmd = open_key(&format!("{ROOT}\\shell\\open\\command"))?;
    let r4 = set_str(cmd, None, &format!("\"{}\" \"%1\"", exe.to_string_lossy()));
    let _ = unsafe { RegCloseKey(cmd) };
    r4?;

    Ok(())
}
