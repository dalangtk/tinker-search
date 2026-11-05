//! Tauri 命令层：前端唯一可触达的入口，全部返回 `Result<T, String>`。

use crate::config::{match_json, AppConfig};
use crate::model::{IndexStats, Row, SearchStart};
use crate::{hotkey, native, plugins, shell, vfs};
use parking_lot::Mutex;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};

/// 保存索引（托盘菜单与退出流程共用）。
pub fn save_index() -> Result<(), String> {
    native::save_db().map(|_| ())
}

/// 停机落盘。
pub fn shutdown_engine() {
    native::shutdown();
}

pub struct AppState {
    pub cfg: Mutex<AppConfig>,
    pub cfg_path: PathBuf,
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn get(&self) -> AppConfig {
        self.cfg.lock().clone()
    }
    pub fn save(&self) {
        let _ = self.cfg.lock().save(&self.cfg_path);
    }
    /// 就地修改配置并落盘。注意 get() 返回的是克隆，改完不写回就等于没改。
    pub fn update<F: FnOnce(&mut AppConfig)>(&self, f: F) {
        let mut c = self.cfg.lock();
        f(&mut c);
        let _ = c.save(&self.cfg_path);
    }
}

fn blocking<T, F>(f: F) -> impl std::future::Future<Output = Result<T, String>>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    async move {
        tauri::async_runtime::spawn_blocking(f)
            .await
            .map_err(|e| format!("后台任务失败：{e}"))?
    }
}

// ============================================================================
// 基础信息
// ============================================================================

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> serde_json::Value {
    let st = native::stats();
    let cfg = state.get();
    serde_json::json!({
        "admin": shell::is_admin(),
        "engineVersion": st.engine_version,
        "dbVersion": st.db_version,
        "dbPath": st.db_path,
        "dataDir": state.data_dir.to_string_lossy(),
        "cfgPath": state.cfg_path.to_string_lossy(),
        "dbExists": native::db_path().exists(),
        "config": cfg,
    })
}

#[tauri::command]
pub fn engine_ready() -> bool {
    native::is_ready()
}

// ============================================================================
// 搜索
// ============================================================================

/// 搜索入口。`places` / `drives` 是左侧分类栏里「常用位置」和「磁盘」的选中项，
/// 二者都会落成目录范围；「文件类型」走 `set_filter`。
#[tauri::command]
pub async fn search(
    app: AppHandle,
    keyword: String,
    mode: i32,
    scope: Option<String>,
    places: Option<Vec<String>>,
    drives: Option<Vec<String>>,
) -> Result<SearchStart, String> {
    let places = places.unwrap_or_default();
    let drives = drives.unwrap_or_default();
    blocking(move || native::search(&app, &keyword, mode, scope.as_deref(), &places, &drives)).await
}

#[tauri::command]
pub fn cancel_search(fingerprint: i32) {
    native::cancel_search(fingerprint);
}

// ============================================================================
// 自然语言搜索（由插件提供词表，宿主提供引擎）
// ============================================================================

/// 自然语言模式是否可用。没装插件或插件没启用时返回 `ready:false`，界面据此隐藏入口。
#[tauri::command]
pub fn nl_status(state: State<'_, AppState>) -> serde_json::Value {
    match plugins::active_nlu(&state.data_dir) {
        Some((m, e)) => serde_json::json!({
            "ready": true,
            "plugin": m.id,
            "name": m.name,
            "version": m.version,
            "engine": e,
        }),
        None => serde_json::json!({ "ready": false }),
    }
}

