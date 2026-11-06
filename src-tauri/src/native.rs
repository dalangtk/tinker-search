//! 索引内核接入层：sscore 引擎 + NTFS USN 实时同步。
//!
//! - 索引完全在内存里，查询直接扫名字池，不碰磁盘；
//! - 用卷级 USN 日志做增量同步，不需要给每个目录开句柄；
//! - 不依赖任何外部 DLL。
//!
//! 能力边界：建库 / 搜索 / 分类筛选 / 目录范围 / SQL / 实时同步。
//! 没有 Lua 脚本、图标缓存、别名与评分，相应命令在命令层返回空结果或 no-op。

use crate::model::{DirListing, IndexStats, MemPart, MemStat, Row, SearchStart};
use once_cell::sync::OnceCell;
use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use sscore::sql::{self, Table, Value};
use sscore::sync::{SyncStats, Syncer};
use sscore::{browse, resolve_path, search_timed, Filter, Hit, Index, QueryOpts};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

fn emit(app: &AppHandle, name: &str, payload: serde_json::Value) {
    let _ = app.emit(name, payload);
}

extern "system" {
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(lprootpathname: *const u16) -> u32;
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn GetCurrentProcessId() -> u32;
    fn SetProcessWorkingSetSize(h: *mut std::ffi::c_void, min: usize, max: usize) -> i32;
    fn GlobalMemoryStatusEx(ptr: *mut MemoryStatusEx) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
    fn Process32FirstW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn CloseHandle(h: isize) -> i32;
}

#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(
        process: *mut std::ffi::c_void,
        counters: *mut ProcessMemoryCountersEx2,
        cb: u32,
    ) -> i32;
}

/// 与 Windows `PROCESS_MEMORY_COUNTERS_EX2` 对齐（Win8+）。
///
/// 比老的 `PROCESS_MEMORY_COUNTERS` 多三个字段，其中
/// `private_working_set_size` 正是任务管理器「内存」那一列的取数口径，
/// 所以这里直接要 EX2，省得面板和工作管理器对不上。
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCountersEx2 {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    /// 私有提交（= 任务管理器「提交大小」）
    private_usage: usize,
    /// 专用工作集（= 任务管理器「内存」，已实测与 WMI WorkingSetPrivate 一致）
    private_working_set_size: usize,
    /// 共享提交
    shared_commit_usage: u64,
}

/// 一个进程的内存读数，单位字节。
#[derive(Default, Clone, Copy)]
struct ProcMem {
    /// 工作集：常驻物理页，含与其他进程共享的 DLL / 映像页
    ws: i64,
    /// 专用工作集：只算独占的页，任务管理器「内存」列用的就是它
    private_ws: i64,
    peak_ws: i64,
    /// 私有提交
    commit: i64,
}

/// 与 Windows `MEMORYSTATUSEX` 对齐。
#[repr(C)]
#[derive(Default)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

/// 与 Windows `PROCESSENTRY32W` 对齐（x64 下 `ULONG_PTR` 是 8 字节）。
#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

// 数组超过 32 个元素时 `[u16; 260]` 没有 Default，只能手写。
impl Default for ProcessEntry32W {
    fn default() -> Self {
        ProcessEntry32W {
            dw_size: 0,
            cnt_usage: 0,
            th32_process_id: 0,
            th32_default_heap_id: 0,
            th32_module_id: 0,
            cnt_threads: 0,
            th32_parent_process_id: 0,
            pc_pri_class_base: 0,
            dw_flags: 0,
            sz_exe_file: [0u16; 260],
        }
    }
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_VM_READ: u32 = 0x0010;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const SNAP_INVALID: isize = -1;

/// 文件类型分类表。名字、代号保持稳定，前端侧栏不需要换一套数据；
/// 「后缀」交给内核做匹配。
pub const CATS: &[(&str, u16, &str)] = &[
    ("文件夹", 255, ""),
    ("快捷方式", 100, "LNK,URL"),
    ("程序", 99, "EXE,MSI,BAT,CMD,COM,SCR,PS1,APK"),
    ("音频", 98, "MP3,FLAC,WAV,APE,AAC,OGG,M4A,WMA,AIFF"),
    ("视频", 97, "MP4,MKV,MOV,AVI,WMV,FLV,TS,MPG,MPEG,WEBM,M4V,RMVB"),
    ("图片", 96, "PNG,JPG,JPEG,GIF,BMP,WEBP,SVG,ICO,TIF,TIFF,RAW,HEIC,PSD"),
    ("文档", 80, "TXT,MD,PDF,HTML,HTM,XML,JSON,YAML,YML,LOG,CSV,INI,RTF,EPUB"),
    ("办公", 79, "DOC,DOCX,XLS,XLSX,PPT,PPTX,WPS,ET,DPS,ODT,ODS,ODP,PAGES,KEY"),
    ("压缩包", 60, "ZIP,RAR,7Z,TAR,GZ,BZ2,XZ,ISO,CAB,DMG"),
    ("代码", 50, "RS,TS,TSX,JS,JSX,VUE,PY,GO,JAVA,C,CPP,H,HPP,CS,SQL,SH,LUA,TOML,SVELTE"),
];

/// 大小/时间排序用的元数据。索引里不存这些字段，第一次按它排序时
/// 现场 `stat` 一遍并缓存，之后切换升降序就不用再碰磁盘。
#[derive(Clone, Copy, Default)]
struct Meta {
    size: i64,
    mtime: i64,
    ctime: i64,
    atime: i64,
}

/// 结果列表支持的排序字段。与前端表头、状态栏下拉共用同一份名字。
pub const SORT_FIELDS: &[&str] = &[
    "相关度",
    "文件名",
    "文件夹",
    "文件大小",
    "修改时间",
    "文件类型",
    "路径",
    "扩展名",
    "创建时间",
    "访问时间",
];

struct Core {
    idx: Index,
    /// 上一次搜索的命中（按相关度排好序），`view` 以它为基准重排。
    hits: Vec<Hit>,
    /// 当前展示顺序（记录 id），分页从这里切片。
    view: Vec<u32>,
    fp: i32,
    keyword: String,
    scope: Option<String>,
    /// 侧栏「文件类型」选中的分类
    filter: Filter,
    /// 路径 → 索引记录 id 的解析缓存（常用位置 / 搜索范围）
    places: HashMap<String, (Option<u32>, Instant)>,
    /// 最近一次 SQL 的完整结果，供 `sql_result` 分页取
    sql: Option<(Table, i32)>,
    /// 当前排序字段与方向（false = 降序）
    sort_field: String,
    sort_asc: bool,
    /// 大小/时间排序的元数据缓存
    meta: HashMap<u32, Meta>,
    /// 最近一次搜索的模式，重排后回发事件时原样带上
    mode: i32,
}

pub struct Native {
    core: RwLock<Core>,
    /// 每个卷一个 Syncer（卷内 FRN 各自独立，不能合并成一张表）
    sync: Mutex<Vec<Syncer>>,
    db: Mutex<PathBuf>,
    ready: AtomicBool,
    scanning: AtomicBool,
    syncing: AtomicBool,
    stop: AtomicBool,
    /// 建库进度 0..1
    progress: Mutex<f64>,
    notice: Mutex<Option<String>>,
    sync_stats: Mutex<SyncStats>,
    /// 上次加载索引耗时（毫秒），性能面板展示
    load_ms: Mutex<i64>,
}

static NATIVE: OnceCell<Native> = OnceCell::new();

/// 初始化（在 setup 阶段调用一次）。
pub fn init(db: PathBuf) {
    let _ = NATIVE.set(Native {
        core: RwLock::new(Core {
            idx: Index::default(),
            hits: Vec::new(),
            view: Vec::new(),
            fp: 0,
            keyword: String::new(),
            scope: None,
            filter: Filter::default(),
            places: HashMap::new(),
            sql: None,
            sort_field: "相关度".into(),
            sort_asc: false,
            meta: HashMap::new(),
            mode: 0,
        }),
        sync: Mutex::new(Vec::new()),
        db: Mutex::new(db),
        ready: AtomicBool::new(false),
        scanning: AtomicBool::new(false),
        syncing: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        progress: Mutex::new(0.0),
        notice: Mutex::new(None),
        sync_stats: Mutex::new(SyncStats::default()),
        load_ms: Mutex::new(0),
    });
}

fn state() -> Result<&'static Native, String> {
    NATIVE.get().ok_or_else(|| "原生后端未初始化".to_string())
}

pub fn is_ready() -> bool {
    NATIVE.get().map(|n| n.ready.load(Ordering::SeqCst)).unwrap_or(false)
}

pub fn db_path() -> PathBuf {
    NATIVE.get().map(|n| n.db.lock().clone()).unwrap_or_default()
}

