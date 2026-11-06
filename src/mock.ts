// 界面预览模式（仅用于开发/设计校验）：浏览器里打开 http://localhost:1420/?mock=1 即可
// 无需引擎与索引即可看到真实排版的列表。生产环境不会走到这里。

const NAMES: [string, string, number, string][] = [
  ["项目结题报告-2026Q3.docx", "D:\\Work\\报告", 284_512, "docx"],
  ["需求规格说明书_v2.1.pdf", "D:\\Work\\文档\\产品", 3_892_011, "pdf"],
  ["main.rs", "E:\\zbl\\rust\\rfile-workbuddy\\src-tauri\\src", 18_402, "rs"],
  ["年度财务汇总.xlsx", "C:\\Users\\woshi\\Documents\\财务", 1_204_882, "xlsx"],
  ["产品宣传片-终版.mp4", "E:\\Media\\视频", 268_435_456, "mp4"],
  ["logo-深色@2x.png", "D:\\Design\\品牌\\logo", 92_331, "png"],
  ["会议纪要 2026-10-28.md", "D:\\Work\\会议", 4_120, "md"],
  ["客户名单.csv", "D:\\Work\\销售", 33_902, "csv"],
  ["Windows", "C:\\", 0, ""],
  ["System32", "C:\\Windows", 0, ""],
  ["explorer.exe", "C:\\Windows", 4_812_800, "exe"],
  ["win.ini", "C:\\Windows", 2_048, "ini"],
  ["Program Files", "C:\\", 0, ""],
  ["setup-x64.msi", "D:\\Downloads", 88_901_221, "msi"],
  ["背景音乐-轻快.flac", "E:\\Media\\音频", 42_118_004, "flac"],
  ["架构图.drawio", "D:\\Work\\架构", 61_002, "drawio"],
  ["备份-2026-11-01.zip", "E:\\Backup", 1_998_112_334, "zip"],
  ["快捷键清单.txt", "C:\\Users\\woshi\\Desktop", 1_204, "txt"],
  ["index.html", "E:\\zbl\\rust\\rfile-workbuddy", 512, "html"],
  ["App.vue", "E:\\zbl\\rust\\rfile-workbuddy\\src", 24_881, "vue"],
  ["设计规范.sketch", "D:\\Design\\规范", 12_004_881, "sketch"],
  ["培训视频-第03讲.mkv", "E:\\Media\\培训", 734_003_221, "mkv"],
  ["数据字典.json", "D:\\Work\\接口", 220_118, "json"],
];

export const MOCK_ROWS = Array.from({ length: 4200 }, (_, i) => {
  const [name, parent, size, ext] = NAMES[i % NAMES.length];
  const idx = i >= NAMES.length ? ` (${Math.floor(i / NAMES.length)})` : "";
  const base = name.replace(/(\.[^.]+)$/, "");
  const finalName = idx ? `${base}${idx}${ext ? "." + ext : ""}` : name;
  const isDir = !ext;
  return {
    id: i + 1,
    name: finalName,
    path: `${parent.replace(/\\+$/, "")}\\${finalName}`,
    parent,
    ext,
    size,
    mtime: Date.now() - i * 3_600_000 * 7,
    ctime: Date.now() - i * 86_400_000,
    atime: Date.now() - i * 3_600_000,
    is_dir: isDir,
    file_type: isDir ? "目录" : "文档",
    alias: i % 37 === 0 ? "常用" : "",
    rating: (i * 7) % 40,
    attrs: 32,
    hl_file: i % 11 === 0 ? [base.slice(0, 2)] : [],
    hl_dir: [],
    hl_alias: [],
  };
});

/* ------------------------ 首页 / 侧栏假数据 ------------------------ */

