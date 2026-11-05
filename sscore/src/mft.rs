//! 直接读 NTFS 的 `$MFT`：`FSCTL_ENUM_USN_DATA` 枚举全卷记录。
//!
//! 只需要「卷句柄 + 枚举」两个系统调用，不需要解析 MFT 记录本身：
//! 枚举结果里已经带了 `FileReferenceNumber`、`ParentFileReferenceNumber`、
//! 文件名和属性位，正好是我们列式索引要的全部字段。
//!
//! 权限：打开卷句柄需要管理员（`CreateFileW` 返回错误 5）。
//! 非管理员走 `walk.rs` 的回退路径。

#![cfg(windows)]

use crate::RawRec;
use std::io;

type Handle = isize;

const INVALID_HANDLE_VALUE: Handle = -1;

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00B3;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

const ERROR_ACCESS_DENIED: i32 = 5;

extern "system" {
    fn CreateFileW(
        lpfilename: *const u16,
        dwdesiredaccess: u32,
        dwsharemode: u32,
        lpsecurityattributes: *const u8,
        dwcreationdisposition: u32,
        dwflagsandattributes: u32,
        htemplatefile: Handle,
    ) -> Handle;
    fn DeviceIoControl(
        hdevice: Handle,
        dwiocontrolcode: u32,
        lpinbuffer: *const u8,
        ninbuffersize: u32,
        lpoutbuffer: *mut u8,
        noutbuffersize: u32,
        lpbytesreturned: *mut u32,
        lpoverlapped: *mut u8,
    ) -> i32;
    fn CloseHandle(hobject: Handle) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 把 `C` / `C:` / `C:\` 统一成 `C`。
pub fn normalize_letter(v: &str) -> io::Result<char> {
    let c = v.chars().next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缺少盘符"))?;
    if !c.is_ascii_alphabetic() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("不是合法盘符: {v}")));
    }
    Ok(c.to_ascii_uppercase())
}

struct VolumeHandle(Handle);

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && self.0 != 0 {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// 枚举整卷。返回 (记录, 枚举耗时毫秒)。
pub fn enum_volume(letter: char) -> io::Result<(Vec<RawRec>, f64)> {
    let raw = format!("\\\\.\\{}:", letter);
    let path = wide(&raw);
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        let e = io::Error::last_os_error();
        let code = e.raw_os_error().unwrap_or(0);
        let hint = if code == ERROR_ACCESS_DENIED {
            "（打开卷句柄需要管理员权限；非管理员请用 walk 模式）"
        } else {
            ""
        };
        return Err(io::Error::new(e.kind(), format!("打开卷 {letter}: 失败: {e}{hint}")));
    }
    let vol = VolumeHandle(h);

    // MFT_ENUM_DATA_V0
    let mut med = [0u8; 24];
    med[0..8].copy_from_slice(&0u64.to_le_bytes()); // StartFileReferenceNumber
    med[8..16].copy_from_slice(&0i64.to_le_bytes()); // LowUsn
    med[16..24].copy_from_slice(&i64::MAX.to_le_bytes()); // HighUsn

    let mut buf = vec![0u8; 1 << 20];
    let mut out: Vec<RawRec> = Vec::with_capacity(1 << 20);
    let mut name_buf: Vec<u16> = Vec::with_capacity(1024);
    let t0 = std::time::Instant::now();

    loop {
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                vol.0,
                FSCTL_ENUM_USN_DATA,
                med.as_ptr(),
                med.len() as u32,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            // 枚举到末尾时部分驱动会返回 ERROR_HANDLE_EOF(38)，视为正常结束。
            if e.raw_os_error() == Some(38) {
                break;
            }
            return Err(io::Error::new(e.kind(), format!("FSCTL_ENUM_USN_DATA 失败: {e}")));
        }
        if returned < 8 {
            break;
        }
        let next = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let mut off = 8usize;
        let end = returned as usize;
        while off + 60 <= end {
            let rec_len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            if rec_len < 60 || off + rec_len > end {
                break;
            }
            let major = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap());
            if major == 2 {
                let frn = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap());
                let pfrn = u64::from_le_bytes(buf[off + 16..off + 24].try_into().unwrap());
                let attrs = u32::from_le_bytes(buf[off + 52..off + 56].try_into().unwrap());
                let name_len = u16::from_le_bytes(buf[off + 56..off + 58].try_into().unwrap()) as usize;
                let name_off = u16::from_le_bytes(buf[off + 58..off + 60].try_into().unwrap()) as usize;
                if name_off + name_len <= rec_len {
                    name_buf.clear();
                    let base = off + name_off;
                    for k in (0..name_len).step_by(2) {
                        name_buf.push(u16::from_le_bytes(buf[base + k..base + k + 2].try_into().unwrap()));
                    }
                    let key = frn & 0x0000_FFFF_FFFF_FFFF;
                    // 跳过 NTFS 自己的元文件（$MFT / $Bitmap …），它们 FRN 极小且带 $ 前缀。
                    let is_meta = key < 16
                        && name_buf.first().copied() == Some(b'$' as u16);
                    if !is_meta {
                        let name = String::from_utf16_lossy(&name_buf);
                        if !name.is_empty() && name != "." {
                            out.push(RawRec {
                                key,
                                parent_key: pfrn & 0x0000_FFFF_FFFF_FFFF,
                                name,
                                is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
                            });
                        }
                    }
                }
            }
            off += rec_len;
        }
        if next == 0 || next == u64::from_le_bytes(med[0..8].try_into().unwrap()) {
            break;
        }
        med[0..8].copy_from_slice(&next.to_le_bytes());
    }

    let secs = t0.elapsed().as_secs_f64() * 1000.0;
    Ok((out, secs))
}
