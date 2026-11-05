//! 插件系统。
//!
//! 设计上插件是**纯数据包**（一个 JSON 清单），不含可执行代码：
//! 自然语言搜索插件提供的只是「词表 + 引擎配置」，由宿主的 `nlu` 引擎解释执行。
//! 这样第三方插件不可能在用户机器上跑任意代码，装上就是安全的。
//!
//! 想换「模型」有两条路，都落在清单的 `engine` 字段里：
//!   - `provider = "rules"`：宿主内置规则引擎，0 字节、毫秒级；
//!   - `provider = "http"`：把文本发给任意 OpenAI 兼容端点（Ollama / LM Studio / 在线 API），
//!     `endpoint` 与 `model` 由用户在插件设置里改，换模型不用换插件。
//!
//! 目录结构（`data_dir/plugins`）：
//!   <id>/plugin.json   清单
//!   <id>/state.json    安装状态（是否启用、来源、时间、引擎覆盖）

use crate::nlu::{NluCfg, NluQuery};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const TYPE_NLU: &str = "nlu";

// ============================================================================
// 清单
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineCfg {
    /// rules | http
    pub provider: String,
    pub model: String,
    pub endpoint: String,
}

impl Default for EngineCfg {
    fn default() -> Self {
        Self { provider: "rules".into(), model: "内置规则引擎".into(), endpoint: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// nlu（当前唯一类型）
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    pub author: String,
    pub homepage: String,
    pub min_app: String,
    pub engine: EngineCfg,
    pub nlu: NluCfg,
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            version: "0.0.0".into(),
            kind: TYPE_NLU.into(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            min_app: "1.0.0".into(),
            engine: EngineCfg::default(),
            nlu: NluCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginState {
    pub enabled: bool,
    pub installed_at: i64,
    /// builtin | market | file | url
    pub source: String,
    /// 用户在界面上改过的引擎配置（覆盖清单里的默认值）
    pub engine: Option<EngineCfg>,
}

/// 给前端的视图：清单 + 本地状态 + 是否有更新。
#[derive(Debug, Clone, Serialize)]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub description: String,
    pub author: String,
    pub homepage: String,
    pub enabled: bool,
    pub installed_at: i64,
    pub source: String,
    pub engine: EngineCfg,
    pub latest: String,
    pub has_update: bool,
    /// 词表规模，界面上用来显示「支持 N 个说法」
    pub vocab: i32,
}

// ============================================================================
// 市场
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    pub author: String,
    pub min_app: String,
    /// `builtin:<id>` 表示随应用携带；否则是 http(s) 地址
    pub download: String,
    pub sha256: String,
    pub changelog: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketIndex {
    pub updated: String,
    pub plugins: Vec<MarketEntry>,
}

/// 随应用携带的内置市场：即使离线、没有发布服务，也能装上自然语言搜索。
const BUNDLED_INDEX: &str = include_str!("../assets/plugins/index.json");
const BUNDLED_NL: &str = include_str!("../assets/plugins/tinker.nl-zh.json");

// ============================================================================
// 路径与读写
// ============================================================================

pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("plugins")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
    let text = fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<(), String> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(v).map_err(|e| format!("序列化失败：{e}"))?;
    fs::write(p, text).map_err(|e| format!("写入失败：{e}"))
}

fn manifest_path(data_dir: &Path, id: &str) -> PathBuf {
    root(data_dir).join(id).join("plugin.json")
}

fn state_path(data_dir: &Path, id: &str) -> PathBuf {
    root(data_dir).join(id).join("state.json")
}

pub fn manifest(data_dir: &Path, id: &str) -> Option<PluginManifest> {
    read_json(&manifest_path(data_dir, id))
}

pub fn state(data_dir: &Path, id: &str) -> PluginState {
    read_json(&state_path(data_dir, id)).unwrap_or_default()
}

fn vocab_count(n: &NluCfg) -> i32 {
    let mut n_words = 0;
    n.ignore.iter().for_each(|_| n_words += 1);
    n.types.iter().for_each(|r| n_words += r.words.len());
    n.fields.iter().for_each(|r| n_words += r.words.len());
    n.times.iter().for_each(|r| n_words += r.words.len());
    n_words += n.folders.len();
    n_words as i32
}

// ============================================================================
// 列表
// ============================================================================

pub fn list(data_dir: &Path) -> Vec<PluginView> {
    list_in(data_dir, "")
}

/// 同 `list`，但用指定的市场索引判断「有没有新版本」。
/// 配了自定义市场时必须走这条，否则只看内置索引，永远显示「已是最新」。
pub fn list_in(data_dir: &Path, market_url: &str) -> Vec<PluginView> {
    let market = market(data_dir, market_url);
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root(data_dir)) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let Some(m) = read_json::<PluginManifest>(&e.path().join("plugin.json")) else {
            continue;
        };
        let st = state(data_dir, &m.id);
        let latest = market
            .iter()
            .find(|x| x.id == m.id)
            .map(|x| x.version.clone())
            .unwrap_or_else(|| m.version.clone());
        out.push(PluginView {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            kind: m.kind.clone(),
            description: m.description.clone(),
            author: m.author.clone(),
            homepage: m.homepage.clone(),
            enabled: st.enabled,
            installed_at: st.installed_at,
            source: st.source.clone(),
            engine: st.engine.unwrap_or_else(|| m.engine.clone()),
            has_update: version_gt(&latest, &m.version),
            latest,
            vocab: vocab_count(&m.nlu),
        });
    }
    out.sort_by(|a, b| b.enabled.cmp(&a.enabled).then(a.name.cmp(&b.name)));
    out
}