/// 从磁盘加载已有索引。文件不存在不算错误，只是「还没建库」。
pub fn load(app: &AppHandle) -> Result<usize, String> {
    let n = state()?;
    let path = n.db.lock().clone();
    if !path.exists() {
        *n.notice.lock() = Some("还没有原生索引，请先建库".into());
        emit(app, "tinker:engine-ready", serde_json::json!({ "ok": false, "error": "索引文件不存在" }));
        return Ok(0);
    }
    let t = Instant::now();
    match Index::load(&path) {
        Ok(mut idx) => {
            // 老库（这版之前建的）里回收站、系统卷信息的记录还带着可见状态，
            // 加载时补一次隐藏位即可，不用重建索引。已经标过的库直接跳过，
            // 免得每次启动都白扫 500 万条。
            if idx.hidden_count() == 0 {
                let hidden = sscore::mark_hidden(&mut idx);
                if hidden > 0 {
                    let _ = idx.save(&path);
                }
            }
            let live = idx.live_count();
            let ms = t.elapsed().as_millis();
            *n.load_ms.lock() = ms as i64;
            {
                let mut c = n.core.write();
                c.idx = idx;
            }
            n.ready.store(true, Ordering::SeqCst);
            *n.notice.lock() = None;
            emit(app, "tinker:engine-ready", serde_json::json!({ "ok": true }));
            emit(app, "tinker:load-complete", serde_json::json!({ "fileCount": live, "elapsedMs": ms }));

            // 索引一就绪就把 USN 同步挂上，后面进程活着期间索引自己保鲜。
            // 非管理员打不开卷句柄，start_sync 会把原因写进 notice。
            start_sync(app.clone());
            Ok(live)
        }
        Err(e) => {
            *n.notice.lock() = Some(format!("索引加载失败：{e}"));
            emit(app, "tinker:engine-ready", serde_json::json!({ "ok": false, "error": e.to_string() }));
            Err(e.to_string())
        }
    }
}

// ============================================================================
// 分类 / 磁盘 / 目录范围
// ============================================================================

/// 原生后端的文件分类表。
pub fn filters() -> Vec<serde_json::Value> {
    let mut out = vec![serde_json::json!({ "名称": "全部", "类型": 0 })];
    for (name, ty, exts) in CATS {
        out.push(serde_json::json!({ "名称": name, "类型": ty, "后缀": exts }));
    }
    out
}

/// 把分类名翻译成内核的筛选条件。
fn build_filter(names: &[String]) -> Filter {
    let mut f = Filter::default();
    for n in names {
        if n == "全部" {
            continue;
        }
        let Some((name, _, exts)) = CATS.iter().find(|(k, _, _)| k == n) else { continue };
        if *name == "文件夹" {
            f.dirs_only = true;
            continue;
        }
        for e in exts.split(',') {
            let e = e.trim();
            if !e.is_empty() {
                f.exts.push(e.to_ascii_lowercase().into_bytes());
            }
        }
    }
    f
}

/// 侧栏「文件类型」多选后由前端下发。这里把名字摊成后缀集合存起来，
/// 下一次查询直接用。
pub fn set_filter(names: &[String]) {
    let Some(n) = NATIVE.get() else { return };
    let mut c = n.core.write();
    c.filter = build_filter(names);
}

/// 把路径解析成索引里的记录 id，带 60 秒缓存。
/// 分类栏的常用位置在每次搜索时都要用，而解析一次要扫全表，必须缓存。
fn resolve_place(c: &mut Core, path: &str) -> Option<u32> {
    if let Some((id, at)) = c.places.get(path) {
        if at.elapsed() < Duration::from_secs(60) {
            return *id;
        }
    }
    let id = resolve_path(&c.idx, path);
    c.places.insert(path.to_string(), (id, Instant::now()));
    id
}

fn vol_index(idx: &Index, letter: &str) -> Option<u16> {
    let l = letter.trim().trim_end_matches(':').to_ascii_uppercase();
    idx.volumes
        .iter()
        .position(|v| v.trim_end_matches(':').eq_ignore_ascii_case(&l))
        .map(|i| i as u16)
}

/// 索引里没收录这个卷时塞进 `Filter::vols` 的哨兵值。
/// 它等于一个不存在的卷号，因此匹配不到任何记录——比把条件悄悄丢掉诚实：
/// 用户点了 D: 却看到 C: 的结果，会以为「D: 里就这些」，比看到 0 条更误导。
const NO_VOL: u16 = u16::MAX;

/// 当前索引实际收录了哪些卷（形如 `["C:", "D:"]`），前端用它标出「未建索引」的磁盘。
pub fn indexed_volumes() -> Vec<String> {
    NATIVE
        .get()
        .map(|n| n.core.read().idx.volumes.clone())
        .unwrap_or_default()
}

/// 这个目录能否在当前索引里定位到。快捷方式的目标不在索引里时，
/// 应用内浏览只会得到空列表，应该退回资源管理器打开。
pub fn can_browse(path: &str) -> bool {
    match state() {
        Ok(n) => resolve_path(&n.core.read().idx, path).is_some(),
        Err(_) => false,
    }
}

// ============================================================================
// 排序
// ============================================================================

/// 支持的排序字段（状态栏下拉直接用它）。
pub fn sort_fields() -> Vec<String> {
    SORT_FIELDS.iter().map(|s| (*s).to_string()).collect()
}