/// 只解析不搜索，用于输入时的实时提示（「已理解为：类型 pdf / 创建时间 今天」）。
#[tauri::command]
pub fn nl_parse(state: State<'_, AppState>, text: String) -> Result<serde_json::Value, String> {
    let (m, e) = plugins::active_nlu(&state.data_dir).ok_or("未启用自然语言搜索插件")?;
    let q = plugins::parse(&m, &e, &text);
    serde_json::to_value(q).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn nl_search(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    scope: Option<String>,
    places: Option<Vec<String>>,
    drives: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let (m, e) = plugins::active_nlu(&state.data_dir).ok_or("未启用自然语言搜索插件")?;
    let q = plugins::parse(&m, &e, &text);
    let places = places.unwrap_or_default();
    let drives = drives.unwrap_or_default();
    let app2 = app.clone();
    blocking(move || {
        let (start, count, scanned) = native::nl_search(&app2, &q, scope.as_deref(), &places, &drives)?;
        Ok(serde_json::json!({
            "fingerprint": start.fingerprint,
            "accepted": start.accepted,
            "count": count,
            "scanned": scanned,
            "nl": q,
        }))
    })
    .await
}

// ============================================================================
// 插件管理
// ============================================================================

#[tauri::command]
pub fn plugins_list(state: State<'_, AppState>) -> Vec<plugins::PluginView> {
    plugins::list_in(&state.data_dir, &state.get().plugins.market_url)
}

/// 插件市场索引。`refresh` 为 true 时忽略 10 分钟缓存重新拉取。
#[tauri::command]
pub async fn plugin_market(state: State<'_, AppState>, refresh: Option<bool>) -> Result<Vec<plugins::MarketEntry>, String> {
    let url = state.get().plugins.market_url;
    let dir = state.data_dir.clone();
    // 强行刷新就先把缓存戳删掉
    if refresh.unwrap_or(false) {
        let _ = std::fs::remove_file(dir.join("market").join("index.json"));
    }
    blocking(move || Ok(plugins::market(&dir, &url))).await
}

#[tauri::command]
pub fn plugin_install(state: State<'_, AppState>, id: String) -> Result<plugins::PluginView, String> {
    let entry = plugins::market(&state.data_dir, &state.get().plugins.market_url)
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| "市场里没有这个插件".to_string())?;
    if let Some(rest) = entry.download.strip_prefix("builtin:") {
        plugins::install_bundled(&state.data_dir, rest)
    } else {
        plugins::install_url(&state.data_dir, &entry.download)
    }
}

#[tauri::command]
pub fn plugin_install_url(state: State<'_, AppState>, url: String) -> Result<plugins::PluginView, String> {
    plugins::install_url(&state.data_dir, &url)
}

#[tauri::command]
pub fn plugin_update(state: State<'_, AppState>, id: String) -> Result<plugins::PluginView, String> {
    let url = state.get().plugins.market_url;
    plugins::update(&state.data_dir, &id, &url)
}

#[tauri::command]
pub fn plugin_set_enabled(state: State<'_, AppState>, id: String, enabled: bool) -> Result<(), String> {
    plugins::set_enabled(&state.data_dir, &id, enabled)
}

#[tauri::command]
pub fn plugin_uninstall(state: State<'_, AppState>, id: String) -> Result<(), String> {
    plugins::uninstall(&state.data_dir, &id)
}

/// 改插件的解析引擎：`rules` 用本地规则，`http` 转发到任意 OpenAI 兼容端点。
#[tauri::command]
pub fn plugin_set_engine(
    state: State<'_, AppState>,
    id: String,
    provider: String,
    model: String,
    endpoint: String,
) -> Result<(), String> {
    plugins::set_engine(&state.data_dir, &id, plugins::EngineCfg { provider, model, endpoint })
}

#[tauri::command]
pub fn plugin_market_url(state: State<'_, AppState>) -> String {
    state.get().plugins.market_url
}

#[tauri::command]
pub fn set_plugin_market_url(state: State<'_, AppState>, url: String) -> Result<(), String> {
    let url = url.trim().to_string();
    if !url.is_empty() && !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("市场地址必须以 http:// 或 https:// 开头".into());
    }
    state.update(|c| c.plugins.market_url = url);
    Ok(())
}

#[tauri::command]
pub async fn get_rows(start: i32, count: i32) -> Result<Vec<Row>, String> {
    blocking(move || native::rows(start, count)).await
}

#[tauri::command]
pub async fn get_row(id: i32) -> Result<Row, String> {
    blocking(move || native::row_by_id(id)).await
}

/// 文件图标由系统 Shell 提供，内核不维护图标缓存，直接返回空让前端回退到通用图标。
#[tauri::command]
pub async fn get_icons(_ids: Vec<i32>, _size: i32) -> Result<Vec<serde_json::Value>, String> {
    Ok(Vec::new())
}

#[tauri::command]
pub async fn get_preview_icon(_id: i32, _size: i32) -> Result<Option<String>, String> {
    Ok(None)
}

#[tauri::command]
pub fn get_filters() -> Vec<serde_json::Value> {
    native::filters()
}

/// 结果列表支持的排序字段：相关度 / 名称 / 位置 / 大小 / 时间 / 类型等。
#[tauri::command]
pub fn get_sort_fields() -> Vec<String> {
    native::sort_fields()
}

