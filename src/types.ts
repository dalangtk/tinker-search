export interface Row {
  id: number;
  name: string;
  path: string;
  parent: string;
  ext: string;
  size: number;
  mtime: number;
  ctime: number;
  atime: number;
  is_dir: boolean;
  file_type: string;
  alias: string;
  rating: number;
  attrs: number;
  hl_file: string[];
  hl_dir: string[];
  hl_alias: string[];
}

export interface SearchStart {
  fingerprint: number;
  accepted: boolean;
}

export interface MatchSettings {
  pinyin_initial: boolean;
  pinyin_full: boolean;
  pinyin_exact: boolean;
  wildcard_star: boolean;
  wildcard_q: boolean;
  fullwidth: boolean;
  case_sensitive: boolean;
  and_sep: string;
  or_sep: string;
  trim: boolean;
  empty_all: boolean;
}

export interface FilterCat {
  名称: string;
  类型: number;
  后缀?: string;
}

export interface CustomMode {
  id: string;
  name: string;
  kind: string;
  template: string;
  desc: string;
}

export interface AppConfig {
  theme: string;
  view: { mode: string; preview: boolean; zoom: number };
  window: {
    always_on_top: boolean;
    hide_on_blur: boolean;
    double_ctrl: boolean;
    shortcut: string;
    width: number;
    height: number;
    x: number | null;
    y: number | null;
  };
  sort: { field: string; asc: boolean };
  filter: string[];
  search: MatchSettings;
  index: {
    db_path: string;
    auto_scan_first_run: boolean;
    drives: string[];
    parent_size_sync: boolean;
    default_query_save: boolean;
    excluded_dirs: string[];
  };
  history: string[];
  custom_modes: CustomMode[];
  boost: { kind: string; value: string; delta: number }[];
  integration: { protocol: boolean };
}

export interface IndexStats {
  ready: boolean;
  scanning: boolean;
  file_count: number;
  memory_bytes: number;
  memory_text: string;
  state: number;
  progress: number;
  scanning_drives: string;
  db_path: string;
  db_version: number;
  engine_version: string;
  syncing: boolean;
  pending_sync: number;
  /** 启动阶段的建库/加载尝试已结束 */
  boot_done: boolean;
  /** 试过建索引但缺管理员权限（界面应引导提权） */
  needs_admin: boolean;
  /** 当前是否管理员：只有建库与实时同步需要，搜索等其余功能都不需要 */
  admin: boolean;
  /** 启动阶段待展示的一句提示，读过即清空 */
  notice: string | null;
}

export interface ModeDef {
  key: string;
  label: string;
  type: number;
  hint: string;
  phone?: string;
  custom?: CustomMode;
}

/* ------------------------------ 首页 / 侧栏 ------------------------------ */

/** 常用位置（桌面、下载、文档…），来自 SHGetKnownFolderPath */
export interface Place {
  key: string;
  name: string;
  path: string;
}

/** 磁盘 + 容量 */
export interface DriveInfo {
  letter: string;
  name: string;
  fs: string;
  kind: number;
  total: number;
  free: number;
  used: number;
  percent: number;
  /** 该卷是否已收录进索引；false 表示点它只会得到 0 条 */
  indexed?: boolean;
}

export interface HomeItem {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  mtime: number;
  ext: string;
}

export interface DirListing {
  path: string;
  items: HomeItem[];
  total: number;
  truncated: boolean;
  error?: string | null;
}

export interface HomeSnapshot {
  places: Place[];
  drives: DriveInfo[];
  desktop: DirListing;
  sections: DirListing[];
}

/** 左侧分类栏的选中项：同一类内「或」，跨类「且」 */
export interface Facets {
  places: string[];
  types: string[];
  drives: string[];
}

/** 地址栏里的分类标签 */
export interface FacetTag {
  kind: "places" | "types" | "drives";
  label: string;
  text: string;
  count: number;
}

export const MODES: ModeDef[] = [
  {
    key: "wildcard",
    label: "通配符",
    type: 0,
    hint: "星号 / 问号 / 拼音首拼 / 空格=且  |  =或",
    phone: "*.txt",
  },
  { key: "regex", label: "正则", type: 1, hint: "PCRE2 正则，^[0-9]{4}-报告.*\\.docx$", phone: "^\\d+$" },
  {
    key: "sql",
    label: "SQL",
    type: 2,
    hint: "FName LIKE '%报告%' AND Size > '100M'",
    phone: "Size > '100M'",
  },
];