fn parent_of(path: &str) -> &str {
    match path.rfind('\\') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// 相对浏览目录的层级：1 = 直接子项，2 = 孙子项…… 不在该子树下返回 `usize::MAX`。
fn depth_under(path: &str, scope: &str) -> usize {
    let s = scope.trim_end_matches(['\\', '/']);
    if s.is_empty() {
        return usize::MAX;
    }
    let p = path.trim_end_matches(['\\', '/']);
    // get() 而不是切片：路径里有中文时，s.len() 可能正好落在字符中间
    let Some(head) = p.get(..s.len()) else {
        return usize::MAX;
    };
    if !head.eq_ignore_ascii_case(s) {
        return usize::MAX;
    }
    let tail = &p[s.len()..];
    // 只认「目录\子项」这种边界。D:\ab 与 D:\abc 拼起来前缀相同，但不是父子关系。
    let rest = if tail.is_empty() {
        ""
    } else if let Some(r) = tail.strip_prefix('\\').or_else(|| tail.strip_prefix('/')) {
        r
    } else {
        return usize::MAX;
    };
    if rest.is_empty() {
        0
    } else {
        rest.matches(['\\', '/']).count() + 1
    }
}

/// 目录浏览时的分组：0 = 当前目录下的子文件夹，1 = 当前目录下的文件，
/// 2 = 更深层的内容。组内顺序仍由用户选的排序字段决定。
fn entry_group(idx: &Index, scope: &str, id: u32) -> u8 {
    match depth_under(&idx.path_of(id), scope) {
        0 | 1 => {
            if idx.is_dir(id) {
                0
            } else {
                1
            }
        }
        _ => 2,
    }
}

/// 给 `ids` 里还没缓存过的记录补 `stat` 元数据。
///
/// 排序要全局有序，不能只 `stat` 一页；百万级命中时顺序 `stat` 太慢，
/// 这里用 rayon 并行取。索引里没有大小/时间列是刻意的取舍：
/// 全盘每条多存 24 字节约 60MB，而按大小/时间排序只是偶发操作。
fn fill_meta(idx: &Index, cache: &mut HashMap<u32, Meta>, ids: &[u32]) {
    let missing: Vec<u32> = ids
        .iter()
        .copied()
        .filter(|id| !cache.contains_key(id))
        .collect();
    if missing.is_empty() {
        return;
    }
    let fetched: Vec<(u32, Meta)> = missing
        .par_iter()
        .map(|&id| {
            let path = idx.path_of(id);
            let meta = match std::fs::metadata(&path) {
                Ok(md) => Meta {
                    size: md.len() as i64,
                    mtime: ms_of(md.modified()),
                    ctime: ms_of(md.created()),
                    atime: ms_of(md.accessed()),
                },
                Err(_) => Meta::default(),
            };
            (id, meta)
        })
        .collect();
    cache.extend(fetched);
}

/// 按 `sort_field` / `sort_asc` 重排 `view`。`hits` 始终保持相关度顺序，
/// 所以切回「相关度」时不需要重新搜索。
fn apply_sort(c: &mut Core) {
    c.view.clear();
    c.view.extend(c.hits.iter().map(|h| h.id));
    sort_by_field(c);

    // 进入某个目录浏览时固定「子文件夹 → 子文件 → 更深层的内容」。
    // sort_by_key 是稳定排序：组内仍然保持上面按字段排好的顺序。
    if let Some(scope) = c.scope.clone() {
        // 只在这里规范化一次：索引里的路径统一是反斜杠，depth_under 不用重复处理
        let scope = scope.replace('/', "\\");
        let idx = &c.idx;
        c.view.sort_by_key(|&id| entry_group(idx, &scope, id));
    }
}

fn sort_by_field(c: &mut Core) {
    let asc = c.sort_asc;
    match c.sort_field.as_str() {
        "相关度" => {
            // hits 已经是高分在前；升序就是反过来（低分在前）。
            if asc {
                c.view.reverse();
            }
            return;
        }
        "文件名" => {
            let idx = &c.idx;
            c.view
                .sort_by_cached_key(|&id| (idx.name(id).to_lowercase(), id));
        }
        "文件夹" => {
            let idx = &c.idx;
            c.view
                .sort_by_cached_key(|&id| (parent_of(&idx.path_of(id)).to_lowercase(), id));
        }
        "路径" => {
            let idx = &c.idx;
            c.view
                .sort_by_cached_key(|&id| (idx.path_of(id).to_lowercase(), id));
        }
        "文件类型" => {
            let idx = &c.idx;
            c.view.sort_by_cached_key(|&id| {
                let key = if idx.is_dir(id) {
                    "文件夹".to_string()
                } else {
                    ext_of(idx.name(id))
                };
                (key, id)
            });
        }
        "扩展名" => {
            let idx = &c.idx;
            c.view
                .sort_by_cached_key(|&id| (ext_of(idx.name(id)), id));
        }
        "文件大小" => {
            fill_meta(&c.idx, &mut c.meta, &c.view);
            let meta = &c.meta;
            c.view.sort_by_cached_key(|&id| {
                (meta.get(&id).copied().unwrap_or_default().size, id)
            });
        }
        "修改时间" => {
            fill_meta(&c.idx, &mut c.meta, &c.view);
            let meta = &c.meta;
            c.view.sort_by_cached_key(|&id| {
                (meta.get(&id).copied().unwrap_or_default().mtime, id)
            });
        }
        "创建时间" => {
            fill_meta(&c.idx, &mut c.meta, &c.view);
            let meta = &c.meta;
            c.view.sort_by_cached_key(|&id| {
                (meta.get(&id).copied().unwrap_or_default().ctime, id)
            });
        }
        "访问时间" => {
            fill_meta(&c.idx, &mut c.meta, &c.view);
            let meta = &c.meta;
            c.view.sort_by_cached_key(|&id| {
                (meta.get(&id).copied().unwrap_or_default().atime, id)
            });
        }
        _ => {
            // 配置里残留了旧字段名：退回相关度，不让排序静默失效。
            c.sort_field = "相关度".into();
            if asc {
                c.view.reverse();
            }
            return;
        }
    }
    if !asc {
        c.view.reverse();
    }
}

/// 设置排序并重排当前结果。结果已经出来时回发 `search-complete`，
/// 让前端按新顺序重新拉第一页。
pub fn set_sort(app: &AppHandle, field: &str, asc: bool) -> Result<(), String> {
    let n = state()?;
    if !SORT_FIELDS.contains(&field) {
        return Err(format!("不支持的排序字段：{field}"));
    }

    let (fp, count, elapsed, mode, emit_it) = {
        let mut c = n.core.write();
        let t = Instant::now();
        c.sort_field = field.to_string();
        c.sort_asc = asc;
        apply_sort(&mut c);
        let has_sql = c.sql.is_some();
        (
            c.fp,
            c.view.len(),
            t.elapsed().as_secs_f64() * 1000.0,
            c.mode,
            !has_sql && c.fp > 0,
        )
    };

    if emit_it {
        emit(
            app,
            "tinker:search-complete",
            serde_json::json!({
                "fingerprint": fp,
                "count": count,
                "elapsedMs": elapsed,
                "searchType": mode,
                "aggregate": false,
                "empty": count == 0,
            }),
        );
    }
    Ok(())
}

// ============================================================================
// 搜索
// ============================================================================

pub fn search(
    app: &AppHandle,
    keyword: &str,
    mode: i32,
    scope: Option<&str>,
    places: &[String],
    drives: &[String],
) -> Result<SearchStart, String> {
    let n = state()?;
    if !n.ready.load(Ordering::SeqCst) {
        return Err("索引尚未就绪".into());
    }
    if mode == 2 {
        return sql_search(app, keyword);
    }

    let t = Instant::now();
    let kw = keyword.trim();
    let mut unresolved: Vec<String> = Vec::new();

    let (count, fp, filter_empty) = {
        let mut c = n.core.write();
        let mut filter = c.filter.clone();

        // 「磁盘」多选 → 卷列号
        for d in drives {
            match vol_index(&c.idx, d) {
                Some(v) => filter.vols.push(v),
                // 这个卷还没进索引：用哨兵让它命中 0 条，而不是当作没选
                None => filter.vols.push(NO_VOL),
            }
        }
        // 「常用位置」+ 搜索范围 → 目录范围
        let scopes: Vec<&str> = places
            .iter()
            .map(String::as_str)
            .chain(scope.iter().copied())
            .filter(|s| !s.trim().is_empty())
            .collect();
        for p in &scopes {
            match resolve_place(&mut c, p) {
                Some(id) => filter.under.push(id),
                None => unresolved.push((*p).to_string()),
            }
        }

        let filter_empty = filter.is_empty();
        let mut hits = if kw.is_empty() {
            if filter_empty {
                Vec::new()
            } else {
                // 空关键词 + 只有分类条件：这本身就是一次「浏览」
                browse(&c.idx, &filter, 50_000)
            }
        } else {
            let opts = QueryOpts {
                limit: 200_000,
                dirs: None,
                search_path: kw.contains('\\') || kw.contains('/'),
                filter: filter.clone(),
            };
            search_timed(&c.idx, kw, &opts).0
        };

        // 目录还没进索引（例如刚建的库缺这条路径）：退回字符串前缀匹配
        if !unresolved.is_empty() {
            let prefixes: Vec<String> = unresolved
                .iter()
                .map(|p| p.trim_end_matches('\\').to_ascii_lowercase())
                .collect();
            hits.retain(|h| {
                let path = c.idx.path_of(h.id).to_ascii_lowercase();
                prefixes.iter().any(|p| path.starts_with(p))
            });
        }

        // 浏览目录时结果只留内容，不留目录自己
        if kw.is_empty() && !filter.under.is_empty() {
            hits.retain(|h| !filter.under.contains(&h.id));
        }

        c.fp += 1;
        let fp = c.fp;
        let count = hits.len();
        c.keyword = keyword.to_string();
        c.scope = scope.map(|s| s.to_string());
        c.hits = hits;
        c.sql = None;
        c.mode = mode;
        // 元数据缓存跨查询保留：id 在索引重建/同步变更时已被清空，
        // 这里再清一次会让「按修改时间排序」在每次输入时都重 stat 全部命中。
        apply_sort(&mut c);
        (count, fp, filter_empty)
    };

    emit(
        app,
        "tinker:search-complete",
        serde_json::json!({
            "fingerprint": fp,
            "count": count,
            "elapsedMs": t.elapsed().as_secs_f64() * 1000.0,
            "searchType": mode,
            "aggregate": false,
            "empty": filter_empty && kw.is_empty(),
        }),
    );
    Ok(SearchStart { fingerprint: fp, accepted: true })
}

pub fn cancel_search(_fp: i32) {
    // 原生搜索是同步完成的（百万级也就几毫秒到几十毫秒），
    // 没有可取消的中间态；保留命令只为让前端逻辑无需分支。
}

/// 自然语言搜索：条件由 `nlu` 解析好再传进来，这里的活是「按条件取候选 + 用 stat 过滤时间」。
///
/// 时间/大小不在索引里（索引只存名字和父指针，这是 46 字节/条的前提），
/// 所以这一步必须现场 `stat`。好在这里只对**已经由后缀 + 关键词收窄过的候选**取元数据，
/// 而且 `c.meta` 跨查询保留，同一批文件第二次过滤基本不花时间。
///
/// 返回 `(SearchStart, 命中数, 过滤前候选数)`，前端用后两个数字显示「从 N 个里筛出 M 个」。
pub fn nl_search(
    app: &AppHandle,
    q: &crate::nlu::NluQuery,
    scope: Option<&str>,
    places: &[String],
    drives: &[String],
) -> Result<(SearchStart, i32, i32), String> {
    let n = state()?;
    if !n.ready.load(Ordering::SeqCst) {
        return Err("索引尚未就绪".into());
    }
    let t = Instant::now();

    let (count, scanned, fp) = {
        let mut c = n.core.write();
        let (count, scanned) = nl_run(&mut c, q, scope, places, drives)?;
        (count, scanned, c.fp)
    };

    emit(
        app,
        "tinker:search-complete",
        serde_json::json!({
            "fingerprint": fp,
            "count": count,
            "elapsedMs": t.elapsed().as_secs_f64() * 1000.0,
            "searchType": 3,
            "aggregate": false,
            "empty": false,
            "nl": q,
            "scanned": scanned,
        }),
    );
    Ok((SearchStart { fingerprint: fp, accepted: true }, count, scanned))
}

/// `nl_search` 的核心，拆出来是为了能脱离 Tauri 运行时做单元测试。
/// 返回 `(命中数, 过滤前候选数)`。
fn nl_run(
    c: &mut Core,
    q: &crate::nlu::NluQuery,
    scope: Option<&str>,
    places: &[String],
    drives: &[String],
) -> Result<(i32, i32), String> {
    let mut unresolved: Vec<String> = Vec::new();

    Ok({        // 自然语言自带类型条件，不再叠加侧栏筛选（否则用户说「pdf」却被侧栏的「图片」挡掉）
        let mut filter = Filter::default();
        for e in &q.exts {
            filter.exts.push(e.as_bytes().to_vec());
        }
        filter.dirs_only = q.dirs_only;

        for d in drives {
            match vol_index(&c.idx, d) {
                Some(v) => filter.vols.push(v),
                None => filter.vols.push(NO_VOL),
            }
        }
        let scopes: Vec<&str> = places
            .iter()
            .map(String::as_str)
            .chain(scope.iter().copied())
            .filter(|s| !s.trim().is_empty())
            .collect();
        for p in &scopes {
            match resolve_place(&mut *c, p) {
                Some(id) => filter.under.push(id),
                None => unresolved.push((*p).to_string()),
            }
        }

        let kw = q.keyword.trim();
        let mut hits = if kw.is_empty() {
            // 纯条件（「今天下载的 pdf」）就是一次带过滤的遍历
            browse(&c.idx, &filter, 60_000)
        } else {
            let opts = QueryOpts {
                limit: 200_000,
                dirs: None,
                search_path: kw.contains('\\') || kw.contains('/'),
                filter: filter.clone(),
            };
            search_timed(&c.idx, kw, &opts).0
        };

        if !unresolved.is_empty() {
            let prefixes: Vec<String> = unresolved
                .iter()
                .map(|p| p.trim_end_matches('\\').to_ascii_lowercase())
                .collect();
            hits.retain(|h| {
                let path = c.idx.path_of(h.id).to_ascii_lowercase();
                prefixes.iter().any(|p| path.starts_with(p))
            });
        }

        let scanned = hits.len();

        // 时间过滤：先并行补齐元数据，再按字段裁掉区间外的
        if q.from_ms > 0 || q.to_ms > 0 {
            if scanned > NL_STAT_LIMIT {
                return Err(format!(
                    "符合「{}」的文件有 {} 个，超过 {} 个就要逐个读文件时间，太慢。请加上更具体的词，比如文件类型、关键词或更短的时间范围。",
                    q.label, scanned, NL_STAT_LIMIT
                ));
            }
            let ids: Vec<u32> = hits.iter().map(|h| h.id).collect();
            // 一次性重借 `c`，避免 `&c.idx` 与 `&mut c.meta` 各借一次守卫
            let core = &mut *c;
            fill_meta(&core.idx, &mut core.meta, &ids);
            let meta = &core.meta;
            let (from, to) = (q.from_ms, q.to_ms);
            let field = q.field.as_str();
            hits.retain(|h| {
                let m = meta.get(&h.id).copied().unwrap_or_default();
                let ts = match field {
                    "mtime" => m.mtime,
                    "atime" => m.atime,
                    _ => m.ctime,
                };
                // stat 失败的时间是 0，直接判为不匹配，避免脏数据混进结果
                ts > 0 && (from == 0 || ts >= from) && (to == 0 || ts < to)
            });
        }

        c.fp += 1;
        let count = hits.len();
        c.keyword = q.keyword.clone();
        c.scope = scope.map(|s| s.to_string());
        c.hits = hits;
        c.sql = None;
        c.mode = 3;
        apply_sort(c);
        (count as i32, scanned as i32)
    })
}

/// 时间过滤最多允许对这么多条候选做 stat。放在这里而不是引擎里，
/// 是因为上限取决于磁盘：NVMe 上 10 万条约 0.4 秒，机械盘会慢一个量级。
const NL_STAT_LIMIT: usize = 120_000;

fn sql_search(app: &AppHandle, text: &str) -> Result<SearchStart, String> {
    let n = state()?;
    let t = Instant::now();
    let mut table = {
        let c = n.core.read();
        sql::run(&c.idx, text, &c.filter, 500)?
    };
    let total = table.total;
    let aggregate = table.aggregate;
    let fp = {
        let mut c = n.core.write();
        c.fp += 1;
        c.hits.clear();
        let fp = c.fp;
        table.total = total;
        c.sql = Some((table, fp));
        fp
    };
    emit(
        app,
        "tinker:search-complete",
        serde_json::json!({
            "fingerprint": fp,
            "count": total,
            "elapsedMs": t.elapsed().as_secs_f64() * 1000.0,
            "searchType": 2,
            "aggregate": aggregate,
        }),
    );
    Ok(SearchStart { fingerprint: fp, accepted: true })
}

/// SQL 结果分页（前端 SqlTable 只吃 JSON）。
pub fn sql_result(start: i32, max: i32) -> Option<serde_json::Value> {
    let n = NATIVE.get()?;
    let c = n.core.read();
    let (t, _) = c.sql.as_ref()?;
    Some(table_json(t, start.max(0) as usize, max.max(1) as usize))
}

fn value_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Num(x) => serde_json::Number::from_f64(*x)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Null => serde_json::Value::Null,
    }
}