// ============================================================================
// 安装 / 启用 / 卸载
// ============================================================================

/// 校验清单：id 必须是 `字母数字.-_`，不允许路径穿越，版本号非空。
fn validate(m: &PluginManifest) -> Result<(), String> {
    if m.id.is_empty() || m.id.len() > 64 {
        return Err("插件 id 非法".into());
    }
    if !m
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err("插件 id 只能包含字母、数字、点、横线、下划线".into());
    }
    if m.name.trim().is_empty() {
        return Err("插件缺少名称".into());
    }
    if m.version.trim().is_empty() {
        return Err("插件缺少版本号".into());
    }
    if m.kind != TYPE_NLU {
        return Err(format!("不支持的插件类型：{}", m.kind));
    }
    Ok(())
}

/// 从清单文本安装。`source` 只用于界面展示来源。
pub fn install_json(data_dir: &Path, text: &str, source: &str) -> Result<PluginView, String> {
    let m: PluginManifest =
        serde_json::from_str(text).map_err(|e| format!("插件清单解析失败：{e}"))?;
    validate(&m)?;
    write_json(&manifest_path(data_dir, &m.id), &m)?;

    let old = state(data_dir, &m.id);
    let st = PluginState {
        // 升级时保留原来的启用状态；全新安装默认不启用，由用户显式开启
        enabled: old.enabled && old.installed_at > 0,
        installed_at: if old.installed_at > 0 { old.installed_at } else { now_ms() },
        source: source.to_string(),
        engine: old.engine,
    };
    write_json(&state_path(data_dir, &m.id), &st)?;
    list(data_dir)
        .into_iter()
        .find(|v| v.id == m.id)
        .ok_or_else(|| "安装后读取失败".to_string())
}

pub fn install_bundled(data_dir: &Path, id: &str) -> Result<PluginView, String> {
    if id == "tinker.nl-zh" {
        return install_json(data_dir, BUNDLED_NL, "builtin");
    }
    Err(format!("内置插件不存在：{id}"))
}

/// 从 URL 安装：只接受 http(s)，限制体积，内容必须是合法清单。
pub fn install_url(data_dir: &Path, url: &str) -> Result<PluginView, String> {
    let text = http_get(url, 2 * 1024 * 1024)?;
    install_json(data_dir, &text, "url")
}

pub fn set_enabled(data_dir: &Path, id: &str, on: bool) -> Result<(), String> {
    if manifest(data_dir, id).is_none() {
        return Err("插件未安装".into());
    }
    let mut st = state(data_dir, id);
    st.enabled = on;
    if st.installed_at == 0 {
        st.installed_at = now_ms();
    }
    write_json(&state_path(data_dir, id), &st)
}

pub fn set_engine(data_dir: &Path, id: &str, engine: EngineCfg) -> Result<(), String> {
    if manifest(data_dir, id).is_none() {
        return Err("插件未安装".into());
    }
    if engine.provider != "rules" && engine.provider != "http" {
        return Err("引擎类型只能是 rules 或 http".into());
    }
    if engine.provider == "http" && !engine.endpoint.starts_with("http") {
        return Err("请填写形如 http://127.0.0.1:11434/v1/chat/completions 的端点".into());
    }
    let mut st = state(data_dir, id);
    st.engine = Some(engine);
    write_json(&state_path(data_dir, id), &st)
}

pub fn uninstall(data_dir: &Path, id: &str) -> Result<(), String> {
    let dir = root(data_dir).join(id);
    if !dir.exists() {
        return Err("插件未安装".into());
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("删除失败：{e}"))
}

/// 更新：有市场地址就下载新版，否则回落到内置包。
pub fn update(data_dir: &Path, id: &str, market_url: &str) -> Result<PluginView, String> {
    let entry = market(data_dir, market_url)
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| "市场里没有这个插件".to_string())?;
    if entry.download.starts_with("builtin:") {
        return install_bundled(data_dir, &entry.download["builtin:".len()..]);
    }
    install_url(data_dir, &entry.download)
}

// ============================================================================
// 市场索引
// ============================================================================

