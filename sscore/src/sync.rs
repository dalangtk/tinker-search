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
/// 要读的那段记录已经被日志裁剪掉了（日志回绕 / 被缩小 / 被重建）。
/// 这是**位置过期**，不是卷坏了——重新定位到 `lowest_valid_usn` 就能接着读。
const ERROR_JOURNAL_ENTRY_DELETED: i32 = 1181;
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

/// 重新定位该把读位置挪到哪里（纯计算，便于单测）。
///
/// - `journal_id` 变了 = 日志被删掉后重建：整份都是新的，从它保留的最早位置重放
///   （`lowest_valid_usn` 为 0 说明还没有有效记录，直接跟到写位置）；
/// - `journal_id` 没变 = 我们要读的那段被日志裁剪了：从能读到的最早位置接着读，
///   中间 `old_next..lowest_valid_usn` 这段确实丢了，如实记进 `skipped_usn`。
fn plan_recovery(
    old_id: u64,
    old_next: i64,
    new_id: u64,
    next_usn: i64,
    lowest: i64,
) -> (Recovery, Option<i64>) {
    if new_id != old_id {
        let target = if lowest > 0 { lowest } else { next_usn };
        (
            Recovery { rebuilt: true, skipped_usn: 0, from: old_next, resume_at: target },
            Some(target),
        )
    } else {
        (
            Recovery {
                rebuilt: false,
                skipped_usn: (lowest - old_next).max(0),
                from: old_next,
                resume_at: lowest,
            },
            Some(lowest),
        )
    }
}

/// 日志重新定位的结果，见 [`Journal::recover`]。
#[derive(Clone, Copy, Debug, Default)]
pub struct Recovery {
    /// 日志被删掉后重建了：整份日志是新的，可以当作「从零重放」
    pub rebuilt: bool,
    /// 被跳过的 USN 跨度（这段区间的变更已经无法补回）
    pub skipped_usn: i64,
    /// 原来的读位置
    pub from: i64,
    /// 现在的读位置
    pub resume_at: i64,
}

/// 在已打开的卷句柄上创建 USN 日志（默认大小）。
fn create_journal(h: Handle, letter: char) -> io::Result<()> {
    // CREATE_USN_JOURNAL_DATA { MaximumSize, AllocationDelta }，传 0 用默认值。
    let input = [0u8; 16];
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h,
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
        let e = io::Error::last_os_error();
        return Err(err(&format!("卷 {letter}: 的 USN 日志不存在且创建失败: {e}")));
    }
    Ok(())
}

/// 卷的 USN 日志句柄 + 断点位置。
pub struct Journal {    handle: Handle,
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
                create_journal(h, letter)?;
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

    /// 读位置失效之后重新对表。
    ///
    /// 会先重新问一次日志的最新状态（`journal_id` / `next_usn` / `lowest_valid_usn`）：
    /// - `journal_id` 变了 → 日志被删除后重建，整个日志是新的，从最早可用位置重放即可；
    /// - `journal_id` 没变 → 只是我们要读的那段被裁剪了，从 `lowest_valid_usn` 接着读。
    ///
    /// 两种情况都会丢掉一段区间（日志里已经不存在了，客观上补不回来），
    /// 返回的 `skipped_usn` 就是这段跨度，调用方据此提示用户「这期间的变化没跟上」。
    /// 关键是**不要把整个卷的同步摘掉**：一次日志回绕不该让 C 盘从此不再实时。
    pub fn recover(&mut self) -> io::Result<Recovery> {
        let old_id = self.journal_id;
        let old_next = self.next_usn;

        match self.query() {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(ERROR_JOURNAL_NOT_ACTIVE) => {
                // 日志被整个删了：重新建一个，再问一次
                create_journal(self.handle, self.letter)?;
                self.query()?;
            }
            Err(e) => return Err(e),
        }

        let (r, seek_to) = plan_recovery(
            old_id,
            old_next,
            self.journal_id,
            self.next_usn,
            self.lowest_valid_usn,
        );
        // 位置必须真正挪过去：不挪的话下一轮还会读同一个失效位置。
        if let Some(usn) = seek_to {
            self.seek(usn);
        }
        Ok(r)
    }

