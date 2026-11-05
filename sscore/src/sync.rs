//! NTFS USN 变更日志实时同步。
//!
//! 为什么用 USN 而不是 `ReadDirectoryChangesW`：
//! - USN 是**卷级**的，一个卷只要一个句柄；目录监视要给每个目录开句柄，
//!   C 盘几十万目录，句柄数和内存都撑不住，而且缓冲区溢出会静默丢事件；
//! - USN 是**持久化**的，进程退出期间的变更也能补读（只要日志没被回绕）；
//! - USN 由内核顺序写入，读取是纯顺序 I/O，几乎不打扰正常业务。
//!
//! 断点信息（journal id 与 USN 位置）随索引一起落盘，重启后可以续读。
//!
//! 权限：打开卷句柄需要管理员。非管理员建的索引（walk 模式）没有 FRN 列，同步不可用。

#![cfg(windows)]

use crate::store::{Index, ROOT_PARENT};
use std::collections::HashMap;
use std::io;

type Handle = isize;

const INVALID_HANDLE_VALUE: Handle = -1;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
const OPEN_EXISTING: u32 = 3;

const FSCTL_CREATE_USN_JOURNAL: u32 = 0x0009_00E7;
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;

// USN_REASON_* 位
pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;

/// 我们只关心「名字 / 存在性」发生变化的四类原因。
///
/// 这个掩码交给内核做过滤，写文件产生的 `DATA_EXTEND` / `DATA_OVERWRITE`
/// 根本不会进到我们的缓冲区——这是同步开销能做低的关键一步。
/// 如果订阅全部原因，一次几 GB 的下载会灌进来几十万条无用记录。
pub const REASON_MASK: u32 =
    USN_REASON_FILE_CREATE | USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME | USN_REASON_RENAME_NEW_NAME;

const ERROR_JOURNAL_DELETE_IN_PROGRESS: i32 = 1178;
const ERROR_JOURNAL_NOT_ACTIVE: i32 = 1179;
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

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.to_string())
}

/// 一条 USN 记录解析后的形态。
#[derive(Clone, Debug)]
pub struct UsnEvent {
    pub frn: u64,
    pub parent_frn: u64,
    pub reason: u32,
    pub attrs: u32,
    pub name: String,
}

impl UsnEvent {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.attrs & 0x10 != 0
    }
}

/// 卷的 USN 日志句柄 + 断点位置。
pub struct Journal {
    handle: Handle,
    pub letter: char,
    pub journal_id: u64,
    /// 下次从这里开始读。落盘保存后，重启就能从断点续读。
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    buf: Vec<u8>,
}

impl Drop for Journal {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE && self.handle != 0 {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

impl Journal {
    /// 打开卷的 USN 日志。日志没启用时会尝试用系统默认大小创建。
    pub fn open(letter: char, buf_bytes: usize) -> io::Result<Journal> {
        let raw = format!("\\\\.\\{}:", letter);
        let path = wide(&raw);
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            let hint = if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                "（USN 同步需要打开卷句柄，必须管理员权限）"
            } else {
                ""
            };
            return Err(err(&format!("打开卷 {letter}: 失败: {e}{hint}")));
        }
        let mut j = Journal {
            handle: h,
            letter,
            journal_id: 0,
            next_usn: 0,
            lowest_valid_usn: 0,
            buf: vec![0u8; buf_bytes],
        };

        match j.query() {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(ERROR_JOURNAL_NOT_ACTIVE) => {
                // CREATE_USN_JOURNAL_DATA { MaximumSize, AllocationDelta }，传 0 用默认值。
                let input = [0u8; 16];
                let mut ret = 0u32;
                let ok = unsafe {
                    DeviceIoControl(
                        j.handle,
                        FSCTL_CREATE_USN_JOURNAL,
                        input.as_ptr(),
                        16,
                        std::ptr::null_mut(),
                        0,
                        &mut ret,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    let e2 = io::Error::last_os_error();
                    return Err(err(&format!("卷 {letter}: 的 USN 日志不存在且创建失败: {e2}")));
                }
                j.query()?;
            }
            Err(e) => return Err(e),
        }
        Ok(j)
    }