const MOCK_PLACES = [
  { key: "home", name: "主目录", path: "C:////Users////woshi" },
  { key: "desktop", name: "桌面", path: "C:////Users////woshi////Desktop" },
  { key: "downloads", name: "下载", path: "C:////Users////woshi////Downloads" },
  { key: "documents", name: "文档", path: "C:////Users////woshi////Documents" },
  { key: "pictures", name: "图片", path: "C:////Users////woshi////Pictures" },
  { key: "music", name: "音乐", path: "C:////Users////woshi////Music" },
  { key: "videos", name: "视频", path: "C:////Users////woshi////Videos" },
];

const MOCK_DRIVES = [
  { letter: "C:", name: "系统", fs: "NTFS", kind: 3, total: 512_000_000_000, free: 128_000_000_000, used: 384_000_000_000, percent: 75, indexed: true },
  { letter: "D:", name: "工作盘", fs: "NTFS", kind: 3, total: 1_000_000_000_000, free: 620_000_000_000, used: 380_000_000_000, percent: 38, indexed: true },
  { letter: "E:", name: "仓库", fs: "NTFS", kind: 3, total: 2_000_000_000_000, free: 180_000_000_000, used: 1_820_000_000_000, percent: 91, indexed: false },
];

function mockItems(prefix: string, names: string[]): any[] {
  return names.map((n, i) => ({
    name: n,
    path: `${prefix}\\${n}`,
    is_dir: !n.includes("."),
    size: n.includes(".") ? 12_000 * (i + 3) : 0,
    mtime: Date.now() - i * 7_200_000,
    ext: n.includes(".") ? n.split(".").pop() : "",
  }));
}

const MOCK_HOME = {
  places: MOCK_PLACES,
  drives: MOCK_DRIVES,
  desktop: {
    path: "C:////Users////woshi////Desktop",
    items: mockItems("C:////Users////woshi////Desktop", [
      "工作",
      "截图",
      "项目结题报告-2026Q3.docx",
      "需求规格说明书_v2.1.pdf",
      "快捷键清单.txt",
      "logo-深色@2x.png",
      "会议纪要 2026-10-28.md",
      "架构图.drawio",
    ]),
    total: 8,
    truncated: false,
  },
  sections: MOCK_DRIVES.map((d) => ({
    path: `${d.letter}\\`,
    items: mockItems(`${d.letter}`, [
      "Users",
      "Windows",
      "Program Files",
      "Work",
      "Media",
      "Backup",
    ]),
    total: 6,
    truncated: false,
  })),
};

let fp = 0;
let current = MOCK_ROWS;

