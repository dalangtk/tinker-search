//! 前后端共享的数据模型。
//!
//! 这些类型由命令层序列化给前端，单独放在这里，避免依赖任何具体后端。

use serde::Serialize;

/// 一条搜索结果。字段名与前端 `Row` 保持一致。
#[derive(Serialize, Clone, Debug, Default)]
pub struct Row {
    pub id: i32,
    pub name: String,
    pub path: String,
    pub parent: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    pub ctime: i64,
    pub atime: i64,
    pub is_dir: bool,
    pub file_type: String,
    pub alias: String,
    pub rating: i16,
    pub attrs: i32,
    pub hl_file: Vec<String>,
    pub hl_dir: Vec<String>,
    pub hl_alias: Vec<String>,
}

/// 一个目录的**直接子项**（预览面板用）。`total` 是全部子项数，
/// `rows` 只带前一段，用来判断「还有更多」。
#[derive(Serialize, Clone, Debug, Default)]
pub struct DirListing {
    pub total: i32,
    pub rows: Vec<Row>,
}

/// 一次搜索提交的结果：`fingerprint` 用于把异步事件与本次请求对上。
#[derive(Serialize, Clone, Debug)]
pub struct SearchStart {
    pub fingerprint: i32,
    pub accepted: bool,
}
/// 索引状态快照，供状态栏与「设置 → 索引管理」使用。
#[derive(Serialize, Clone, Debug, Default)]
pub struct IndexStats {
    pub ready: bool,
    pub scanning: bool,
    pub file_count: i32,
    pub memory_bytes: i64,
    pub memory_text: String,
    pub state: i32,
    pub progress: f64,
    pub scanning_drives: String,
    pub db_path: String,
    pub db_version: f64,
    pub engine_version: String,
    pub syncing: bool,
    pub pending_sync: i32,
    /// 启动阶段的建库/加载尝试已结束
    pub boot_done: bool,
    /// 没索引又不是管理员 —— 界面应引导提权
    pub needs_admin: bool,
    /// 启动阶段要告诉用户的一句话（加载失败原因等）
    pub notice: Option<String>,
    /// 当前进程是否管理员：只有建库与实时同步需要，搜索/预览/文件操作都不需要
    pub admin: bool,
}

/// 内存明细里的一项（某一列 / 某块缓存）。
#[derive(Serialize, Clone, Debug, Default)]
pub struct MemPart {
    pub name: String,
    pub bytes: i64,
    /// 占索引内存的千分比整数（前端画占比条直接用它，避免浮点误差）
    pub per_mille: i32,
}

/// 「设置 → 性能与内存」整页的数据快照。
///
/// 单位统一用字节，格式化交给前端——后端只保证数值真实可核对。
#[derive(Serialize, Clone, Debug, Default)]
pub struct MemStat {
    // ---- 进程 ----
    /// 主进程专用工作集：只算独占的物理页，**和任务管理器「内存」列同口径**
    pub private_working_set: i64,
    /// 主进程工作集：含与其他进程共享的 DLL / 映像页，所以总比上面那个大
    pub working_set: i64,
    pub peak_working_set: i64,
    /// 主进程私有提交（任务管理器「提交大小」）
    pub private_bytes: i64,
    /// 主进程 + 辅助进程的工作集合计，**不含 WebView2 界面进程**
    pub total_working_set: i64,
    /// 同上，专用工作集口径
    pub total_private_working_set: i64,
    pub process_count: i32,
    /// WebView2 界面进程（渲染/GPU/网络）合计，只做说明，不计入上面两项
    pub webview_bytes: i64,
    /// 同上，专用工作集口径
    pub webview_private_bytes: i64,
    pub webview_count: i32,
    // ---- 系统 ----
    pub sys_total: i64,
    pub sys_avail: i64,
    pub sys_load: i32,
    // ---- 索引 ----
    pub index_bytes: i64,
    pub index_parts: Vec<MemPart>,
    /// 同步用的「FRN → 记录下标」映射表
    pub frn_map_bytes: i64,
    /// 每个卷的 USN 读缓冲
    pub journal_bytes: i64,
    /// 命中集 / 重排视图 / 元数据等运行时缓存
    pub cache_bytes: i64,
    pub db_file_bytes: i64,
    pub records: i32,
    pub dirs: i32,
    pub files: i32,
    pub hidden_records: i32,
    pub tomb_records: i32,
    pub bytes_per_record: f64,
    // ---- 状态 ----
    pub indexed_volumes: String,
    pub load_ms: i64,
    pub syncing: bool,
    pub sync_text: String,
    pub admin: bool,
}