    fn query(&mut self) -> io::Result<()> {
        // USN_JOURNAL_DATA_V0：56 字节
        let mut out = [0u8; 64];
        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                out.as_mut_ptr(),
                out.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        self.journal_id = u64::from_le_bytes(out[0..8].try_into().unwrap());
        // FirstUsn @8 / NextUsn @16 / LowestValidUsn @24
        self.next_usn = i64::from_le_bytes(out[16..24].try_into().unwrap());
        self.lowest_valid_usn = i64::from_le_bytes(out[24..32].try_into().unwrap());
        Ok(())
    }

    /// 把读位置手动拨到某个 USN（断点续读）。
    pub fn seek(&mut self, usn: i64) {
        self.next_usn = usn;
    }

    /// 阻塞等待并读取一批。`wait_bytes` 为 0 表示不等待立即返回。
    ///
    /// 返回解析后的事件；返回的 `usn_advanced` 表示读位置有没有前进。
    pub fn read(&mut self, wait_bytes: u64, timeout_ms: u64) -> io::Result<(Vec<UsnEvent>, bool)> {
        let mut inp = [0u8; 40];
        inp[0..8].copy_from_slice(&self.next_usn.to_le_bytes());
        inp[8..12].copy_from_slice(&REASON_MASK.to_le_bytes());
        inp[12..16].copy_from_slice(&0u32.to_le_bytes()); // ReturnOnlyOnClose = false
        inp[16..24].copy_from_slice(&(timeout_ms * 10_000).to_le_bytes()); // 100ns 单位
        inp[24..32].copy_from_slice(&wait_bytes.to_le_bytes());
        inp[32..40].copy_from_slice(&self.journal_id.to_le_bytes());

        let mut ret = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_READ_USN_JOURNAL,
                inp.as_ptr(),
                40,
                self.buf.as_mut_ptr(),
                self.buf.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if ret < 8 {
            return Ok((Vec::new(), false));
        }

        let next = u64::from_le_bytes(self.buf[0..8].try_into().unwrap());
        let mut advanced = false;
        if next != 0 && next as i64 != self.next_usn {
            self.next_usn = next as i64;
            advanced = true;
        }

        let end = ret as usize;
        let mut off = 8usize;
        let mut events: Vec<UsnEvent> = Vec::new();
        while off + 60 <= end {
            let rec_len = u32::from_le_bytes(self.buf[off..off + 4].try_into().unwrap()) as usize;
            if rec_len < 60 || off + rec_len > end {
                break;
            }
            let major = u16::from_le_bytes(self.buf[off + 4..off + 6].try_into().unwrap());
            // V2 是当前唯一常见格式；V3/V4 用 128 位 FRN，遇到就跳过。
            if major == 2 {
                let frn = u64::from_le_bytes(self.buf[off + 8..off + 16].try_into().unwrap());
                let pfrn = u64::from_le_bytes(self.buf[off + 16..off + 24].try_into().unwrap());
                let reason = u32::from_le_bytes(self.buf[off + 40..off + 44].try_into().unwrap());
                let attrs = u32::from_le_bytes(self.buf[off + 52..off + 56].try_into().unwrap());
                let nlen = u16::from_le_bytes(self.buf[off + 56..off + 58].try_into().unwrap()) as usize;
                let noff = u16::from_le_bytes(self.buf[off + 58..off + 60].try_into().unwrap()) as usize;
                if noff + nlen <= rec_len && reason != 0 {
                    let base = off + noff;
                    let units: Vec<u16> = (0..nlen)
                        .step_by(2)
                        .map(|k| u16::from_le_bytes([self.buf[base + k], self.buf[base + k + 1]]))
                        .collect();
                    let name = String::from_utf16_lossy(&units);
                    if !name.is_empty() && name != "." {
                        events.push(UsnEvent {
                            // 只留低 48 位：和 MFT 枚举里的 key 口径保持一致。
                            frn: frn & 0x0000_FFFF_FFFF_FFFF,
                            parent_frn: pfrn & 0x0000_FFFF_FFFF_FFFF,
                            reason,
                            attrs,
                            name,
                        });
                    }
                }
            }
            off += rec_len;
        }
        Ok((events, advanced))
    }