fn table_json(t: &Table, start: usize, max: usize) -> serde_json::Value {
    let end = (start + max).min(t.rows.len());
    let rows: Vec<serde_json::Value> = if start >= t.rows.len() {
        Vec::new()
    } else {
        t.rows[start..end]
            .iter()
            .map(|r| serde_json::Value::Array(r.iter().map(value_json).collect()))
            .collect()
    };
    // 行号：前端靠它反查完整记录（预览、双击打开）
    let ids: Vec<u32> = if start >= t.ids.len() {
        Vec::new()
    } else {
        t.ids[start..end.min(t.ids.len())].to_vec()
    };
    let groups: Vec<serde_json::Value> = t
        .groups
        .iter()
        .map(|g| {
            serde_json::json!({
                "键文本": g.key_text,
                "键整数": g.key_num,
                "计数": g.count,
                "求和": g.sum,
                "最小值": g.min,
                "最大值": g.max,
            })
        })
        .collect();
    serde_json::json!({
        "聚合": t.aggregate,
        "字段数量": t.fields.len(),
        "字段": t.fields,
        "类型": t.types,
        "总数量": t.total,
        "起始": start,
        "限制": max,
        "行": rows,
        "行id": ids,
        "分组": groups,
        "分组字段": t.group_label,
    })
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// 把 SystemTime 转成毫秒时间戳（前端 `new Date(ms)` 直接用）。
fn ms_of(t: std::io::Result<std::time::SystemTime>) -> i64 {
    match t {
        Ok(t) => t
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// 组一条结果行。
///
/// 索引里只存「父记录 + 名字」，所以路径要现场拼；大小和时间**不存**，
/// 取一页时按需 `stat` 一下。一页最多几十行，SSD 上约 10µs/次，
/// 比在索引里给每条记录多存 24 字节（全盘 257 万条约 60MB）划算得多。
/// 只对筛到最后的少量候选取元数据，避免全盘 stat。
fn row_of(idx: &Index, id: u32) -> Row {
    let name = idx.name(id).to_string();
    let path = idx.path_of(id);
    let parent = match path.rfind('\\') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    };
    let is_dir = idx.is_dir(id);
    let mut row = Row {
        id: id as i32,
        ext: ext_of(&name),
        name,
        path,
        parent,
        is_dir,
        file_type: if is_dir { "文件夹".into() } else { String::new() },
        ..Default::default()
    };
    if let Ok(md) = std::fs::metadata(&row.path) {
        row.size = md.len() as i64;
        row.is_dir = md.is_dir();
        row.mtime = ms_of(md.modified());
        row.ctime = ms_of(md.created());
        row.atime = ms_of(md.accessed());
        if row.is_dir && row.file_type.is_empty() {
            row.file_type = "文件夹".into();
        }
    }
    row
}

pub fn rows(start: i32, count: i32) -> Result<Vec<Row>, String> {
    let n = state()?;
    let c = n.core.read();
    let s = start.max(0) as usize;
    if s >= c.view.len() {
        return Ok(Vec::new());
    }
    let e = (s + count.max(0) as usize).min(c.view.len());
    let mut out = Vec::with_capacity(e - s);
    for &id in &c.view[s..e] {
        // 同步线程可能刚把这条删掉，墓碑直接跳过。
        if c.idx.is_live(id) {
            out.push(row_of(&c.idx, id));
        }
    }
    Ok(out)
}

pub fn row_by_id(id: i32) -> Result<Row, String> {
    let n = state()?;
    let c = n.core.read();
    if id < 0 || (id as usize) >= c.idx.len() || !c.idx.is_live(id as u32) {
        return Err("记录不存在".into());
    }
    Ok(row_of(&c.idx, id as u32))
}

/// 列出某个目录的**直接子项**（预览面板用，不递归）。
///
/// 和 `search` 带 scope 的目录浏览不同：那是「这个子树里有什么」，
/// 这个是「这个文件夹里摆着什么」，跟资源管理器双击进去看到的一层一致。
/// 目录在前、同类按名字排；只给前 `limit` 条读磁盘元数据。
pub fn list_dir(path: &str, limit: i32) -> Result<DirListing, String> {
    let n = state()?;
    let c = n.core.read();
    let idx = &c.idx;
    let Some(parent) = resolve_path(idx, path) else {
        // 空目录也会 resolve 到、只是没子项；resolve 不到说明索引里没有这条路径
        // （比如普通权限下的静态快照，或索引建好之后才新建的目录）。
        return Err("索引里没有这个文件夹的内容".into());
    };
    if !idx.is_live(parent) || !idx.is_dir(parent) {
        return Err("索引里没有这个文件夹的内容".into());
    }

    let mut kids: Vec<u32> = Vec::new();
    for (i, p) in idx.parent.iter().enumerate() {
        // 先比父指针（一列连续内存，最便宜），再判可见
        if *p == parent {
            let id = i as u32;
            if idx.is_visible(id) {
                kids.push(id);
            }
        }
    }
    let total = kids.len();
    kids.sort_by_cached_key(|&id| (!idx.is_dir(id), idx.name(id).to_ascii_lowercase()));

    let cap = limit.clamp(1, 1000) as usize;
    kids.truncate(cap);
    let rows = kids.into_iter().map(|id| row_of(idx, id)).collect();
    Ok(DirListing { total: total as i32, rows })
}

// ============================================================================
// 状态
// ============================================================================

pub fn stats() -> IndexStats {
    let Some(n) = NATIVE.get() else {
        return IndexStats::default();
    };
    let c = n.core.read();
    let mem = c.idx.memory_bytes();
    IndexStats {
        ready: n.ready.load(Ordering::SeqCst),
        scanning: n.scanning.load(Ordering::SeqCst),
        file_count: c.idx.live_count() as i32,
        memory_bytes: mem as i64,
        memory_text: format!("{:.1} MB", mem as f64 / 1048576.0),
        state: if n.ready.load(Ordering::SeqCst) { 1 } else { 0 },
        progress: *n.progress.lock(),
        scanning_drives: c
            .idx
            .volumes
            .iter()
            .map(|v| v.trim_end_matches(':').to_string())
            .collect::<Vec<_>>()
            .join(","),
        db_path: n.db.lock().to_string_lossy().to_string(),
        db_version: sscore::store::VERSION as f64,
        engine_version: "sscore 0.1 (native)".into(),
        syncing: n.syncing.load(Ordering::SeqCst),
        pending_sync: 0,
        boot_done: true,
        needs_admin: !crate::shell::is_admin() && !n.ready.load(Ordering::SeqCst),
        notice: n.notice.lock().clone(),
        admin: crate::shell::is_admin(),
    }
}

// ============================================================================
// 内存
// ============================================================================

/// `PROCESS_MEMORY_COUNTERS_EX` 的字节数（EX2 多两个字段，去掉它们就是 EX）。
const CB_EX: u32 = std::mem::size_of::<ProcessMemoryCountersEx2>() as u32 - 16;

/// 老系统不认 EX2 时退化成 EX：专用工作集拿不到，就直接用工作集顶上，
/// 免得面板显示 0 MB。
fn to_proc_mem(c: &ProcessMemoryCountersEx2) -> ProcMem {
    let ws = c.working_set_size as i64;
    let private_ws = c.private_working_set_size as i64;
    ProcMem {
        ws,
        private_ws: if private_ws > 0 { private_ws } else { ws },
        peak_ws: c.peak_working_set_size as i64,
        commit: c.private_usage as i64,
    }
}

/// 读一个进程句柄的内存计数；EX2 失败就退到 EX 再试一次。
fn read_counters(h: *mut std::ffi::c_void) -> Option<ProcMem> {
    let mut c = ProcessMemoryCountersEx2 {
        cb: std::mem::size_of::<ProcessMemoryCountersEx2>() as u32,
        ..Default::default()
    };
    if unsafe { GetProcessMemoryInfo(h, &mut c, c.cb) } != 0 {
        return Some(to_proc_mem(&c));
    }
    let mut e = ProcessMemoryCountersEx2 { cb: CB_EX, ..Default::default() };
    if unsafe { GetProcessMemoryInfo(h, &mut e, e.cb) } != 0 {
        return Some(to_proc_mem(&e));
    }
    None
}

/// 本进程的内存读数。
fn process_memory() -> ProcMem {
    read_counters(unsafe { GetCurrentProcess() }).unwrap_or_default()
}

/// 指定进程的内存读数；打不开（权限/已退出）就当 0。
fn process_mem_of(pid: u32) -> ProcMem {
    let h = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid)
    };
    if h == 0 {
        return ProcMem::default();
    }
    let out = read_counters(h as *mut std::ffi::c_void);
    unsafe { CloseHandle(h) };
    out.unwrap_or_default()
}