#[tauri::command]
pub async fn set_sort(
    app: AppHandle,
    state: State<'_, AppState>,
    field: String,
    asc: bool,
) -> Result<(), String> {
    native::set_sort(&app, &field, asc)?;
    state.update(|c| {
        c.sort.field = field;
        c.sort.asc = asc;
    });
    Ok(())
}

#[tauri::command]
pub async fn set_filter(state: State<'_, AppState>, names: Vec<String>) -> Result<(), String> {
    state.update(|c| c.filter = names.clone());
    native::set_filter(&names);
    Ok(())
}

#[tauri::command]
pub async fn set_match(state: State<'_, AppState>, search: crate::config::MatchSettings) -> Result<(), String> {
    state.update(|c| c.search = search);
    Ok(())
}

#[tauri::command]
pub fn match_settings_json(state: State<'_, AppState>) -> String {
    match_json(&state.get().search)
}

/// 加分规则由索引内核内部评分，暂不支持外部加权。
#[tauri::command]
pub async fn add_boost(_kind: String, _value: String, _delta: i16) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn sql_result(start: i32, max: i32) -> Result<serde_json::Value, String> {
    blocking(move || {
        Ok(native::sql_result(start, max).unwrap_or(serde_json::Value::Null))
    })
    .await
}

/// 首页快照：磁盘（含容量）+ 常用位置 + 桌面内容 + 各盘根目录。
#[tauri::command]
pub async fn home_snapshot() -> Result<serde_json::Value, String> {
    blocking(|| {
        let mut v = serde_json::to_value(vfs::home(60, 24)).unwrap_or_default();
        // 给每块磁盘标上「索引里有没有」：没收录的盘点了只会得到 0 条，
        // 界面上得说清楚是「还没建索引」而不是「这个盘是空的」。
        let indexed = native::indexed_volumes();
        if let Some(drives) = v.get_mut("drives").and_then(|d| d.as_array_mut()) {
            for d in drives.iter_mut() {
                let letter = d
                    .get("letter")
                    .and_then(|l| l.as_str())
                    .unwrap_or_default()
                    .trim_end_matches(':')
                    .to_ascii_uppercase();
                let hit = indexed
                    .iter()
                    .any(|v| v.trim_end_matches(':').eq_ignore_ascii_case(&letter));
                if let Some(o) = d.as_object_mut() {
                    o.insert("indexed".into(), serde_json::Value::Bool(hit));
                }
            }
        }
        Ok(v)
    })
    .await
}

/// 侧栏用的常用位置（不含目录内容，轻量）。
#[tauri::command]
pub fn known_places() -> Vec<serde_json::Value> {
    vfs::places()
        .into_iter()
        .filter_map(|p| serde_json::to_value(p).ok())
        .collect()
}

/// SQL 模式的语法提示。
#[tauri::command]
pub fn engine_prompt(_kind: i32) -> String {
    "SELECT FName, Path, Ext, Size FROM alltable WHERE FName LIKE '%关键词%' ORDER BY FName LIMIT 100".into()
}

// ============================================================================
// 索引管理
// ============================================================================

#[tauri::command]
pub fn index_status() -> IndexStats {
    native::stats()
}

#[tauri::command]
pub fn list_drives() -> Vec<serde_json::Value> {
    native::drives()
}

#[tauri::command]
pub async fn start_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    drives: Vec<String>,
) -> Result<(), String> {
    let excluded = state.get().index.excluded_dirs;
    state.update(|c| c.index.drives = drives.clone());
    native::start_scan(app, drives, excluded);
    Ok(())
}

#[tauri::command]
pub async fn stop_scan() -> Result<(), String> {
    native::stop_scan();
    Ok(())
}

#[tauri::command]
pub async fn save_db() -> Result<String, String> {
    blocking(|| {
        let bytes = native::save_db()?;
        Ok(format!(
            "索引已保存：{}（{:.1} MB）",
            native::db_path().display(),
            bytes as f64 / 1048576.0
        ))
    })
    .await
}

#[tauri::command]
pub fn excluded_dirs(state: State<'_, AppState>) -> Vec<String> {
    state.get().index.excluded_dirs
}

