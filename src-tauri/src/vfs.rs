//! 文件系统的「只读视图」：常用位置、磁盘容量、目录快照。
//!
//! 和索引无关，直接问 Windows 要现成的结果：
//! - 常用位置走 `SHGetKnownFolderPath`，桌面 / 下载被 OneDrive 重定向也能拿到真实路径；
//! - 容量走 `GetDiskFreeSpaceExW`；
//! - 目录内容走 `read_dir`（一屏几十条，几十微秒）。
//!
//! 首页就是这三个东西拼出来的：先磁盘，再桌面，最后每个盘的根目录。

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug)]
pub struct Place {
    pub key: String,
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Drive {
    pub letter: String,
    pub name: String,
    pub fs: String,
    pub kind: u32,
    pub total: u64,
    pub free: u64,
    pub used: u64,
    /// 已用百分比 0..100
    pub percent: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Item {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: i64,
    pub mtime: i64,
    pub ext: String,
}

impl Item {
    fn new(path: PathBuf, md: &std::fs::Metadata) -> Item {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = if md.is_dir() {
            String::new()
        } else {
            path.extension()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        };
        Item {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: md.is_dir(),
            size: md.len() as i64,
            mtime: md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            ext,
        }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Listing {
    pub path: String,
    pub items: Vec<Item>,
    /// 实际子项数（可能大于 items.len()）
    pub total: usize,
    pub truncated: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Home {
    pub places: Vec<Place>,
    pub drives: Vec<Drive>,
    pub desktop: Listing,
    /// 各磁盘根目录内容
    pub sections: Vec<Listing>,
}

/* ---------------------------- 常用位置 ---------------------------- */

#[cfg(windows)]
fn known(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::SHGetKnownFolderPath;
    unsafe {
        let p = SHGetKnownFolderPath(id, windows::Win32::UI::Shell::KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

pub fn places() -> Vec<Place> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
            FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos,
        };
        let defs: [(&str, &str, &windows::core::GUID); 7] = [
            ("home", "主目录", &FOLDERID_Profile),
            ("desktop", "桌面", &FOLDERID_Desktop),
            ("downloads", "下载", &FOLDERID_Downloads),
            ("documents", "文档", &FOLDERID_Documents),
            ("pictures", "图片", &FOLDERID_Pictures),
            ("music", "音乐", &FOLDERID_Music),
            ("videos", "视频", &FOLDERID_Videos),
        ];
        let mut out = Vec::new();
        for (key, name, guid) in defs {
            if let Some(p) = known(guid) {
                if p.exists() {
                    out.push(Place {
                        key: key.into(),
                        name: name.into(),
                        path: p.to_string_lossy().to_string(),
                    });
                }
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/* ---------------------------- 磁盘 ---------------------------- */

#[cfg(windows)]
pub fn drives() -> Vec<Drive> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
    };

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        let w = wide(&root);
        let kind = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
        // 0 = 未知, 1 = 无根目录；这两类不展示
        if kind <= 1 {
            continue;
        }
        let mut total: u64 = 0;
        let mut free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(w.as_ptr()),
                None,
                Some(&mut total as *mut u64),
                Some(&mut free as *mut u64),
            )
        }
        .is_ok();
        if !ok || total == 0 {
            continue;
        }
        let mut label = [0u16; 128];
        let mut fsname = [0u16; 32];
        let _ = unsafe {
            GetVolumeInformationW(
                PCWSTR(w.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut fsname),
            )
        };
        let trimmed = |b: &[u16]| {
            let n = b.iter().position(|c| *c == 0).unwrap_or(b.len());
            String::from_utf16_lossy(&b[..n])
        };
        let used = total.saturating_sub(free);
        out.push(Drive {
            letter: format!("{}:", letter as char),
            name: trimmed(&label),
            fs: trimmed(&fsname),
            kind,
            total,
            free,
            used,
            percent: if total > 0 {
                used as f64 * 100.0 / total as f64
            } else {
                0.0
            },
        });
    }
    out
}

#[cfg(not(windows))]
pub fn drives() -> Vec<Drive> {
    Vec::new()
}

/* ---------------------------- 目录快照 ---------------------------- */

#[cfg(windows)]
fn is_hidden(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_HIDDEN = 0x2, FILE_ATTRIBUTE_SYSTEM = 0x4
    md.file_attributes() & 0x6 != 0
}

#[cfg(not(windows))]
fn is_hidden(_md: &std::fs::Metadata) -> bool {
    false
}

/// 列出目录的直接子项（目录优先、按名字排序，跳过隐藏/系统项）。
pub fn list_dir(path: &str, limit: usize, skip_hidden: bool) -> Listing {
    let mut out = Listing { path: path.to_string(), ..Default::default() };
    let rd = match std::fs::read_dir(Path::new(path)) {
        Ok(v) => v,
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    };
    let mut items: Vec<Item> = Vec::new();
    for ent in rd.flatten() {
        let p = ent.path();
        let md = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if skip_hidden && is_hidden(&md) {
            continue;
        }
        items.push(Item::new(p, &md));
    }
    items.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.total = items.len();
    out.truncated = items.len() > limit;
    items.truncate(limit.max(1));
    out.items = items;
    out
}

/// 首页一次性取齐：磁盘 + 桌面 + 各盘根目录。
pub fn home(desktop_limit: usize, drive_limit: usize) -> Home {
    let places = places();
    let drives = drives();
    let desktop_path = places
        .iter()
        .find(|p| p.key == "desktop")
        .map(|p| p.path.clone())
        .unwrap_or_default();

    // 桌面与各盘根目录互不依赖，并行取，冷启动时省一半时间
    let mut desktop = Listing { path: desktop_path.clone(), ..Default::default() };
    let mut sections: Vec<Listing> = Vec::new();
    std::thread::scope(|s| {
        let dp = desktop_path.clone();
        let dh = s.spawn(move || {
            if dp.is_empty() {
                Listing::default()
            } else {
                list_dir(&dp, desktop_limit, true)
            }
        });
        let mut handles = Vec::new();
        for d in &drives {
            let root = if d.letter.is_empty() {
                String::new()
            } else {
                format!("{}\\", d.letter)
            };
            handles.push(s.spawn(move || list_dir(&root, drive_limit, true)));
        }
        desktop = dh.join().unwrap_or_default();
        sections = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    });

    Home { places, drives, desktop, sections }
}