/// 主进程 + 它的所有后代进程的工作集合计。
///
/// WebView2 的渲染/GPU/网络进程都是本进程的子进程（任务管理器里分成好几行），
/// 只看主进程会低报很多，所以按父子关系把整棵树加起来。
/// 进程树的内存分布：引擎自身 vs WebView2 界面进程。
#[derive(Default)]
struct FamilyUsage {
    /// 不含 WebView2 的合计（本进程 + 将来可能的辅助进程），工作集
    engine_bytes: i64,
    /// 同上，专用工作集（任务管理器口径）
    engine_private: i64,
    engine_procs: i32,
    /// WebView2 渲染 / GPU / 网络 / 崩溃处理进程，面板里只做说明，不计入
    webview_bytes: i64,
    webview_private: i64,
    webview_procs: i32,
}

/// WebView2 的进程可执行文件名。渲染、GPU、网络、crashpad 都是这个名字。
fn is_webview_exe(exe: &str) -> bool {
    let n = exe.to_ascii_lowercase();
    n.contains("msedgewebview2") || n.contains("webview2")
}

/// 本进程 + 后代进程的工作集，按引擎 / WebView2 分开。
///
/// 早先整棵进程树一把加起来，界面渲染（几百 MB 的 WebView2）全算成了「搜索占用」，
/// 数字既不可控也没法优化。界面属于外壳，不该混进引擎账上，所以分开统计：
/// 引擎侧算总和，WebView2 单独列出来提醒"这块没算进去"。
fn app_family_working_set() -> FamilyUsage {
    let me = unsafe { GetCurrentProcessId() };
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == SNAP_INVALID {
        let m = process_memory();
        return FamilyUsage {
            engine_bytes: m.ws,
            engine_private: m.private_ws,
            engine_procs: 1,
            ..Default::default()
        };
    }
    let mut list: Vec<(u32, u32, String)> = Vec::with_capacity(512);
    let mut e = ProcessEntry32W {
        dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
        ..Default::default()
    };
    let mut ok = unsafe { Process32FirstW(snap, &mut e) } != 0;
    while ok {
        let len = e.sz_exe_file.iter().position(|c| *c == 0).unwrap_or(e.sz_exe_file.len());
        list.push((
            e.th32_process_id,
            e.th32_parent_process_id,
            String::from_utf16_lossy(&e.sz_exe_file[..len]),
        ));
        ok = unsafe { Process32NextW(snap, &mut e) } != 0;
    }
    unsafe { CloseHandle(snap) };

    // 从本进程出发做一次 BFS，收集整棵进程树
    let mut members: Vec<u32> = vec![me];
    let mut i = 0;
    while i < members.len() {
        let p = members[i];
        for (pid, ppid, _) in list.iter() {
            if *ppid == p && !members.contains(pid) {
                members.push(*pid);
            }
        }
        i += 1;
    }

    let mut out = FamilyUsage::default();
    for pid in members {
        let exe = list.iter().find(|(p, _, _)| *p == pid).map(|x| x.2.as_str()).unwrap_or("");
        let m = process_mem_of(pid);
        if is_webview_exe(exe) {
            out.webview_bytes += m.ws;
            out.webview_private += m.private_ws;
            out.webview_procs += 1;
        } else {
            out.engine_bytes += m.ws;
            out.engine_private += m.private_ws;
            out.engine_procs += 1;
        }
    }
    out
}

fn system_memory() -> (i64, i64, i32) {
    let mut m = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        ..Default::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return (0, 0, 0);
    }
    (m.total_phys as i64, m.avail_phys as i64, m.memory_load as i32)
}

/// 「设置 → 性能与内存」的整页数据。
pub fn mem_stats() -> MemStat {
    let Some(n) = NATIVE.get() else {
        return MemStat::default();
    };
    let me = process_memory();
    let fam = app_family_working_set();
    let (sys_total, sys_avail, sys_load) = system_memory();

    let (index_bytes, parts, cache, records, dirs, hidden, tomb, bpr, vols, live_files) = {
        let c = n.core.read();
        let idx = &c.idx;
        let index_bytes = idx.memory_bytes();
        let parts: Vec<MemPart> = idx
            .memory_breakdown()
            .into_iter()
            .map(|(name, bytes)| MemPart {
                name: name.to_string(),
                bytes: bytes as i64,
                per_mille: if index_bytes == 0 {
                    0
                } else {
                    (bytes * 1000 / index_bytes) as i32
                },
            })
            .collect();
        // 运行时缓存：命中集、重排视图、按大小/时间排序的元数据
        let cache = c.hits.capacity() * std::mem::size_of::<Hit>()
            + c.view.capacity() * 4
            + c.meta.capacity() * 40;
        let live = idx.live_count();
        let dirs = idx.dir_count();
        (
            index_bytes,
            parts,
            cache,
            idx.len(),
            dirs,
            idx.hidden_count(),
            idx.tomb_count(),
            idx.bytes_per_record(),
            idx.volumes.join(", "),
            live - dirs,
        )
    };

    // 锁顺序保持 core → sync，和泵线程一致
    let (frn_map, journal) = {
        let s = n.sync.lock();
        s.iter().fold((0usize, 0usize), |(f, j), x| {
            (f + x.frn_map_bytes(), j + x.journal_bytes())
        })
    };

    let db_file = n
        .db
        .lock()
        .metadata()
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    MemStat {
        working_set: me.ws,
        private_working_set: me.private_ws,
        peak_working_set: me.peak_ws,
        private_bytes: me.commit,
        total_working_set: fam.engine_bytes,
        total_private_working_set: fam.engine_private,
        process_count: fam.engine_procs,
        webview_bytes: fam.webview_bytes,
        webview_private_bytes: fam.webview_private,
        webview_count: fam.webview_procs,
        sys_total,
        sys_avail,
        sys_load,
        index_bytes: index_bytes as i64,
        index_parts: parts,
        frn_map_bytes: frn_map as i64,
        journal_bytes: journal as i64,
        cache_bytes: cache as i64,
        db_file_bytes: db_file,
        records: records as i32,
        dirs: dirs as i32,
        files: live_files as i32,
        hidden_records: hidden as i32,
        tomb_records: tomb as i32,
        bytes_per_record: bpr,
        indexed_volumes: vols,
        load_ms: *n.load_ms.lock(),
        syncing: n.syncing.load(Ordering::SeqCst),
        sync_text: n.sync_stats.lock().summary(),
        admin: crate::shell::is_admin(),
    }
}

/// 把本进程的物理页交还给系统（`SetProcessWorkingSetSize(-1, -1)`）。
///
/// 语义要说清楚：这只是让任务管理器里的「内存」掉下来，提交量不变；
/// 下次搜索要重新缺页读入，会有一次轻微卡顿。返回释放的工作集字节数。
pub fn trim_memory() -> i64 {
    let before = process_memory().ws;
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
    let after = process_memory().ws;
    (before - after).max(0)
}

/// 同步的详细计数，供性能面板显示。
pub fn sync_text() -> String {
    match NATIVE.get() {
        Some(n) => n.sync_stats.lock().summary(),
        None => String::new(),
    }
}

pub fn drives() -> Vec<serde_json::Value> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let w: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let kind = unsafe { GetDriveTypeW(w.as_ptr()) };
        out.push(serde_json::json!({ "盘符": format!("{letter}:"), "盘类型": kind }));
    }
    out
}