    /// USN 读返回的「位置失效」类错误：日志被删 / 正在删 / 那段记录已被裁剪。
    ///
    /// 这类错误都可以靠 [`Journal::recover`] 重新定位后继续，不该当成致命错误。
    pub fn is_journal_lost(e: &io::Error) -> bool {
        matches!(
            e.raw_os_error(),
            Some(ERROR_JOURNAL_NOT_ACTIVE)
                | Some(ERROR_JOURNAL_DELETE_IN_PROGRESS)
                | Some(ERROR_JOURNAL_ENTRY_DELETED)
        )
    }
}

/// FRN → 索引下标的查找表。
///
/// 主表是按 FRN 升序的 `(frn, id)` 数组（MFT 号本身稀疏但基本递增，
/// 排序后二分 22 次即可）；同步期间新增的记录先进 `overlay`，
/// 攒够一批再合并回主表，避免每次插入都搬动大数组。
pub struct FrnIndex {
    /// 按该卷 FRN 升序排好的**记录下标**。
    ///
    /// 早先存 `(u64, u32)` 对，每条 12 字节；FRN 本来就躺在索引的 frn 列里，
    /// 这里只需要下标（4 字节），比较时回索引里取值即可——500 万条省下约 40 MB。
    keys: Vec<u32>,
    overlay: HashMap<u64, u32>,
}

impl FrnIndex {
    /// 只收 `vol` 这一个卷的记录。
    ///
    /// FRN（MFT 记录号）是**卷内编号**：C:、D:、E: 各自都从 5 号开始，
    /// 大容量盘上同一个号在多个卷里同时存在是常态（本机 45049 号同时是
    /// D:\测试1 和 C: 里的一个文件）。把三个卷混进一张表，E: 新建文件夹
    /// 的事件会撞上 D:/C: 的同号记录，被当成「同一 FRN 重复上报」丢掉。
    pub fn build(idx: &Index, vol: u16) -> FrnIndex {
        // 先数一遍再按精确长度分配：按 len/2 预分配会因翻倍扩容残留一倍冗余，
        // 500 万条上就是白占 20 MB。
        let count = idx
            .frn
            .iter()
            .enumerate()
            .filter(|(i, f)| **f != 0 && idx.vol[*i] == vol)
            .count();
        let mut keys: Vec<u32> = Vec::with_capacity(count);
        for (i, frn) in idx.frn.iter().enumerate() {
            if *frn != 0 && idx.vol[i] == vol {
                keys.push(i as u32);
            }
        }
        keys.sort_unstable_by_key(|&id| idx.frn[id as usize]);
        keys.dedup_by_key(|id: &mut u32| idx.frn[*id as usize]);
        FrnIndex { keys, overlay: HashMap::new() }
    }

    #[inline]
    pub fn get(&self, idx: &Index, frn: u64) -> Option<u32> {
        if let Some(id) = self.overlay.get(&frn) {
            return Some(*id);
        }
        // 下标升序 = FRN 升序（build/flush 都排过），比较时回索引取 FRN。
        self.keys
            .binary_search_by(|&id| idx.frn[id as usize].cmp(&frn))
            .ok()
            .map(|i| self.keys[i])
    }