#[tauri::command]
pub async fn add_excluded(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state.update(|c| {
        if !c.index.excluded_dirs.iter().any(|p| p.eq_ignore_ascii_case(&path)) {
            c.index.excluded_dirs.push(path);
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn remove_excluded(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state.update(|c| c.index.excluded_dirs.retain(|p| !p.eq_ignore_ascii_case(&path)));
    Ok(())
}

/// 从当前索引里移除某个目录及其子树，返回移除的记录数。
#[tauri::command]
pub async fn remove_index_path(path: String) -> Result<i32, String> {
    blocking(move || Ok(native::remove_path(&path))).await
}

#[tauri::command]
pub async fn perf_text() -> Result<serde_json::Value, String> {
    // 前端只把它当一段可复制的诊断文本，键名保持兼容。
    blocking(|| {
        Ok(serde_json::json!({
            "load": "",
            "scan": "",
            "sync": native::sync_text(),
        }))
    })
    .await
}

/// 内核最近一次错误（「关于」页诊断用）。原生内核的错误通过事件与 notice 传递。
#[tauri::command]
pub fn engine_last_error() -> String {
    String::new()
}

/// 别名与评分依赖可写的索引元数据，当前内核不维护，明确报错而不是假装成功。
#[tauri::command]
pub async fn set_alias(_id: i32, _alias: Option<String>) -> Result<(), String> {
    Err("当前索引内核不支持文件别名".into())
}

#[tauri::command]
pub async fn boost_rating(_id: i32, _delta: i16) -> Result<i16, String> {
    Err("当前索引内核不支持文件评分".into())
}

// ============================================================================
// 文件操作
// ============================================================================

#[tauri::command]
pub async fn open_file(path: String) -> Result<(), String> {
    blocking(move || shell::open_path(&path)).await
}

/// 解析 .lnk 的目标，供前端决定「在应用内进入目录」还是「用系统程序打开」。
/// 不是快捷方式或解析失败时返回 null，前端退回 `open_file`。
#[tauri::command]
pub fn resolve_link(path: String) -> Option<serde_json::Value> {
    let target = shell::resolve_shortcut(&path)?;
    let is_dir = std::fs::metadata(&target).map(|m| m.is_dir()).unwrap_or(false);
    let target_text = target.to_string_lossy().to_string();
    Some(serde_json::json!({
        "target": target_text,
        "isDir": is_dir,
        "indexed": is_dir && native::can_browse(&target_text),
    }))
}

#[tauri::command]
pub fn reveal_file(path: String) -> Result<(), String> {
    shell::reveal(&path)
}

#[tauri::command]
pub fn file_properties(path: String) -> Result<(), String> {
    shell::properties(&path)
}

#[tauri::command]
pub async fn delete_files(paths: Vec<String>) -> Result<(), String> {
    blocking(move || shell::recycle(&paths)).await
}

#[tauri::command]
pub async fn rename_file(path: String, new_name: String) -> Result<String, String> {
    blocking(move || shell::rename(&path, &new_name)).await
}

#[tauri::command]
pub fn copy_paths(paths: Vec<String>) -> Result<(), String> {
    shell::copy_files_to_clipboard(&paths)
}

#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    shell::copy_text_to_clipboard(&text)
}

#[tauri::command]
pub fn open_dir(kind: String, state: State<'_, AppState>) -> Result<(), String> {
    let dir = match kind.as_str() {
        "config" => state
            .cfg_path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        "db" => native::db_path()
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        _ => state.data_dir.to_string_lossy().to_string(),
    };
    shell::open_dir(&dir);
    Ok(())
}

// ============================================================================
// 配置 / 历史
// ============================================================================

#[tauri::command]
pub fn config_get(state: State<'_, AppState>) -> AppConfig {
    state.get()
}

#[tauri::command]
pub async fn config_save(
    state: State<'_, AppState>,
    cfg: AppConfig,
) -> Result<(), String> {
    let filter = cfg.filter.clone();
    {
        let mut cur = state.cfg.lock();
        *cur = cfg.clone();
    }
    state.save();
    // 设置面板改完快捷键立即生效，不用重启
    if !hotkey::apply(&cfg.window.shortcut, cfg.window.double_ctrl) {
        hotkey::apply("DoubleCtrl", cfg.window.double_ctrl);
    }
    // 侧栏筛选与内核保持同步
    native::set_filter(&filter);
    Ok(())
}

#[tauri::command]
pub fn history_add(state: State<'_, AppState>, keyword: String) -> Vec<String> {
    let k = keyword.trim().to_string();
    if k.is_empty() {
        return state.get().history;
    }
    let mut cfg = state.cfg.lock();
    cfg.history.retain(|h| h != &k);
    cfg.history.insert(0, k);
    if cfg.history.len() > 200 {
        cfg.history.truncate(200);
    }
    let list = cfg.history.clone();
    drop(cfg);
    state.save();
    list
}

#[tauri::command]
pub fn history_remove(state: State<'_, AppState>, keyword: String) -> Vec<String> {
    let mut cfg = state.cfg.lock();
    cfg.history.retain(|h| h != &keyword);
    let list = cfg.history.clone();
    drop(cfg);
    state.save();
    list
}

#[tauri::command]
pub fn history_clear(state: State<'_, AppState>) -> Vec<String> {
    {
        let mut cfg = state.cfg.lock();
        cfg.history.clear();
    }
    state.save();
    Vec::new()
}

// ============================================================================
// 窗口 / 生命周期
// ============================================================================

#[tauri::command]
pub fn set_always_on_top(app: AppHandle, on: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(on);
    }
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

#[tauri::command]
pub fn minimize_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.minimize();
    }
}

#[tauri::command]
pub fn toggle_maximize(app: AppHandle) -> bool {
    if let Some(w) = app.get_webview_window("main") {
        let max = w.is_maximized().unwrap_or(false);
        if max {
            let _ = w.unmaximize();
            false
        } else {
            let _ = w.maximize();
            true
        }
    } else {
        false
    }
}

#[tauri::command]
pub async fn app_exit(app: AppHandle) -> Result<(), String> {
    let _ = app.emit("tinker:exiting", serde_json::json!({}));
    tauri::async_runtime::spawn_blocking(shutdown_engine)
        .await
        .ok();
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn elevate(app: AppHandle) -> Result<(), String> {
    // --elevated 跳过单实例检查；--wait-pid 让新实例等本进程真正退出后再建 WebView2
    let args = format!("--elevated --wait-pid {}", std::process::id());
    shell::relaunch_elevated(&args)?;
    app.exit(0);
    Ok(())
}

/// 双击 Ctrl 的键盘钩子是否已装上
#[tauri::command]
pub fn hotkey_hook_ok() -> bool {
    hotkey::hook_ok()
}

#[tauri::command]
pub fn is_admin() -> bool {
    shell::is_admin()
}

#[tauri::command]
pub fn toggle_hotkey(on: bool) {
    hotkey::set_enabled(on);
}

/// 立即应用设置面板里的自定义快捷键，返回配置串是否合法。
/// `spec` 形如 `DoubleCtrl` / `Ctrl+Alt+S` / `Alt+Space` / `None`。
#[tauri::command]
pub fn set_hotkey(spec: String, enabled: bool) -> bool {
    hotkey::apply(&spec, enabled)
}

#[tauri::command]
pub fn window_outside_click_hide(app: AppHandle, on: bool) {
    let _ = app.emit("ui:hide-on-blur", serde_json::json!({ "on": on }));
}

#[tauri::command]
pub fn register_url_protocol(state: State<'_, AppState>, enable: bool) -> Result<(), String> {
    shell::register_url_protocol(enable)?;
    state.update(|c| c.integration.protocol = enable);
    Ok(())
}

/// tinker:// 是否已注册（读注册表实际状态）
#[tauri::command]
pub fn url_protocol_registered() -> bool {
    shell::url_protocol_registered()
}

/// 取走启动参数里挂起的 tinker:// 链接（取一次即清空）
#[tauri::command]
pub fn take_pending_url() -> Option<String> {
    crate::PENDING_URL.lock().take()
}

/// 预览用：读取文本文件（UTF-8 优先，回退 GBK）
#[tauri::command]
pub async fn read_text_file(path: String, max: usize) -> Result<serde_json::Value, String> {
    blocking(move || {
        use std::io::Read;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let limit = max.clamp(4096, 4_000_000);
        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(64 * 1024);
        f.take(limit as u64)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        if buf.iter().take(8192).any(|b| *b == 0) {
            return Ok(serde_json::json!({
                "binary": true, "text": "", "truncated": false, "size": meta.len()
            }));
        }
        let text = match std::str::from_utf8(&buf) {
            Ok(s) => s.to_string(),
            Err(_) => {
                let (cow, _, _) = encoding_rs::GBK.decode(&buf);
                cow.into_owned()
            }
        };
        let text = text.trim_start_matches('\u{feff}').to_string();
        Ok(serde_json::json!({
            "binary": false,
            "text": text,
            "truncated": meta.len() > limit as u64,
            "size": meta.len()
        }))
    })
    .await
}
