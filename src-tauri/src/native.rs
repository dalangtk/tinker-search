//! 索引内核接入层：sscore 引擎 + NTFS USN 实时同步。
//!
//! - 索引完全在内存里，查询直接扫名字池，不碰磁盘；
//! - 用卷级 USN 日志做增量同步，不需要给每个目录开句柄；
//! - 不依赖任何外部 DLL。
//!
//! 能力边界：建库 / 搜索 / 分类筛选 / 目录范围 / SQL / 实时同步。
//! 没有 Lua 脚本、图标缓存、别名与评分，相应命令在命令层返回空结果或 no-op。

use crate::model::{IndexStats, Row, SearchStart};
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
}

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
    sync: Mutex<Option<Syncer>>,
    db: Mutex<PathBuf>,
    ready: AtomicBool,
    scanning: AtomicBool,
    syncing: AtomicBool,
    stop: AtomicBool,
    /// 建库进度 0..1
    progress: Mutex<f64>,
    notice: Mutex<Option<String>>,
    sync_stats: Mutex<SyncStats>,
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
        sync: Mutex::new(None),
        db: Mutex::new(db),
        ready: AtomicBool::new(false),
        scanning: AtomicBool::new(false),
        syncing: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        progress: Mutex::new(0.0),
        notice: Mutex::new(None),
        sync_stats: Mutex::new(SyncStats::default()),
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
        Ok(idx) => {
            let live = idx.live_count();
            let ms = t.elapsed().as_millis();
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
            let first = {
                let c = n.core.read();
                c.idx.volumes.first().and_then(|v| v.chars().next())
            };
            if let Some(letter) = first {
                start_sync(app.clone(), letter);
            }
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

        // 建库完成立刻把第一个盘的同步挂上，索引在后台自己保鲜。
        if let Some(first) = drives.first() {
            if let Ok(letter) = sscore::mft::normalize_letter(first) {
                start_sync(app, letter);
            }
        }
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
    c.idx.save(&path).map_err(|e| e.to_string())
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

/// 给第一个盘挂上 USN 监听。已经在跑就直接复用。
pub fn start_sync(app: AppHandle, letter: char) {
    let Some(n) = NATIVE.get() else { return };
    if n.syncing.swap(true, Ordering::SeqCst) {
        return;
    }

    {
        let mut c = n.core.write();
        match Syncer::attach(&mut c.idx, letter) {
            Ok(mut s) => {
                // 快照就是当前状态，历史变更不必重放。
                s.skip_to_end();
                *n.sync.lock() = Some(s);
            }
            Err(e) => {
                n.syncing.store(false, Ordering::SeqCst);
                *n.notice.lock() = Some(format!("实时同步未启用：{e}"));
                emit(&app, "tinker:sync-change", serde_json::json!({ "info": "", "error": e.to_string() }));
                return;
            }
        }
    }

    emit(&app, "tinker:sync-change", serde_json::json!({ "info": format!("{letter}: 实时同步已启用") }));

    std::thread::spawn(move || {
        let Some(n) = NATIVE.get() else { return };
        let mut lost = false;
        loop {
            if n.stop.load(Ordering::SeqCst) || !n.syncing.load(Ordering::SeqCst) {
                break;
            }
            let mut abnormal = false;
            {
                let mut sync = n.sync.lock();
                if let Some(s) = sync.as_mut() {
                    let mut c = n.core.write();
                    match s.pump(&mut c.idx, 0, 0) {
                        Ok(_) => {}
                        Err(e) => {
                            abnormal = true;
                            if sscore::sync::Journal::is_journal_lost(&e) {
                                lost = true;
                            }
                            *n.notice.lock() = Some(format!("实时同步中断：{e}"));
                        }
                    }
                    let changed = s.stats.added + s.stats.removed + s.stats.renamed;
                    if changed > 0 {
                        // 目录结构变了：路径 → id 的缓存必须作废
                        c.places.clear();
                        c.meta.clear();
                    }
                    *n.sync_stats.lock() = s.stats.clone();
                }
            }
            if abnormal {
                n.syncing.store(false, Ordering::SeqCst);
                emit(
                    &app,
                    "tinker:sync-change",
                    serde_json::json!({ "info": "", "lost": lost, "error": "USN 日志不可读" }),
                );
                break;
            }
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
    use super::depth_under;

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