// ============================================================================
// 建库
// ============================================================================

/// 把 `src` 追加到 `dst` 末尾，父指针整体平移。
fn merge(dst: &mut Index, src: Index, label: &str) -> usize {
    let vol = dst.ensure_volume(label);
    let base = dst.len() as u32;
    let n = src.len();
    for i in 0..n {
        let p = src.parent[i];
        let parent = if p == sscore::ROOT_PARENT || p as usize >= n { sscore::ROOT_PARENT } else { base + p };
        let id = dst.push_live(vol, src.frn[i], src.name_bytes(i as u32), src.is_dir(i as u32));
        dst.parent[id as usize] = parent;
        // 回收站/系统目录的隐藏位要跟着搬，否则每个盘的记录合并后就"晾"出来了。
        if src.flags[i] & sscore::store::FLAG_HIDE != 0 {
            dst.set_hidden(id, true);
        }
    }
    n
}

/// 把 `idx` 里位于这些目录下的记录全部打墓碑（含目录本身）。
/// 用计数排序建一次子节点索引，避免逐条回溯父链。
fn exclude_subtrees(idx: &mut Index, paths: &[String]) -> usize {
    let n = idx.len();
    if n == 0 || paths.is_empty() {
        return 0;
    }
    let mut roots: Vec<u32> = Vec::new();
    for p in paths {
        if let Some(id) = resolve_path(idx, p) {
            roots.push(id);
        }
    }
    if roots.is_empty() {
        return 0;
    }

    // CSR：start[i]..start[i+1] 是节点 i 的子节点在 children 里的区间
    let mut start = vec![0u32; n + 1];
    for &p in &idx.parent {
        let pi = p as usize;
        if pi < n {
            start[pi + 1] += 1;
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut cursor = start.clone();
    let mut children = vec![0u32; start[n] as usize];
    for (i, &p) in idx.parent.iter().enumerate() {
        let pi = p as usize;
        if pi < n {
            children[cursor[pi] as usize] = i as u32;
            cursor[pi] += 1;
        }
    }

    let mut stack = roots;
    let mut killed = 0usize;
    while let Some(id) = stack.pop() {
        let i = id as usize;
        if i >= n || !idx.is_live(id) {
            continue;
        }
        idx.kill(id);
        killed += 1;
        stack.extend_from_slice(&children[start[i] as usize..start[i + 1] as usize]);
    }
    killed
}

pub fn start_scan(app: AppHandle, drives: Vec<String>, excluded: Vec<String>) {
    let Ok(n) = state() else { return };
    if n.scanning.swap(true, Ordering::SeqCst) {
        return;
    }
    n.stop.store(false, Ordering::SeqCst);
    *n.progress.lock() = 0.0;

    if !crate::shell::is_admin() {
        n.scanning.store(false, Ordering::SeqCst);
        emit(
            &app,
            "tinker:scan-failed",
            serde_json::json!({ "reason": "建立索引需要读取 NTFS 主文件表，请以管理员身份运行" }),
        );
        return;
    }

    std::thread::spawn(move || {
        let Some(n) = NATIVE.get() else { return };
        emit(&app, "tinker:scan-started", serde_json::json!({ "drives": drives }));

        let mut idx = Index::default();
        let mut total = 0usize;
        let t_all = Instant::now();

        for (i, d) in drives.iter().enumerate() {
            if n.stop.load(Ordering::SeqCst) {
                n.scanning.store(false, Ordering::SeqCst);
                emit(&app, "tinker:scan-terminated", serde_json::json!({ "reason": "用户取消" }));
                return;
            }
            emit(&app, "tinker:scan-partition", serde_json::json!({ "drive": d }));
            let letter = match sscore::mft::normalize_letter(d) {
                Ok(c) => c,
                Err(_) => continue,
            };
            *n.progress.lock() = i as f64 / drives.len().max(1) as f64;

            let label = format!("{letter}:");
            match sscore::mft::enum_volume(letter) {
                Ok((recs, _ms)) => {
                    let mut part = sscore::assemble(&label, &recs);
                    exclude_subtrees(&mut part, &excluded);
                    total += merge(&mut idx, part, &label);
                }
                Err(e) => {
                    n.scanning.store(false, Ordering::SeqCst);
                    emit(&app, "tinker:scan-failed", serde_json::json!({ "reason": e.to_string() }));
                    return;
                }
            }
            *n.progress.lock() = (i + 1) as f64 / drives.len().max(1) as f64;
        }

        if idx.is_empty() {
            n.scanning.store(false, Ordering::SeqCst);
            emit(&app, "tinker:scan-failed", serde_json::json!({ "reason": "没有读到任何记录" }));
            return;
        }

        let path = n.db.lock().clone();
        let save_err = idx.save(&path).err().map(|e| e.to_string());
        let live = idx.live_count();
        let ms = t_all.elapsed().as_secs_f64() * 1000.0;

        {
            let mut c = n.core.write();
            c.idx = idx;
            c.hits.clear();
            c.view.clear();
            c.meta.clear();
            c.sql = None;
            c.places.clear();
        }
        // 索引换成新的了：旧 Syncer 的 FRN 表绑的是旧索引的下标，
        // 先摘掉，免得泵线程在新索引上按旧表写错记录。
        n.sync.lock().clear();
        n.ready.store(true, Ordering::SeqCst);
        n.scanning.store(false, Ordering::SeqCst);
        *n.notice.lock() = save_err.clone();

        emit(
            &app,
            "tinker:scan-complete",
            serde_json::json!({
                "count": live,
                "total": total,
                "elapsedMs": ms,
                "saved": save_err.is_none(),
                "error": save_err,
            }),
        );

        // 建库完成立刻把每个盘的同步都挂上，索引在后台自己保鲜。
        start_sync(app);
    });
}

pub fn stop_scan() {
    if let Some(n) = NATIVE.get() {
        n.stop.store(true, Ordering::SeqCst);
    }
}

pub fn save_db() -> Result<u64, String> {
    let n = state()?;
    let c = n.core.read();
    if c.idx.is_empty() {
        return Err("索引为空，不保存".into());
    }
    let path = n.db.lock().clone();
    let r = c.idx.save(&path).map_err(|e| e.to_string());
    drop(c);
    // 索引和断点一起落盘：只存索引不存断点，下次启动就得跳过历史，
    // 没开应用那段时间新建的文件就再也补不回来了。
    save_sync_state();
    r
}

/// 从当前索引里移除某个目录及其子树，返回移除的记录数。
pub fn remove_path(path: &str) -> i32 {
    let Some(n) = NATIVE.get() else { return 0 };
    let mut c = n.core.write();
    let killed = exclude_subtrees(&mut c.idx, &[path.to_string()]);
    if killed > 0 {
        // 目录结构变了，路径 → id 的缓存必须作废
        c.places.clear();
        c.meta.clear();
    }
    killed as i32
}

// ============================================================================
// 实时同步
// ============================================================================

/// 断点文件：和索引放在一起，记录每个卷上次读到哪一条 USN。
fn sync_state_path() -> PathBuf {
    let mut p = db_path();
    p.set_extension("sync.json");
    p
}

fn read_sync_state() -> HashMap<String, (u64, i64)> {
    let Ok(text) = std::fs::read_to_string(sync_state_path()) else {
        return HashMap::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            let id = val.get("journal").and_then(|x| x.as_u64()).unwrap_or(0);
            let usn = val.get("usn").and_then(|x| x.as_i64()).unwrap_or(0);
            out.insert(k.clone(), (id, usn));
        }
    }
    out
}

/// 把当前读到的位置落盘：下次启动就能从断点续读，
/// 而不是把「应用没开时新建的文件」全当历史丢掉。
pub fn save_sync_state() {
    let Some(n) = NATIVE.get() else { return };
    let sync = n.sync.lock();
    if sync.is_empty() {
        return;
    }
    let mut obj = serde_json::Map::new();
    for s in sync.iter() {
        obj.insert(
            format!("{}", s.journal.letter),
            serde_json::json!({ "journal": s.journal.journal_id, "usn": s.journal.next_usn }),
        );
    }
    let path = sync_state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, serde_json::Value::Object(obj).to_string());
}