    /// 日志被删掉/重建时，读会直接报错；上层据此触发全量重建。
    pub fn is_journal_lost(e: &io::Error) -> bool {
        matches!(
            e.raw_os_error(),
            Some(ERROR_JOURNAL_NOT_ACTIVE) | Some(ERROR_JOURNAL_DELETE_IN_PROGRESS)
        )
    }
}

/// FRN → 索引下标的查找表。
///
/// 主表是按 FRN 升序的 `(frn, id)` 数组（MFT 号本身稀疏但基本递增，
/// 排序后二分 22 次即可）；同步期间新增的记录先进 `overlay`，
/// 攒够一批再合并回主表，避免每次插入都搬动大数组。
pub struct FrnIndex {
    keys: Vec<(u64, u32)>,
    overlay: HashMap<u64, u32>,
}

impl FrnIndex {
    pub fn build(idx: &Index) -> FrnIndex {
        let mut keys: Vec<(u64, u32)> = Vec::with_capacity(idx.len());
        for (i, frn) in idx.frn.iter().enumerate() {
            if *frn != 0 {
                keys.push((*frn, i as u32));
            }
        }
        keys.sort_unstable_by_key(|(f, _)| *f);
        keys.dedup_by_key(|(f, _)| *f);
        FrnIndex { keys, overlay: HashMap::new() }
    }

    #[inline]
    pub fn get(&self, frn: u64) -> Option<u32> {
        if let Some(id) = self.overlay.get(&frn) {
            return Some(*id);
        }
        self.keys.binary_search_by_key(&frn, |(f, _)| *f).ok().map(|i| self.keys[i].1)
    }

    pub fn insert(&mut self, frn: u64, id: u32) {
        self.overlay.insert(frn, id);
    }

    pub fn len(&self) -> usize {
        self.keys.len() + self.overlay.len()
    }

    #[inline]
    pub fn keys_len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 把 overlay 合并进主表。overlay 大到一定程度调一次即可，
    /// 合并是 O(n log n)，别在每条事件上都做。
    pub fn flush(&mut self) {
        if self.overlay.is_empty() {
            return;
        }
        self.keys.reserve(self.overlay.len());
        for (f, id) in self.overlay.drain() {
            self.keys.push((f, id));
        }
        self.keys.sort_unstable_by_key(|(f, _)| *f);
        self.keys.dedup_by_key(|(f, _)| *f);
    }
}

/// 同步统计，用于评估开销。
#[derive(Default, Clone)]
pub struct SyncStats {
    pub rounds: u64,
    pub events: u64,
    pub added: u64,
    pub removed: u64,
    pub renamed: u64,
    pub revived: u64,
    /// 上一轮处理耗时（微秒），空闲时通常是几微秒。
    pub last_round_us: f64,
    /// 累计花在同步处理上的时间（毫秒）。
    pub busy_ms: f64,
    pub last_journal_id: u64,
    pub last_usn: i64,
}

impl SyncStats {
    pub fn summary(&self) -> String {
        format!(
            "轮次 {} / 事件 {} (增 {} 删 {} 改 {} 复活 {}) / 上轮 {:.1}µs / 累计 {:.1}ms",
            self.rounds,
            self.events,
            self.added,
            self.removed,
            self.renamed,
            self.revived,
            self.last_round_us,
            self.busy_ms
        )
    }
}

/// 把 USN 事件应用到索引上。
pub struct Syncer {
    pub journal: Journal,
    frn: FrnIndex,
    vol: u16,
    pub stats: SyncStats,
}

impl Syncer {
    /// 给已有索引挂上同步。要求索引带 FRN 列（即 MFT 建的库）。
    pub fn attach(idx: &mut Index, letter: char) -> io::Result<Syncer> {
        if !idx.has_frn() {
            return Err(err("这个索引没有 FRN 列（多半是 walk 建的），不支持实时同步；请用 mft 模式重建"));
        }
        let vol = idx.ensure_volume(&format!("{}:", letter.to_ascii_uppercase()));
        let journal = Journal::open(letter, 4 << 20)?;
        let frn = FrnIndex::build(idx);
        Ok(Syncer {
            journal,
            frn,
            vol,
            stats: SyncStats { last_journal_id: 0, ..Default::default() },
        })
    }

    /// 从保存的断点恢复：如果日志还是同一个、断点仍在有效区间，就续读；
    /// 否则返回 false，调用方应全量重建。
    pub fn resume_from(&mut self, saved_id: u64, saved_usn: i64) -> bool {
        if saved_id != 0 && saved_id == self.journal.journal_id && saved_usn >= self.journal.lowest_valid_usn {
            self.journal.seek(saved_usn);
            true
        } else {
            false
        }
    }