/// 读市场索引。配置了地址就拉远程（10 分钟缓存），失败或无地址用内置索引。
pub fn market(data_dir: &Path, url: &str) -> Vec<MarketEntry> {
    let cache_path = data_dir.join("market").join("index.json");
    let url = url.trim();
    if url.is_empty() {
        return bundled_entries();
    }
    if let Some((at, idx)) = read_json::<(i64, MarketIndex)>(&cache_path) {
        if SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64 - at < 600)
            .unwrap_or(false)
        {
            return idx.plugins;
        }
    }
    match http_get(url, 1024 * 1024)
        .and_then(|t| serde_json::from_str::<MarketIndex>(&t).map_err(|e| format!("索引解析失败：{e}")))
    {
        Ok(idx) => {
            let _ = write_json(&cache_path, &(now_ms() / 1000, &idx));
            idx.plugins
        }
        Err(_) => bundled_entries(),
    }
}

fn bundled_entries() -> Vec<MarketEntry> {
    serde_json::from_str::<MarketIndex>(BUNDLED_INDEX)
        .map(|i| i.plugins)
        .unwrap_or_default()
}

// ============================================================================
// 供搜索链路使用
// ============================================================================

/// 当前已启用、且类型为 nlu 的插件。没有就返回 None —— 界面据此隐藏自然语言模式。
pub fn active_nlu(data_dir: &Path) -> Option<(PluginManifest, EngineCfg)> {
    for v in list(data_dir) {
        if v.enabled && v.kind == TYPE_NLU {
            let m = manifest(data_dir, &v.id)?;
            return Some((m, v.engine));
        }
    }
    None
}

/// 解析一句话：`rules` 走本地规则，`http` 走外部模型（失败自动回落到规则）。
pub fn parse(m: &PluginManifest, engine: &EngineCfg, text: &str) -> NluQuery {
    let now = chrono::Local::now();
    if engine.provider == "http" && !engine.endpoint.is_empty() {
        if let Some(q) = crate::nlu_model::parse_http(text, engine, &m.nlu) {
            return q;
        }
    }
    crate::nlu::parse(text, &m.nlu, now)
}

// ============================================================================
// 版本与 HTTP
// ============================================================================

/// 简化的版本比较：按数字段比，缺失补 0。
pub fn version_gt(a: &str, b: &str) -> bool {
    let pa: Vec<u32> = a.split(['.', '-', '+']).filter_map(|s| s.parse().ok()).collect();
    let pb: Vec<u32> = b.split(['.', '-', '+']).filter_map(|s| s.parse().ok()).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 用系统自带 curl 取文本：不引第三方 HTTP 库，装包体积不变。
/// （Windows 10 1803 起 System32 下就有 curl.exe）
pub fn http_get(url: &str, max_bytes: usize) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("只支持 http/https 地址".into());
    }
    let out = std::process::Command::new("curl")
        .args(["-sSL", "--max-time", "20", "--max-filesize", &max_bytes.to_string(), url])
        .output()
        .map_err(|e| format!("调用 curl 失败：{e}"))?;
    if !out.status.success() {
        return Err(format!("下载失败：{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    if out.stdout.len() > max_bytes {
        return Err("文件超过大小限制".into());
    }
    String::from_utf8(out.stdout).map_err(|_| "内容不是 UTF-8 文本".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tinker-plug-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    /// 安装 → 默认不启用 → 启用后生效 → 卸载后失效
    #[test]
    fn install_enable_uninstall() {
        let dir = tmp("life");
        let v = install_bundled(&dir, "tinker.nl-zh").unwrap();
        assert_eq!(v.id, "tinker.nl-zh");
        assert!(!v.enabled, "刚装上不该自动启用");
        assert!(active_nlu(&dir).is_none(), "未启用时不能生效");

        set_enabled(&dir, "tinker.nl-zh", true).unwrap();
        let (m, e) = active_nlu(&dir).expect("启用后应当拿到插件");
        assert_eq!(e.provider, "rules");

        let q = parse(&m, &e, "今天下载的pdf文件");
        assert_eq!(q.exts, vec!["pdf"]);
        assert_eq!(q.field, "ctime");
        assert!(q.from_ms > 0);

        uninstall(&dir, "tinker.nl-zh").unwrap();
        assert!(active_nlu(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 非法清单要被挡住：路径穿越、缺版本、未知类型
    #[test]
    fn rejects_bad_manifest() {
        let dir = tmp("bad");
        let bad_id = r#"{"id":"../../evil","name":"x","version":"1.0.0","type":"nlu"}"#;
        assert!(install_json(&dir, bad_id, "file").is_err());

        let no_version = r#"{"id":"ok.id","name":"x","version":"","type":"nlu"}"#;
        assert!(install_json(&dir, no_version, "file").is_err());

        let bad_type = r#"{"id":"ok.id","name":"x","version":"1.0.0","type":"exe"}"#;
        assert!(install_json(&dir, bad_type, "file").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_compare() {
        assert!(version_gt("1.0.10", "1.0.9"));
        assert!(version_gt("1.2.0", "1.1.9"));
        assert!(!version_gt("1.0.0", "1.0.0"));
        assert!(!version_gt("1.0", "1.0.1"));
    }

    /// 内置市场至少要有自然语言搜索这一条，否则离线场景装不上
    #[test]
    fn bundled_market_has_nl_plugin() {
        let list = bundled_entries();
        assert!(list.iter().any(|e| e.id == "tinker.nl-zh"));
    }
}