const SORT_FIELDS = [
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
let sortField = "相关度";
let sortAsc = false;

function sortKey(r: any, field: string): string | number {
  switch (field) {
    case "文件名":
      return String(r.name).toLowerCase();
    case "文件夹":
      return String(r.parent || "").toLowerCase();
    case "文件大小":
      return r.size;
    case "修改时间":
      return r.mtime;
    case "文件类型":
      return r.is_dir ? "文件夹" : String(r.ext || "").toLowerCase();
    case "路径":
      return String(r.path).toLowerCase();
    case "扩展名":
      return String(r.ext || "").toLowerCase();
    case "创建时间":
      return r.ctime;
    case "访问时间":
      return r.atime;
    default:
      return 0;
  }
}

/** 统一分隔符并折叠重复的斜杠（预览数据里的路径写法比较随意） */
function normPath(p: string): string {
  return p.replace(/\//g, "\\").replace(/\\{2,}/g, "\\").toLowerCase();
}

/** 与原生内核一致：相关度保持命中顺序，升序取反；其余字段按 key 排序后反向 */
function applySort(rows: any[]): any[] {
  if (sortField === "相关度") return sortAsc ? [...rows].reverse() : rows;
  const out = [...rows].sort((a, b) => {
    const ka = sortKey(a, sortField);
    const kb = sortKey(b, sortField);
    const c = ka < kb ? -1 : ka > kb ? 1 : a.id - b.id;
    return sortAsc ? c : -c;
  });
  return out;
}

const DEFAULT_CFG = {
  theme: "default",
  view: { mode: "list", preview: true, zoom: 116 },
  window: {
    always_on_top: false,
    hide_on_blur: false,
    double_ctrl: true,
    shortcut: "DoubleCtrl",
    width: 1120,
    height: 720,
    x: null,
    y: null,
  },
  sort: { field: "相关度", asc: false },
  filter: ["全部"],
  search: {
    pinyin_initial: true,
    pinyin_full: true,
    pinyin_exact: false,
    wildcard_star: true,
    wildcard_q: true,
    fullwidth: true,
    case_sensitive: false,
    and_sep: " ",
    or_sep: "|",
    trim: true,
    empty_all: true,
  },
  index: {
    db_path: "C:\\Users\\woshi\\AppData\\Roaming\\com.tinker.search\\sscore.idx",
    auto_scan_first_run: true,
    drives: [],
    parent_size_sync: false,
    default_query_save: true,
    excluded_dirs: [],
  },
  history: ["报告", "*.pdf", "wd", "会议纪要"],
  custom_modes: [],
  boost: [],
  integration: { protocol: false },
};

/* ------------------------ 极小事件总线 ------------------------ */
type Handler = (e: { payload: any }) => void;
const bus: Record<string, Handler[]> = {};

export function mockListen(name: string, h: Handler) {
  (bus[name] ||= []).push(h);
  return () => {
    bus[name] = (bus[name] || []).filter((x) => x !== h);
  };
}

function emit(name: string, payload: any) {
  (bus[name] || []).forEach((h) => h({ payload }));
}

export async function mockInvoke(cmd: string, args: any = {}): Promise<any> {
  await new Promise((r) => setTimeout(r, 2));
  switch (cmd) {
    case "app_info":
      return {
        admin: false,
        engineVersion: "sscore 0.1 (native)",
        dbVersion: 1,
        dbPath: DEFAULT_CFG.index.db_path,
        dataDir: "C:\\Users\\woshi\\AppData\\Roaming\\com.tinker.search",
        cfgPath: "C:\\Users\\woshi\\AppData\\Roaming\\com.tinker.search\\config.json",
        dbExists: true,
        config: JSON.parse(JSON.stringify(DEFAULT_CFG)),
      };
    case "config_get":
      return JSON.parse(JSON.stringify(DEFAULT_CFG));
    case "config_save":
      return null;
    case "mem_stats": {
      const parts = [
        { name: "父指针", bytes: 19_100_000 },
        { name: "名字偏移", bytes: 19_100_000 },
        { name: "名字长度", bytes: 9_600_000 },
        { name: "卷号", bytes: 9_600_000 },
        { name: "标志位", bytes: 4_800_000 },
        { name: "文件号", bytes: 38_300_000 },
        { name: "名字池", bytes: 121_200_000 },
      ];
      const total = parts.reduce((a, p) => a + p.bytes, 0);
      return {
        private_working_set: 282_000_000,
        working_set: 325_000_000,
        peak_working_set: 341_000_000,
        private_bytes: 352_000_000,
        total_working_set: 325_000_000,
        total_private_working_set: 282_000_000,
        process_count: 1,
        webview_bytes: 226_000_000,
        webview_private_bytes: 188_000_000,
        webview_count: 4,
        sys_total: 34_359_738_368,
        sys_avail: 18_253_611_008,
        sys_load: 47,
        index_bytes: total,
        index_parts: parts.map((p) => ({
          ...p,
          per_mille: Math.round((p.bytes * 1000) / total),
        })),
        frn_map_bytes: 20_100_000,
        journal_bytes: 3_145_728,
        cache_bytes: 1_800_000,
        db_file_bytes: 232_461_356,
        records: 5_019_159,
        dirs: 973_897,
        files: 4_045_262,
        hidden_records: 32_272,
        tomb_records: 11_408,
        bytes_per_record: total / 5_019_159,
        indexed_volumes: "C:, D:, E:",
        load_ms: 102,
        syncing: true,
        sync_text: "同步 3 轮 / 事件 12 / 新增 3 / 删除 1 / 上一轮 8 µs",
        admin: true,
      };
    }
    case "trim_memory":
      return 128_000_000;
    case "index_status":
      return {
        ready: true,
        scanning: false,
        file_count: 4_512_338,
        memory_bytes: 281_000_000,
        memory_text: "",
        state: 0,
        progress: 100,
        scanning_drives: '["C:","D:","E:"]',
        db_path: DEFAULT_CFG.index.db_path,
        db_version: 1,
        engine_version: "sscore 0.1 (native)",
        syncing: true,
        pending_sync: 0,
        boot_done: true,
        needs_admin: false,
        admin: true,
        notice: null,
      };
    case "get_filters":
      return [
        { 名称: "全部", 类型: 0 },
        { 名称: "文件夹", 类型: 255 },
        { 名称: "快捷方式", 类型: 100, 后缀: "LNK" },
        { 名称: "程序", 类型: 99, 后缀: "EXE,BAT,MSI" },
        { 名称: "音频", 类型: 98, 后缀: "MP3,FLAC,WAV" },
        { 名称: "视频", 类型: 97, 后缀: "MP4,MKV,MOV" },
        { 名称: "图片", 类型: 96, 后缀: "PNG,JPG,SVG" },
        { 名称: "文档", 类型: 80, 后缀: "TXT,HTML,CPP" },
        { 名称: "办公", 类型: 79, 后缀: "DOC,PDF,PPTX" },
        { 名称: "压缩包", 类型: 60, 后缀: "RAR,ZIP,7Z" },
      ];
    case "get_sort_fields":
      return SORT_FIELDS;
    case "set_sort": {
      sortField = String(args.field ?? "相关度");
      sortAsc = !!args.asc;
      current = applySort(current);
      const fpv = fp;
      const count = current.length;
      setTimeout(() => {
        emit("tinker:search-complete", {
          fingerprint: fpv,
          count,
          elapsedMs: 1,
          searchType: 0,
          aggregate: false,
          empty: count === 0,
        });
      }, 10);
      return null;
    }
    case "get_rows": {
      const { start = 0, count = 100 } = args;
      const rows = current.slice(start, start + count);
      return rows.map((r) => ({
        ...r,
        hl_file: args.__kw ? [] : r.hl_file,
      }));
    }
    case "list_dir": {
      const p = String(args.path ?? "")
        .replace(/\//g, "\\")
        .replace(/\\+$/, "")
        .toLowerCase();
      const kids = MOCK_ROWS.filter((r) => r.parent.replace(/\\+$/, "").toLowerCase() === p);
      kids.sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name));
      return { total: kids.length, rows: kids.slice(0, args.limit ?? 200) };
    }
    case "get_icons":
      return [];
    case "get_preview_icon":
      return null;
    case "resolve_link":
      // 预览数据里没有快捷方式
      return null;
    case "search": {
      const raw = String(args.keyword ?? "").trim();
      const mode = args.mode ?? 0;
      const scope = normPath(String(args.scope ?? "").trim()).replace(/\\+$/, "");
      fp += 1;
      const my = fp;
      if (mode === -2) {
        // 多重搜索链：逐段在上一段结果里继续过滤（真实引擎由 DLL 完成）
        let rows = MOCK_ROWS;
        try {
          for (const seg of JSON.parse(raw) as { q?: string; mode?: string }[]) {
            // 表达式类片段（sql / 正则）在预览里不做模拟，只按通配符片段过滤
            const m = String(seg.mode || "").toLowerCase();
            if (m && !["0", "wild", "wildcard", "通配符"].includes(m)) continue;
            const q = String(seg.q || "")
              .toLowerCase()
              .replace(/\*/g, "");
            if (q) rows = rows.filter((r) => r.name.toLowerCase().includes(q));
          }
        } catch {
          rows = [];
        }
        current = applySort(rows);
      } else {
        const kw = raw.toLowerCase();
        // 进入目录：只看该目录下的记录（空关键词就是浏览）
        const pool = scope
          ? MOCK_ROWS.filter((r) => normPath(r.path).startsWith(scope + "\\"))
          : MOCK_ROWS;
        const hits = kw ? pool.filter((r) => r.name.toLowerCase().includes(kw)) : pool;
        current = applySort(hits);
      }
      const fpv = my;
      setTimeout(() => {
        emit("tinker:search-complete", {
          fingerprint: fpv,
          count: mode === 2 ? 0 : current.length,
          elapsedMs: 2 + Math.floor(Math.random() * 6),
          searchType: mode,
          aggregate: false,
        });
      }, 20);
      return { fingerprint: my, accepted: true };
    }
    case "sql_result":
      return {
        聚合: false,
        字段数量: 3,
        字段: ["Path", "FName", "Size"],
        类型: [1, 1, 3],
        总数量: 3,
        起始: 0,
        限制: 500,
        行: [
          ["D:\\Work\\报告\\结题报告.docx", "结题报告.docx", 284512],
          ["D:\\Work\\文档\\说明书.pdf", "说明书.pdf", 3892011],
          ["E:\\Media\\视频\\宣传片.mp4", "宣传片.mp4", 268435456],
        ],
      };
    case "history_add":
      return ["报告"];
    case "history_clear":
      return [];
    case "excluded_dirs":
      return ["D:\\$RECYCLE.BIN"];
    case "list_drives":
      return [
        { 盘符: "C:", 盘类型: 3 },
        { 盘符: "D:", 盘类型: 3 },
        { 盘符: "E:", 盘类型: 3 },
      ];
    case "known_places":
      return MOCK_PLACES;
    case "home_snapshot":
      return MOCK_HOME;
    case "perf_text":
      return { load: "加载_文件载入: 12ms\n加载_导入文件名: 8ms", scan: "枚举_NTFS: 3.2s" };
    // ---- 自然语言 / 插件（预览模式下的假数据）----
    case "nl_status":
      return {
        ready: true,
        plugin: "tinker.nl-zh",
        name: "自然语言搜索（中文）",
        version: "1.0.0",
        engine: { provider: "rules", model: "内置规则引擎", endpoint: "" },
      };
    case "nl_parse":
    case "nl_search": {
      const nl = {
        keyword: "",
        exts: ["pdf"],
        dirs_only: false,
        field: "ctime",
        from_ms: 1,
        to_ms: 2,
        label: "今天",
        score: 0.8,
        notes: ["类型 pdf", "创建时间 今天"],
        leftover: [],
      };
      return cmd === "nl_parse"
        ? nl
        : { fingerprint: 1, count: 12, scanned: 488, nl };
    }
    case "plugins_list":
      return [
        {
          id: "tinker.nl-zh",
          name: "自然语言搜索（中文）",
          version: "1.0.0",
          kind: "nlu",
          description: "把「今天下载的 pdf」这类描述翻译成搜索条件。",
          author: "tinker",
          homepage: "",
          enabled: true,
          installed_at: 0,
          source: "builtin",
          engine: { provider: "rules", model: "内置规则引擎", endpoint: "" },
          latest: "1.0.0",
          has_update: false,
          vocab: 126,
        },
      ];
    case "plugin_market":
      return [
        {
          id: "tinker.nl-zh",
          name: "自然语言搜索（中文）",
          version: "1.0.0",
          kind: "nlu",
          description: "说人话就能搜：今天下载的 pdf、上个月改过的 word。",
          author: "tinker",
          min_app: "1.0.0",
          download: "builtin:tinker.nl-zh",
          sha256: "",
          changelog: "首个版本",
        },
      ];
    case "plugin_market_url":
      return "";
    default:
      return null;
  }
}
