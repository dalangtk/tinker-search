//! 配置持久化：单一 JSON 文件，字段全部用中文键名便于与本项目原有配置对齐。

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchSettings {
    #[serde(alias = "支持首拼")]
    pub pinyin_initial: bool,
    #[serde(alias = "支持全拼")]
    pub pinyin_full: bool,
    #[serde(alias = "拼音完整匹配")]
    pub pinyin_exact: bool,
    #[serde(alias = "支持星号")]
    pub wildcard_star: bool,
    #[serde(alias = "支持问号")]
    pub wildcard_q: bool,
    #[serde(alias = "匹配全角")]
    pub fullwidth: bool,
    #[serde(alias = "区分大小写")]
    pub case_sensitive: bool,
    #[serde(alias = "且")]
    pub and_sep: String,
    #[serde(alias = "或")]
    pub or_sep: String,
    #[serde(alias = "删搜索词首尾空")]
    pub trim: bool,
    #[serde(alias = "搜索词为空时显示所有文件")]
    pub empty_all: bool,
}

impl Default for MatchSettings {
    fn default() -> Self {
        Self {
            pinyin_initial: true,
            pinyin_full: true,
            pinyin_exact: false,
            wildcard_star: true,
            wildcard_q: true,
            fullwidth: true,
            case_sensitive: false,
            and_sep: " ".into(),
            or_sep: "|".into(),
            trim: true,
            empty_all: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewCfg {
    /// dense | list | grid-medium | grid-large
    pub mode: String,
    pub preview: bool,
    /// 0 = 自动
    pub zoom: u32,
}

impl Default for ViewCfg {
    fn default() -> Self {
        Self { mode: "list".into(), preview: true, zoom: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowCfg {
    pub always_on_top: bool,
    pub hide_on_blur: bool,
    /// 是否启用全局呼出快捷键。字段名沿用旧版「双击 Ctrl 开关」，
    /// 现在具体按键由 `hotkey` 决定，不再固定为 Ctrl。
    pub double_ctrl: bool,
    /// 全局呼出快捷键：`DoubleCtrl` / `Ctrl+Alt+S` / `Alt+Space` / `None`。
    /// 空串或 None 表示不绑定。由设置面板录制，热更新无需重启。
    /// 旧版配置里有个从未启用的占位字段 `hotkey`（值恒为 "Alt+Space"），
    /// 这里换名后它会被 serde 直接忽略，用户升级后仍保持原来的双击 Ctrl。
    pub shortcut: String,
    pub width: u32,
    pub height: u32,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            always_on_top: false,
            hide_on_blur: false,
            double_ctrl: true,
            shortcut: "DoubleCtrl".into(),
            width: 980,
            height: 620,
            x: None,
            y: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SortCfg {
    pub field: String,
    pub asc: bool,
}

impl Default for SortCfg {
    fn default() -> Self {
        Self { field: "相关度".into(), asc: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexCfg {
    /// 索引库文件路径（空 = 使用默认 AppData 路径）
    pub db_path: String,
    pub auto_scan_first_run: bool,
    /// 重建索引时使用的盘符（空 = 全部固定盘）
    pub drives: Vec<String>,
    pub parent_size_sync: bool,
    pub default_query_save: bool,
    pub excluded_dirs: Vec<String>,
}

impl Default for IndexCfg {
    fn default() -> Self {
        Self {
            db_path: String::new(),
            auto_scan_first_run: true,
            drives: Vec::new(),
            parent_size_sync: false,
            default_query_save: true,
            excluded_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMode {
    pub id: String,
    pub name: String,
    /// wildcard | regex | sql | lua | lua-exec
    pub kind: String,
    pub template: String,
    pub desc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub theme: String,
    pub view: ViewCfg,
    pub window: WindowCfg,
    pub sort: SortCfg,
    pub filter: Vec<String>,
    pub search: MatchSettings,
    pub index: IndexCfg,
    pub history: Vec<String>,
    pub custom_modes: Vec<CustomMode>,
    pub boost: Vec<BoostRule>,
    pub integration: IntegrationCfg,
}

/// 与系统集成相关的开关
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationCfg {
    /// tinker:// URL 协议是否已注册（true 时启动会按当前 exe 路径刷新注册表项）
    pub protocol: bool,
}

impl Default for IntegrationCfg {
    fn default() -> Self {
        Self { protocol: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoostRule {
    pub kind: String, // path | ext
    pub value: String,
    pub delta: i16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: "default".into(),
            view: ViewCfg::default(),
            window: WindowCfg::default(),
            sort: SortCfg::default(),
            filter: vec!["全部".into()],
            search: MatchSettings::default(),
            index: IndexCfg::default(),
            history: Vec::new(),
            custom_modes: Vec::new(),
            boost: Vec::new(),
            integration: IntegrationCfg::default(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str::<AppConfig>(&s).unwrap_or_else(|e| {
                eprintln!("[config] 解析失败，使用默认配置: {e}");
                AppConfig::default()
            }),
            Err(_) => AppConfig::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// 引擎匹配设置需要的中文键 JSON
pub fn match_json(m: &MatchSettings) -> String {
    serde_json::json!({
        "支持首拼": m.pinyin_initial,
        "支持全拼": m.pinyin_full,
        "拼音完整匹配": m.pinyin_exact,
        "支持星号": m.wildcard_star,
        "支持问号": m.wildcard_q,
        "匹配全角": m.fullwidth,
        "区分大小写": m.case_sensitive,
        "且": m.and_sep,
        "或": m.or_sep,
        "删搜索词首尾空": m.trim,
        "搜索词为空时显示所有文件": m.empty_all,
    })
    .to_string()
}