/// 补读断点之后的变更。返回并入索引的事件数。
///
/// 只在「同一个 USN 日志 + 断点还在有效区间」时成立；日志被删过/回绕过就返回 0，
/// 由调用方退回「跳过历史」——那种情况下确实补不回来，只能重建。
fn catch_up(s: &mut Syncer, idx: &mut Index) -> usize {
    const MAX_EVENTS: usize = 2_000_000;
    let mut applied = 0usize;
    for _ in 0..4096 {
        match s.pump(idx, 0, 0) {
            Ok(0) => break,
            Ok(k) => {
                applied += k;
                if applied >= MAX_EVENTS {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    applied
}

/// 给索引里的**每个**卷都挂上 USN 监听，并保证后台泵线程只有一个。
///
/// 早先只给 `volumes.first()`（第一个盘）挂，索引里其它盘的新建文件永远进不来；
/// 现在按卷各挂一个 Syncer，每个 Syncer 只认自己卷的 FRN。
///
/// 重建索引后也会再调一次：这里会**重新**附着到新索引上（旧的 Syncer 连同
/// FrnIndex 一起丢掉），泵线程复用，不会出现两份线程抢着写同一个索引。
pub fn start_sync(app: AppHandle) {
    let Some(n) = NATIVE.get() else { return };
    let spawn = !n.syncing.swap(true, Ordering::SeqCst);

    // 先只在本卷范围内附着，拿好 core 写锁就放，再动 n.sync，
    // 保持「core → sync」这个唯一加锁顺序，避免和泵线程互等。
    let letters: Vec<char> = {
        let c = n.core.read();
        c.idx.volumes.iter().filter_map(|v| v.chars().next()).collect()
    };
    let saved = read_sync_state();
    let mut syncers: Vec<Syncer> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut caught_up = 0usize;
    {
        let mut c = n.core.write();
        for letter in letters {
            match Syncer::attach(&mut c.idx, letter) {
                Ok(mut s) => {
                    let key = letter.to_ascii_uppercase().to_string();
                    let resumed = match saved.get(&key) {
                        Some((id, usn)) => s.resume_from(*id, *usn),
                        None => false,
                    };
                    if resumed {
                        // 断点还在：把应用没开期间的新建/删除/改名补进来
                        caught_up += catch_up(&mut s, &mut c.idx);
                    } else {
                        // 没有断点（首次运行这一版、或断点对应的日志已被删掉）：
                        // 从日志里还留着的最早一条开始重放。USN 是有序的，
                        // 「建了又删」会按顺序抵消，「早就存在」的 CREATE 会因为
                        // FRN 已命中而被忽略，所以重放是安全的；重放不到就只能重建。
                        let first = s.journal.lowest_valid_usn;
                        if first > 0 {
                            s.journal.seek(first);
                            let n = catch_up(&mut s, &mut c.idx);
                            if n == 0 {
                                s.skip_to_end();
                            } else {
                                caught_up += n;
                            }
                        } else {
                            s.skip_to_end();
                        }
                    }
                    syncers.push(s);
                }
                Err(e) => failed.push(format!("{letter}: {e}")),
            }
        }
        if caught_up > 0 {
            // 目录结构变了：路径 → id 的缓存、搜索结果都作废
            c.places.clear();
            c.meta.clear();
            c.hits.clear();
            c.view.clear();
        }
    }

    if syncers.is_empty() {
        n.syncing.store(false, Ordering::SeqCst);
        *n.sync.lock() = Vec::new();
        let msg = failed.join("；");
        *n.notice.lock() = Some(format!("实时同步未启用：{msg}"));
        emit(&app, "tinker:sync-change", serde_json::json!({ "info": "", "error": msg }));
        return;
    }

    let names: Vec<String> = syncers.iter().map(|s| format!("{}:", s.journal.letter)).collect();
    *n.sync.lock() = syncers;
    // 这里**不能**写断点：索引要到退出/手动保存时才落盘，断点先写出去就等于
    // 宣称「索引已包含到这条 USN」，一旦进程被杀，这段区间的变更就永久丢了。
    // 断点统一由 save_db() 在索引落盘之后写。

    let info = if caught_up > 0 {
        format!("{} 实时同步已启用（补入 {caught_up} 条变更）", names.join(" / "))
    } else {
        format!("{} 实时同步已启用", names.join(" / "))
    };
    emit(&app, "tinker:sync-change", serde_json::json!({ "info": info }));
    if !failed.is_empty() {
        *n.notice.lock() = Some(format!("部分卷未同步：{}", failed.join("；")));
    }

    if !spawn {
        return;
    }

    std::thread::spawn(move || {
        let Some(n) = NATIVE.get() else { return };
        loop {
            if n.stop.load(Ordering::SeqCst) || !n.syncing.load(Ordering::SeqCst) {
                break;
            }
            let mut broken: Vec<char> = Vec::new();
            let mut lost = false;
            let mut recovered: Vec<(char, sscore::sync::Recovery)> = Vec::new();
            let mut total = SyncStats::default();
            {
                let mut sync = n.sync.lock();
                let mut c = n.core.write();
                for s in sync.iter_mut() {
                    let pumped = s.pump(&mut c.idx, 0, 0);
                    // 日志回绕/被删：pump 已经重新定位并接着读了，这里只负责告知
                    if let Some(r) = s.take_recovery() {
                        recovered.push((s.journal.letter, r));
                    }
                    if let Err(e) = pumped {
                        if sscore::sync::Journal::is_journal_lost(&e) {
                            lost = true;
                        }
                        // 走到这里说明重新定位也失败了（卷离线 / 权限不足），才是真的中断
                        *n.notice.lock() =
                            Some(format!("{}: 实时同步中断：{e}（重新挂载或重启应用后恢复）", s.journal.letter));
                        broken.push(s.journal.letter);
                    }
                    let st = &s.stats;
                    total.rounds += st.rounds;
                    total.events += st.events;
                    total.added += st.added;
                    total.removed += st.removed;
                    total.renamed += st.renamed;
                    total.revived += st.revived;
                    total.recovered += st.recovered;
                    total.skipped_usn += st.skipped_usn;
                    total.busy_ms += st.busy_ms;
                    total.last_round_us = total.last_round_us.max(st.last_round_us);
                }
                if total.added + total.removed + total.renamed > 0 {
                    // 目录结构变了：路径 → id 的缓存必须作废
                    c.places.clear();
                    c.meta.clear();
                }
            }

            // 自愈提示：告诉用户「跳过了哪一段」，别让人以为索引一直准确。
            // 只写 notice（界面顶部通知条会展示），同步本身已经恢复了，不必弹窗。
            for (letter, r) in recovered {
                let msg = if r.rebuilt {
                    format!("{letter}: USN 日志被重建，已从日志起点重新跟上；更早的变更需要重建索引")
                } else {
                    format!(
                        "{letter}: USN 日志已回绕，跳过了 USN {}–{} 这一段；实时同步已自动恢复，必要时请重建索引",
                        r.from, r.resume_at
                    )
                };
                *n.notice.lock() = Some(msg);
            }

            if !broken.is_empty() {
                // 只摘掉出问题的卷，其余卷继续保鲜
                n.sync.lock().retain(|s| !broken.contains(&s.journal.letter));
                if n.sync.lock().is_empty() {
                    n.syncing.store(false, Ordering::SeqCst);
                    emit(
                        &app,
                        "tinker:sync-change",
                        serde_json::json!({ "info": "", "lost": lost, "error": "USN 日志不可读" }),
                    );
                    break;
                }
                emit(
                    &app,
                    "tinker:sync-change",
                    serde_json::json!({ "info": "", "lost": lost, "error": "部分卷的 USN 日志不可读" }),
                );
            }
            *n.sync_stats.lock() = total;
            std::thread::sleep(Duration::from_millis(250));
        }
    });
}

pub fn shutdown() {
    if let Some(n) = NATIVE.get() {
        n.stop.store(true, Ordering::SeqCst);
        n.syncing.store(false, Ordering::SeqCst);
        let _ = save_db();
    }
}

#[cfg(test)]
mod tests {
    use super::{depth_under, init, is_webview_exe, mem_stats, trim_memory};

    /// 内存页依赖真实 Win32 调用：工作集、系统内存、进程树都得有值，
    /// 否则面板会显示一片 0。
    #[test]
    fn mem_stats_reads_process_and_system() {
        init(std::env::temp_dir().join("sscore-mem-test.idx"));
        let m = mem_stats();
        assert!(m.working_set > 0, "工作集应为正");
        assert!(m.peak_working_set >= m.working_set, "峰值不该小于当前");
        assert!(m.sys_total > 0, "系统内存总量应为正");
        assert!(m.sys_avail > 0 && m.sys_avail <= m.sys_total);
        assert!(m.process_count >= 1, "至少要有本进程");
        // 进程树合计在「整理内存」之后可能反而小于先前拍的单进程值
        // （另一个测试线程正在 trim），这里只要求它有值。
        assert!(m.total_working_set > 0);
    }

    /// 面板主数字必须是**专用工作集**（任务管理器「内存」列的口径）。
    /// 专用工作集只会小于等于工作集，差额就是与别的进程共享的 DLL/映像页。
    #[test]
    fn private_working_set_is_reported() {
        init(std::env::temp_dir().join("sscore-mem-test.idx"));
        let m = mem_stats();
        assert!(m.private_working_set > 0, "专用工作集应为正");
        assert!(m.private_working_set <= m.working_set, "专用不该大于总工作集");
        assert!(m.total_private_working_set > 0);
        assert!(m.total_private_working_set <= m.total_working_set);
    }

    /// 界面进程要能被认出来：WebView2 的渲染/GPU/网络进程都是这个名字，
    /// 主进程不能被误判成界面进程（否则面板会把引擎占用减成 0）。
    #[test]
    fn webview_exe_is_recognized() {
        assert!(is_webview_exe("msedgewebview2.exe"));
        assert!(is_webview_exe("MSEDGEWEBVIEW2.EXE"));
        assert!(!is_webview_exe("tinker-search.exe"));
        assert!(!is_webview_exe(""));
    }

    /// 面板给出的「全部进程」必须排除界面：进程树里只要有 WebView2，
    /// 这一项就该严格小于「引擎 + 界面」的总和。
    #[test]
    fn mem_stats_excludes_webview() {
        init(std::env::temp_dir().join("sscore-mem-test.idx"));
        let m = mem_stats();
        assert!(m.process_count >= 1);
        assert!(m.total_working_set > 0);
        if m.webview_count > 0 {
            // 有界面进程在跑：两个口径必须分开记
            assert!(m.webview_bytes > 0);
            assert!(m.total_working_set + m.webview_bytes > m.total_working_set);
        }
    }

    /// 整理内存返回「释放了多少」，不能是负数（失败时算 0）。
    #[test]
    fn trim_memory_returns_non_negative() {
        init(std::env::temp_dir().join("sscore-mem-test.idx"));
        assert!(trim_memory() >= 0);
    }

    /// 排序分组依赖这个层级：1 = 当前目录的直接子项，2 = 孙项……
    #[test]
    fn depth_counts_from_scope() {
        assert_eq!(depth_under(r"D:/a/b", r"D:/a/b"), 0);
        assert_eq!(depth_under(r"D:/a/b/c.txt", r"D:/a/b"), 1);
        assert_eq!(depth_under(r"D:/a/b/sub/x.txt", r"D:/a/b"), 2);
        assert_eq!(depth_under(r"D:/a/b/sub/deep/x.txt", r"D:/a/b"), 3);
        // 尾部分隔符、大小写不影响层级
        assert_eq!(depth_under(r"D:/A/B/Sub/", r"d:/a/b/"), 1);
    }

    /// D:/abc 只是和 D:/ab 字符串前缀相同，不是父子关系，不能算进当前目录。
    #[test]
    fn depth_rejects_sibling_prefixes() {
        assert_eq!(depth_under(r"D:/abc/x.txt", r"D:/ab"), usize::MAX);
        assert_eq!(depth_under(r"C:/other/a.txt", r"D:/a"), usize::MAX);
        assert_eq!(depth_under(r"D:/a/b", ""), usize::MAX);
    }
}

#[cfg(test)]
mod browse_order_tests {
    use super::entry_group;
    use sscore::{Index, IndexBuilder, ROOT_PARENT};

    /// D:/root
    ///   ├─ sub\           (文件夹，应为第 1 组)
    ///   ├─ a.txt          (文件，第 2 组)
    ///   ├─ Zed.txt        (文件，第 2 组)
    ///   └─ sub\b.txt      (更深层，第 3 组)
    fn tree() -> Index {
        let mut b = IndexBuilder::new();
        let vol = b.add_volume("D:");
        let root = b.push_str(vol, "root", true);
        b.set_parent(root, ROOT_PARENT);
        let sub = b.push_str(vol, "sub", true);
        b.set_parent(sub, root);
        let a = b.push_str(vol, "a.txt", false);
        b.set_parent(a, root);
        let z = b.push_str(vol, "Zed.txt", false);
        b.set_parent(z, root);
        let deep = b.push_str(vol, "b.txt", false);
        b.set_parent(deep, sub);
        b.finish()
    }

    /// 索引里的路径是反斜杠，apply_sort 也会先规范化，这里保持一致。
    fn scope() -> String {
        "D:/root".replace('/', "\\")
    }

    /// 与 apply_sort 相同的两步：先按字段排（这里用文件名升序），再稳定地按分组排。
    fn view(idx: &Index, ids: &[u32]) -> Vec<String> {
        let s = scope();
        let mut v = ids.to_vec();
        v.sort_by_cached_key(|&id| (idx.name(id).to_lowercase(), id));
        v.sort_by_key(|&id| entry_group(idx, &s, id));
        v.iter().map(|&id| idx.path_of(id)).collect()
    }

    /// 搜索/浏览结果里不含「当前目录本身」，所以从 1 开始取。
    fn children(idx: &Index) -> Vec<u32> {
        (1..idx.len() as u32).collect()
    }

    #[test]
    fn folders_then_files_then_deeper() {
        let idx = tree();
        assert_eq!(
            view(&idx, &children(&idx)),
            vec![
                "D:/root/sub".replace('/', "\\"),
                "D:/root/a.txt".replace('/', "\\"),
                "D:/root/Zed.txt".replace('/', "\\"),
                "D:/root/sub/b.txt".replace('/', "\\"),
            ]
        );
    }

    /// 分组是稳定排序，组内必须保持用户选的字段顺序（含降序）。
    #[test]
    fn group_keeps_inner_order() {
        let idx = tree();
        let s = scope();
        let mut v = children(&idx);
        // 名字降序
        v.sort_by_cached_key(|&id| std::cmp::Reverse(idx.name(id).to_lowercase()));
        v.sort_by_key(|&id| entry_group(&idx, &s, id));
        let names: Vec<&str> = v.iter().map(|&id| idx.name(id)).collect();
        assert_eq!(names, vec!["sub", "Zed.txt", "a.txt", "b.txt"]);
    }

    /// 没有进入目录（scope 为空）时不分组，完全按字段顺序。
    #[test]
    fn no_scope_keeps_plain_order() {
        let idx = tree();
        let mut v = children(&idx);
        v.sort_by_cached_key(|&id| idx.name(id).to_lowercase());
        let names: Vec<&str> = v.iter().map(|&id| idx.name(id)).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "sub", "Zed.txt"]);
    }
}

#[cfg(test)]
mod nl_search_tests {
    use super::*;
    use crate::nlu::NluQuery;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::time::SystemTime;

    fn core_with(idx: Index) -> Core {
        Core {
            idx,
            hits: Vec::new(),
            view: Vec::new(),
            fp: 0,
            keyword: String::new(),
            scope: None,
            filter: Filter::default(),
            places: HashMap::new(),
            sql: None,
            sort_field: "相关度".into(),
            sort_asc: false,
            meta: HashMap::new(),
            mode: 0,
        }
    }

    /// 建一个真实文件并把修改时间往前调，用于验证时间过滤
    fn touch(p: &Path, days_ago: u64) {
        let mut f = fs::File::create(p).unwrap();
        f.write_all(b"x").unwrap();
        let t = SystemTime::now() - Duration::from_secs(days_ago * 86_400);
        f.set_modified(t).unwrap();
    }

    #[test]
    fn nl_type_and_time_filter_end_to_end() {
        let root = std::env::temp_dir().join(format!("tinker-nl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        touch(&root.join("今天.pdf"), 0);
        touch(&root.join("上个月.pdf"), 40);
        touch(&root.join("今天.docx"), 0);
        touch(&root.join("sub").join("深层.pdf"), 0);

        // 根记录的名字就是磁盘路径，这样 path_of() 拼出来的才是真实路径，
        // 后面按时间过滤时的 stat 才能命中文件
        let label = root.to_string_lossy().to_string();
        let (recs, _) = sscore::walk::walk(&root, &label, 8).unwrap();
        let idx = sscore::assemble(&label, &recs);
        let mut c = core_with(idx);

        // 只按类型：pdf 有 3 个（含子目录里的）
        let q = NluQuery { exts: vec!["pdf".into()], ..Default::default() };
        let (count, _) = nl_run(&mut c, &q, None, &[], &[]).unwrap();
        assert_eq!(count, 3, "pdf 应当有 3 个（含子目录）");

        // 类型 + 最近一天修改：40 天前那个要被滤掉
        let now = chrono::Utc::now().timestamp_millis();
        let q = NluQuery {
            exts: vec!["pdf".into()],
            field: "mtime".into(),
            from_ms: now - 86_400_000,
            to_ms: now + 60_000,
            label: "今天".into(),
            ..Default::default()
        };
        let (count, scanned) = nl_run(&mut c, &q, None, &[], &[]).unwrap();
        assert_eq!(scanned, 3, "过滤前候选是 3 个 pdf");
        assert_eq!(count, 2, "40 天前的 pdf 应当被时间条件滤掉");

        // 再加关键词：只剩「今天.pdf」
        let q = NluQuery {
            keyword: "今天".into(),
            exts: vec!["pdf".into()],
            field: "mtime".into(),
            from_ms: now - 86_400_000,
            to_ms: now + 60_000,
            ..Default::default()
        };
        let (count, _) = nl_run(&mut c, &q, None, &[], &[]).unwrap();
        assert_eq!(count, 1, "关键词应当把范围收到一个文件");

        let _ = fs::remove_dir_all(&root);
    }

    /// 端到端：一句「上个月改过的pdf」→ 解析 → 真的落盘按时间筛。
    /// 只测解析不够，过滤链路（stat + 字段 + 半开区间）也要一起过。
    #[test]
    fn nl_parse_last_month_then_filter() {
        use chrono::{Datelike, Months};

        let cfg: crate::nlu::NluCfg = {
            let v: serde_json::Value =
                serde_json::from_str(include_str!("../assets/plugins/tinker.nl-zh.json")).unwrap();
            serde_json::from_value(v["nlu"].clone()).unwrap()
        };

        let root = std::env::temp_dir().join(format!("tinker-nl-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        touch(&root.join("上个月.pdf"), 0);
        touch(&root.join("今天.pdf"), 0);
        touch(&root.join("上个月.docx"), 0);

        // 把「上个月.pdf」的修改时间挪到上个月中旬（ctime 改不了，所以测 mtime）
        let first = chrono::Local::now().with_day(1).unwrap();
        let mid = first - Months::new(1) + chrono::Duration::days(10);
        let st = std::time::UNIX_EPOCH + Duration::from_millis(mid.timestamp_millis() as u64);
        fs::File::options()
            .write(true)
            .open(root.join("上个月.pdf"))
            .unwrap()
            .set_modified(st)
            .unwrap();

        let label = root.to_string_lossy().to_string();
        let (recs, _) = sscore::walk::walk(&root, &label, 8).unwrap();
        let mut c = core_with(sscore::assemble(&label, &recs));

        let q = crate::nlu::parse("上个月改过的pdf", &cfg, chrono::Local::now());
        assert_eq!(q.field, "mtime");
        assert_eq!(q.exts, vec!["pdf"]);
        let (count, scanned) = nl_run(&mut c, &q, None, &[], &[]).unwrap();
        assert_eq!(scanned, 2, "候选是两个 pdf");
        assert_eq!(count, 1, "只有挪到上个月的那个 pdf 该留下");

        let _ = fs::remove_dir_all(&root);
    }
}
