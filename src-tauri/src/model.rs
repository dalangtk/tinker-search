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