    /// 这张表自己占多少字节（性能面板用）。overlay 是个小 HashMap，
    /// 每条按 16 字节估（键值 12 字节 + 控制字节 + 装载因子余量）。
    pub fn memory_bytes(&self) -> usize {
        self.keys.capacity() * 4 + self.overlay.capacity() * 16
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
    pub fn flush(&mut self, idx: &Index) {
        if self.overlay.is_empty() {
            return;
        }
        self.keys.reserve(self.overlay.len());
        for (_, id) in self.overlay.drain() {
            self.keys.push(id);
        }
        self.keys.sort_unstable_by_key(|&id| idx.frn[id as usize]);
        self.keys.dedup_by_key(|id: &mut u32| idx.frn[*id as usize]);
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
    /// 日志回绕/被删导致重新定位的次数
    pub recovered: u64,
    /// 累计被跳过的 USN 跨度（约等于没跟上的事件量级）
    pub skipped_usn: i64,
    /// 上一轮处理耗时（微秒），空闲时通常是几微秒。
    pub last_round_us: f64,
    /// 累计花在同步处理上的时间（毫秒）。
    pub busy_ms: f64,
    pub last_journal_id: u64,
    pub last_usn: i64,
}

impl SyncStats {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "轮次 {} / 事件 {} (增 {} 删 {} 改 {} 复活 {}) / 上轮 {:.1}µs / 累计 {:.1}ms",
            self.rounds,
            self.events,
            self.added,
            self.removed,
            self.renamed,
            self.revived,
            self.last_round_us,
            self.busy_ms
        );
        // 日志回绕过的卷要把「跳过了多少」摆出来，不然用户会以为索引一直是对的
        if self.recovered > 0 {
            s.push_str(&format!(
                " / 日志重定位 {} 次（跳过 USN 跨度 {}）",
                self.recovered, self.skipped_usn
            ));
        }
        s
    }
}

/// 把 USN 事件应用到索引上。
pub struct Syncer {
    pub journal: Journal,
    frn: FrnIndex,
    vol: u16,
    pub stats: SyncStats,
    /// 本轮事件动过隐藏位（有目录进/出回收站），泵线程据此决定要不要补一遍子树。
    hide_dirty: bool,
    /// 最近一次日志重定位的结果，被 `take_recovery()` 取走后清空。
    last_recovery: Option<Recovery>,
}