    /// 把读位置推到日志末尾（新建索引时用：历史变更已经体现在快照里了）。
    pub fn skip_to_end(&mut self) {
        // open() 已把读位置设在当时的 NextUsn；这里再查一次，
        // 把「建库快照」到「同步起点」之间新增的事件也跳过，不留缝。
        let _ = self.journal.query();
    }

    /// 等一批事件并应用。返回本轮处理的事件数。
    pub fn pump(&mut self, idx: &mut Index, wait_bytes: u64, timeout_ms: u64) -> io::Result<usize> {
        let t0 = std::time::Instant::now();
        let (events, advanced) = self.journal.read(wait_bytes, timeout_ms)?;
        if !advanced && events.is_empty() {
            self.stats.rounds += 1;
            self.stats.last_round_us = t0.elapsed().as_secs_f64() * 1e6;
            self.stats.busy_ms += self.stats.last_round_us / 1000.0;
            return Ok(0);
        }
        let n = events.len();
        for ev in &events {
            self.apply(idx, ev);
        }
        self.stats.rounds += 1;
        self.stats.events += n as u64;
        self.stats.last_round_us = t0.elapsed().as_secs_f64() * 1e6;
        self.stats.busy_ms += self.stats.last_round_us / 1000.0;
        self.stats.last_journal_id = self.journal.journal_id;
        self.stats.last_usn = self.journal.next_usn;
        if self.frn.len() > self.frn.keys_len() + 65_536 {
            self.frn.flush();
        }
        Ok(n)
    }

    #[inline]
    fn apply(&mut self, idx: &mut Index, ev: &UsnEvent) {
        let r = ev.reason;
        let existing = self.frn.get(ev.frn);

        if r & USN_REASON_FILE_CREATE != 0 {
            match existing {
                Some(id) if idx.is_live(id) => {
                    // 同一 FRN 重复上报（CREATE 常和 CLOSE 合并出现），忽略。
                }
                Some(id) => {
                    // FRN 被复用了：MFT 号回收给了新文件，把墓碑复活。
                    idx.flags[id as usize] = if ev.is_dir() { crate::store::FLAG_DIR } else { 0 };
                    let p = self.parent_of(idx, ev.parent_frn, id);
                    idx.rename(id, ev.name.as_bytes(), Some(p));
                    self.stats.revived += 1;
                    self.stats.added += 1;
                }
                None => {
                    let id = idx.push_live(self.vol, ev.frn, ev.name.as_bytes(), ev.is_dir());
                    let p = self.parent_of(idx, ev.parent_frn, id);
                    idx.parent[id as usize] = p;
                    self.frn.insert(ev.frn, id);
                    self.stats.added += 1;
                }
            }
            return;
        }

        if r & USN_REASON_FILE_DELETE != 0 {
            if let Some(id) = existing {
                if idx.is_live(id) {
                    idx.kill(id);
                    self.stats.removed += 1;
                }
            }
            return;
        }

        if r & USN_REASON_RENAME_NEW_NAME != 0 {
            if let Some(id) = existing {
                let p = self.parent_of(idx, ev.parent_frn, id);
                idx.rename(id, ev.name.as_bytes(), Some(p));
                self.stats.renamed += 1;
            }
            return;
        }
        // RENAME_OLD_NAME 不用处理：紧随其后的 NEW_NAME 才是最终状态。
    }

    /// 找父记录的下标。父不在索引里（比如被排除的目录、或跨卷移动的源）时挂到卷根。
    #[inline]
    fn parent_of(&self, idx: &Index, parent_frn: u64, self_id: u32) -> u32 {
        match self.frn.get(parent_frn) {
            Some(p) if p != self_id && p < idx.len() as u32 && idx.is_live(p) => p,
            _ => ROOT_PARENT,
        }
    }
}

/// 一致性修复：删掉父链断裂的记录（父被删但子没收到 DELETE 事件）。
/// 正常情况下 USN 会给每条子记录都发 DELETE，但日志回绕、强制断电等场景会漏。
pub fn reap_orphans(idx: &mut Index) -> usize {
    let n = idx.len();
    let mut killed = 0;
    for i in 0..n {
        if !idx.is_live(i as u32) {
            continue;
        }
        let p = idx.parent[i];
        if p == ROOT_PARENT || p == i as u32 {
            continue;
        }
        if p as usize >= n || !idx.is_live(p) {
            idx.kill(i as u32);
            killed += 1;
        }
    }
    killed
}