impl Syncer {
    /// 给已有索引挂上同步。要求索引带 FRN 列（即 MFT 建的库）。
    pub fn attach(idx: &mut Index, letter: char) -> io::Result<Syncer> {
        if !idx.has_frn() {
            return Err(err("这个索引没有 FRN 列（多半是 walk 建的），不支持实时同步；请用 mft 模式重建"));
        }
        let vol = idx.ensure_volume(&format!("{}:", letter.to_ascii_uppercase()));
        // 1 MB 足够装下几千条事件；每个卷都常驻一份缓冲，3 个盘就是 3 MB
        // 而不是 12 MB。突发事件多时 pump 会多读几轮，代价远小于常驻内存。
        let journal = Journal::open(letter, 1 << 20)?;
        let frn = FrnIndex::build(idx, vol);
        Ok(Syncer {
            journal,
            frn,
            vol,
            stats: SyncStats { last_journal_id: 0, ..Default::default() },
            hide_dirty: false,
            last_recovery: None,
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

    /// 本卷同步表占用（FRN 映射）。
    pub fn frn_map_bytes(&self) -> usize {
        self.frn.memory_bytes()
    }

    /// 本卷 USN 读缓冲占用。
    pub fn journal_bytes(&self) -> usize {
        self.journal.buf.capacity()
    }

    /// 取走最近一次日志重定位的结果（没有就返回 None）。
    pub fn take_recovery(&mut self) -> Option<Recovery> {
        self.last_recovery.take()
    }

    /// 等一批事件并应用。返回本轮处理的事件数。
    pub fn pump(&mut self, idx: &mut Index, wait_bytes: u64, timeout_ms: u64) -> io::Result<usize> {
        let t0 = std::time::Instant::now();
        // 日志回绕 / 被删 / 被缩小：读位置过期了，重新对表再读。
        //
        // 早先这里直接把错误抛出去，上层只能把整卷同步摘掉——一次日志回绕
        // 就让 C 盘从此停在旧快照上（用户看到的「实时同步中断」）。丢掉的
        // 区间客观上补不回来，但同步本身必须接着跑。
        // 最多重定位两次：极端情况下别的程序正在删/重建日志，重定位会撞上竞态。
        let mut attempts = 0u32;
        let (events, advanced) = loop {
            match self.journal.read(wait_bytes, timeout_ms) {
                Ok(v) => break v,
                Err(e) if Journal::is_journal_lost(&e) && attempts < 2 => {
                    attempts += 1;
                    let r = self.journal.recover()?;
                    self.stats.recovered += 1;
                    self.stats.skipped_usn += r.skipped_usn;
                    self.last_recovery = Some(r);
                }
                Err(e) => return Err(e),
            }
        };
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
        if self.hide_dirty {
            self.hide_dirty = false;
            cascade_hide(idx);
        }
        self.stats.rounds += 1;
        self.stats.events += n as u64;
        self.stats.last_round_us = t0.elapsed().as_secs_f64() * 1e6;
        self.stats.busy_ms += self.stats.last_round_us / 1000.0;
        self.stats.last_journal_id = self.journal.journal_id;
        self.stats.last_usn = self.journal.next_usn;
        if self.frn.len() > self.frn.keys_len() + 65_536 {
            self.frn.flush(idx);
        }
        Ok(n)
    }

    #[inline]
    fn apply(&mut self, idx: &mut Index, ev: &UsnEvent) {
        let r = ev.reason;
        let existing = self.frn.get(idx, ev.frn);

        if r & USN_REASON_FILE_CREATE != 0 {
            match existing {
                Some(id) if idx.is_live(id) => {
                    // 同一 FRN 重复上报（CREATE 常和 CLOSE 合并出现），忽略。
                }
                Some(id) => {
                    // FRN 被复用了：MFT 号回收给了新文件，把墓碑复活。
                    let p = self.parent_of(idx, ev.parent_frn, id);
                    idx.revive(id, ev.name.as_bytes(), p, ev.is_dir());
                    let hide = p != ROOT_PARENT && idx.is_hidden(p);
                    self.set_hidden(idx, id, hide);
                    self.stats.revived += 1;
                    self.stats.added += 1;
                }
                None => {
                    let id = idx.push_live(self.vol, ev.frn, ev.name.as_bytes(), ev.is_dir());
                    let p = self.parent_of(idx, ev.parent_frn, id);
                    idx.parent[id as usize] = p;
                    self.frn.insert(ev.frn, id);
                    // 在回收站 / 系统卷信息里新建的记录，天生就该是隐藏的。
                    let hide = p != ROOT_PARENT && idx.is_hidden(p);
                    self.set_hidden(idx, id, hide);
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
                // 墓碑上又来改名：多半是文件从回收站还原（FRN 没变、父目录重新出现），
                // 也可能是号被复用，两种都按「事件为准」复活。
                let revived = !idx.is_live(id);
                if revived {
                    idx.revive(id, ev.name.as_bytes(), p, ev.is_dir());
                    self.stats.revived += 1;
                } else {
                    idx.rename(id, ev.name.as_bytes(), Some(p));
                }
                // 删进回收站 = 改名到 $RECYCLE.BIN 下：跟着父目录一起隐藏，
                // 还原回普通目录时再放出来。
                let hide = p != ROOT_PARENT && idx.is_hidden(p);
                self.set_hidden(idx, id, hide);
                self.stats.renamed += 1;
            }
            return;
        }
        // RENAME_OLD_NAME 不用处理：紧随其后的 NEW_NAME 才是最终状态。
    }

    #[inline]
    fn set_hidden(&mut self, idx: &mut Index, id: u32, on: bool) {
        if idx.is_hidden(id) != on {
            idx.set_hidden(id, on);
            self.hide_dirty = true;
        }
    }

    /// 找父记录的下标。父不在索引里（比如被排除的目录、或跨卷移动的源）时挂到卷根。
    #[inline]
    fn parent_of(&self, idx: &Index, parent_frn: u64, self_id: u32) -> u32 {
        match self.frn.get(idx, parent_frn) {
            Some(p) if p != self_id && p < idx.len() as u32 && idx.is_live(p) => p,
            _ => ROOT_PARENT,
        }
    }
}

/// 这条记录本身是不是隐藏区的根（卷根下的回收站 / 系统卷信息）。
#[inline]
fn is_hide_root(idx: &Index, id: u32) -> bool {
    if idx.parent[id as usize] != ROOT_PARENT {
        return false;
    }
    let name = idx.name(id);
    crate::HIDDEN_DIRS.iter().any(|w| name.eq_ignore_ascii_case(w))
}

/// 让「隐藏」沿父子关系传播一致：父在隐藏区里，子就必须隐藏；父出来了，子也跟着出来。
///
/// 为什么需要补这一步：USN 只给「被移动的那一个目录」发事件，把整个文件夹删进
/// 回收站时，里面的子文件一条事件都不会有。不补的话，目录藏了、子文件还在结果里。
///
/// 只在真的动过隐藏位时调用（`hide_dirty`），平时一轮事件不花任何扫描。
fn cascade_hide(idx: &mut Index) -> usize {
    let n = idx.len();
    let mut total = 0usize;
    // 层次深度最多几百，够用；每轮至少改一条才会继续，所以不会空转。
    for _ in 0..512 {
        let mut changed = 0usize;
        for i in 0..n {
            let id = i as u32;
            if !idx.is_live(id) {
                continue;
            }
            let p = idx.parent[i];
            let should = is_hide_root(idx, id)
                || (p != ROOT_PARENT && (p as usize) < n && idx.is_hidden(p));
            if should != idx.is_hidden(id) {
                idx.set_hidden(id, should);
                changed += 1;
            }
        }
        total += changed;
        if changed == 0 {
            break;
        }
    }
    total
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个不碰真实卷句柄的 Syncer：只测「USN 事件怎么落到索引上」。
    fn syncer_for(idx: &Index, vol: u16, letter: char) -> Syncer {
        Syncer {
            journal: Journal {
                handle: 0,
                letter,
                journal_id: 0,
                next_usn: 0,
                lowest_valid_usn: 0,
                buf: Vec::new(),
            },
            frn: FrnIndex::build(idx, vol),
            vol,
            stats: SyncStats::default(),
            hide_dirty: false,
            last_recovery: None,
        }
    }

    fn event(frn: u64, parent: u64, reason: u32, name: &str) -> UsnEvent {
        UsnEvent {
            frn,
            parent_frn: parent,
            reason,
            attrs: 0x10, // FILE_ATTRIBUTE_DIRECTORY
            name: name.to_string(),
        }
    }

    fn file_event(frn: u64, parent: u64, reason: u32, name: &str) -> UsnEvent {
        UsnEvent { attrs: 0, ..event(frn, parent, reason, name) }
    }

    /// 本机真实数据：D:/测试1 与 E:/测试1 的 MFT 号都是 45049。
    /// 早先把三个卷的 FRN 混成一张表，E: 新建的同号目录会被当成
    /// 「同一 FRN 重复上报」直接丢掉，于是磁盘上有、搜不到。
    #[test]
    fn create_on_one_volume_is_not_swallowed_by_another() {
        let mut idx = Index::default();
        let d = idx.ensure_volume("D:");
        let e = idx.ensure_volume("E:");
        let d_id = idx.push_live(d, 45049, "旧文件.dat".as_bytes(), false);
        idx.parent[d_id as usize] = ROOT_PARENT;

        let mut s = syncer_for(&idx, e, 'E');
        s.apply(&mut idx, &event(45049, 5, USN_REASON_FILE_CREATE, "测试1"));

        let on_e: Vec<u32> = (0..idx.len() as u32)
            .filter(|i| idx.is_live(*i) && idx.vol[*i as usize] == e)
            .collect();
        assert_eq!(on_e.len(), 1, "E: 上的新目录必须进索引");
        assert_eq!(idx.name(on_e[0]), "测试1");
        // 别的卷的同号记录不能被顶替
        assert!(idx.is_live(d_id));
        assert_eq!(idx.name(d_id), "旧文件.dat");
    }

    /// 改名事件同样只能落在本卷的记录上，否则会把别的盘的文件改掉。
    #[test]
    fn rename_does_not_touch_same_frn_on_other_volume() {
        let mut idx = Index::default();
        let c = idx.ensure_volume("C:");
        let e = idx.ensure_volume("E:");
        let c_id = idx.push_live(c, 45049, "别人的文件.dat".as_bytes(), false);
        let e_id = idx.push_live(e, 45049, "改名之前.txt".as_bytes(), false);
        idx.parent[c_id as usize] = ROOT_PARENT;
        idx.parent[e_id as usize] = ROOT_PARENT;

        let mut s = syncer_for(&idx, e, 'E');
        s.apply(&mut idx, &event(45049, 5, USN_REASON_RENAME_NEW_NAME, "改名之后.txt"));

        assert_eq!(idx.name(e_id), "改名之后.txt");
        assert_eq!(idx.name(c_id), "别人的文件.dat");
    }

    /// 同一卷内重复上报 CREATE（CREATE 常与 CLOSE 合并）仍要忽略，不能重复插入。
    #[test]
    fn duplicate_create_same_volume_is_ignored() {
        let mut idx = Index::default();
        let e = idx.ensure_volume("E:");
        let mut s = syncer_for(&idx, e, 'E');
        s.apply(&mut idx, &event(100, 5, USN_REASON_FILE_CREATE, "a.txt"));
        s.apply(&mut idx, &event(100, 5, USN_REASON_FILE_CREATE, "a.txt"));
        assert_eq!(idx.live_count(), 1);
    }

    /// 1181（日志项已被删除）必须归到「可重定位」，否则一次日志回绕就会让
    /// 整卷同步被摘掉——那正是用户看到的「C: 实时同步中断」。
    #[test]
    fn journal_entry_deleted_is_recoverable() {
        let e = std::io::Error::from_raw_os_error(1181);
        assert!(Journal::is_journal_lost(&e));
        // 卷拔掉 / 权限不足这类才是真·不可恢复
        assert!(!Journal::is_journal_lost(&std::io::Error::from_raw_os_error(21)));
        assert!(!Journal::is_journal_lost(&std::io::Error::from_raw_os_error(5)));
    }

    /// 日志回绕（同一份日志被裁剪）：挪到最早可用位置，并把丢掉的跨度如实报出来。
    #[test]
    fn recovery_rebases_to_lowest_valid() {
        let (r, seek) = plan_recovery(7, 1_000, 7, 90_000, 50_000);
        assert!(!r.rebuilt);
        assert_eq!(r.skipped_usn, 49_000);
        assert_eq!(r.from, 1_000);
        assert_eq!(r.resume_at, 50_000);
        assert_eq!(seek, Some(50_000));
    }

    /// 日志被删掉重建（journal id 变了）：从新日志的最早位置整段重放。
    #[test]
    fn recovery_replays_rebuilt_journal() {
        let (r, seek) = plan_recovery(7, 1_000, 9, 4_000, 100);
        assert!(r.rebuilt);
        assert_eq!(r.skipped_usn, 0);
        assert_eq!(seek, Some(100));

        // 新日志还没有有效记录时，跟到写位置，不能 seek 到 0（0 不是有效 USN）
        let (r2, seek2) = plan_recovery(7, 1_000, 9, 4_000, 0);
        assert!(r2.rebuilt);
        assert_eq!(seek2, Some(4_000));
    }

    /// 位置其实还在有效区间内（别的错误触发 recover）：跨度算 0，不能出现负数。
    #[test]
    fn recovery_never_reports_negative_skip() {
        let (r, seek) = plan_recovery(7, 60_000, 7, 90_000, 50_000);
        assert_eq!(r.skipped_usn, 0);
        assert_eq!(seek, Some(50_000));
    }

    /// 造一个「E: 根下已有 $RECYCLE.BIN」的索引，回收站里的记录都是隐藏的。
    fn idx_with_recycle() -> (Index, u16, u32) {
        let mut idx = Index::default();
        let e = idx.ensure_volume("E:");
        let rec = idx.push_live(e, 100, b"$RECYCLE.BIN", true);
        idx.parent[rec as usize] = ROOT_PARENT;
        crate::mark_hidden(&mut idx);
        (idx, e, rec)
    }

    /// 删进回收站后不能再被搜出来：事件把父目录改到 $RECYCLE.BIN 下，
    /// 记录必须跟着父目录一起隐藏。
    #[test]
    fn move_into_recycle_bin_hides_record() {
        let (mut idx, e, _rec) = idx_with_recycle();
        let f = idx.push_live(e, 200, "合同.pdf".as_bytes(), false);
        idx.parent[f as usize] = ROOT_PARENT;
        let mut s = syncer_for(&idx, e, 'E');

        s.apply(&mut idx, &file_event(200, 100, USN_REASON_RENAME_NEW_NAME, "合同.pdf"));

        assert!(idx.is_live(f), "文件在磁盘上仍然存在，不能当墓碑");
        assert!(idx.is_hidden(f), "回收站里的文件必须隐藏");
        assert!(!idx.is_visible(f));
    }

    /// 从回收站还原回普通目录时要重新可见。
    #[test]
    fn restore_from_recycle_bin_makes_record_visible_again() {
        let (mut idx, e, rec) = idx_with_recycle();
        let f = idx.push_live(e, 200, "合同.pdf".as_bytes(), false);
        idx.parent[f as usize] = rec;
        idx.set_hidden(f, true);
        let mut s = syncer_for(&idx, e, 'E');

        s.apply(&mut idx, &file_event(200, 5, USN_REASON_RENAME_NEW_NAME, "合同.pdf"));

        assert!(idx.is_visible(f), "还原后应重新出现在结果里");
        assert_eq!(idx.parent[f as usize], ROOT_PARENT);
    }

    /// 回收站里新建的记录不能漏进搜索结果（父目录已隐藏 → 子项继承隐藏）。
    #[test]
    fn create_inside_recycle_bin_is_hidden() {
        let (mut idx, e, rec) = idx_with_recycle();
        let mut s = syncer_for(&idx, e, 'E');
        s.apply(&mut idx, &file_event(300, 100, USN_REASON_FILE_CREATE, "$RABC123.pdf"));

        let id = (0..idx.len() as u32).find(|i| idx.name(*i) == "$RABC123.pdf").unwrap();
        assert_eq!(idx.parent[id as usize], rec);
        assert!(!idx.is_visible(id));
    }

    /// 整文件夹删进回收站：子文件收不到任何事件，靠 cascade 补隐藏。
    #[test]
    fn deleting_folder_cascades_hide_to_children() {
        let (mut idx, e, _rec) = idx_with_recycle();
        let dir = idx.push_live(e, 400, "旧项目".as_bytes(), true);
        idx.parent[dir as usize] = ROOT_PARENT;
        let child = idx.push_live(e, 401, "C20261104.pdf".as_bytes(), false);
        idx.parent[child as usize] = dir;

        let mut s = syncer_for(&idx, e, 'E');
        s.apply(&mut idx, &event(400, 100, USN_REASON_RENAME_NEW_NAME, "旧项目"));
        assert!(idx.is_hidden(dir));
        // 真实链路里 pump 会看到 hide_dirty 再补一遍
        cascade_hide(&mut idx);

        assert!(!idx.is_visible(dir));
        assert!(!idx.is_visible(child), "文件夹藏了，里面的文件也必须藏");
    }

    /// 还原文件夹时子文件一起放出来。
    #[test]
    fn restoring_folder_cascades_visibility_to_children() {
        let (mut idx, e, rec) = idx_with_recycle();
        let dir = idx.push_live(e, 400, "旧项目".as_bytes(), true);
        idx.parent[dir as usize] = rec;
        idx.set_hidden(dir, true);
        let child = idx.push_live(e, 401, "C20261104.pdf".as_bytes(), false);
        idx.parent[child as usize] = dir;
        idx.set_hidden(child, true);

        let mut s = syncer_for(&idx, e, 'E');
        s.apply(&mut idx, &event(400, 5, USN_REASON_RENAME_NEW_NAME, "旧项目"));
        cascade_hide(&mut idx);

        assert!(idx.is_visible(dir));
        assert!(idx.is_visible(child));
    }
}
