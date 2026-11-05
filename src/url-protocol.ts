// tinker:// 协议解析
// 把外部递进来的链接还原成一个结构化的「意图」，由 App.vue 落到界面状态上。
// 参数表与安全边界参考原项目《URL协议简介》：协议是外界递进来的字符串，
// 而本程序以管理员权限运行，所以能做的事被限死在「搜索与界面设置」——
// run= 打开可执行/脚本类文件一律硬拒，连确认框都不弹。

export interface ChainSeg {
  q: string;
  mode?: string;
}

export interface UrlIntent {
  action: "new" | "search" | "focus";
  keyword: string | null;
  /** 多重搜索链（ms=） */
  chain: ChainSeg[] | null;
  /** 模式：引擎类型值（-4~2），多重链时为 -2 */
  modeType: number | null;
  /** 命中的自定义模式名 */
  modeName: string | null;
  filters: string[] | null;
  scope: { path: string; rec: boolean }[] | null;
  view: string | null;
  skin: string | null;
  zoom: number | null;
  preview: boolean | null;
  sort: { field: string; asc: boolean } | null;
  win: string | null;
  save: boolean;
  hotkey: string | null;
  settings: string | null;
  reveal: string | null;
  props: string | null;
  openFile: string | null;
  addMode: { name: string; kind: string; tpl: string; desc: string; priv: boolean } | null;
  delMode: string | null;
  clearTags: boolean;
  /** 匹配开关（?pinyinfull=1 一类），键为配置字段名 */
  match: Record<string, boolean>;
  /** 被拒 / 被忽略的原因，落到右下角提示 */
  notes: string[];
}

/** 超过这个长度的链接整条忽略（原项目 32KB） */
export const MAX_URL_LEN = 32 * 1024;

const ACTIONS: Record<string, UrlIntent["action"]> = {
  new: "new",
  open: "new",
  search: "search",
  find: "search",
  focus: "focus",
};

/** 模式别名 → 搜索类型值 */
const MODE_ALIAS: Record<string, number> = {
  "0": 0,
  wild: 0,
  wildcard: 0,
  通配符: 0,
  "1": 1,
  re: 1,
  regex: 1,
  正则: 1,
  "2": 2,
  sql: 2,
};

/** 自定义模式的类型别名 → 内部 kind（配置里存的字符串） */
const KIND_ALIAS: Record<string, string> = {
  "0": "wildcard",
  wild: "wildcard",
  wildcard: "wildcard",
  通配符: "wildcard",
  "1": "regex",
  re: "regex",
  regex: "regex",
  正则: "regex",
  "2": "sql",
  sql: "sql",
};

/** run= 的硬拒名单：双击就等于运行代码的扩展名 */
const BLOCKED_EXT = new Set(
  (
    "exe com scr pif bat cmd ps1 ps1xml psm1 vbs vbe js jse wsf wsh ws " +
    "hta lnk url msi msp msix appx appxbundle iso img vhd vhdx chm hlp " +
    "docm dotm xlsm xltm pptm potm xlam jar cpl msc reg inf ins theme " +
    "deskthemepack scf shb slk gadget"
  ).split(/\s+/)
);

/** 视图档位别名 → 内部 view.mode */
const VIEW_ALIAS: Record<string, string> = {
  dense: "dense",
  list: "list",
  details: "list",
  medium: "grid",
  large: "grid",
  grid: "grid",
  严密: "dense",
  紧凑: "dense",
  详情: "list",
  列表: "list",
  中等: "grid",
  大图标: "grid",
};

/** 设置页名 → 内部 tab key */
const SETTINGS_TAB: Record<string, string> = {
  搜索匹配: "search",
  搜索: "search",
  匹配: "search",
  索引: "index",
  索引管理: "index",
  外观: "appearance",
  窗口: "appearance",
  外观与窗口: "appearance",
  关于: "about",
  search: "search",
  index: "index",
  appearance: "appearance",
  about: "about",
};

/** 匹配开关参数名 → 配置字段名 */
const MATCH_KEYS: Record<string, string> = {
  pinyinfull: "pinyin_full",
  pinyininitial: "pinyin_initial",
  pinyinexact: "pinyin_exact",
  wildstar: "wildcard_star",
  wildq: "wildcard_q",
  fullwidth: "fullwidth",
  casesensitive: "case_sensitive",
  emptyall: "empty_all",
};

/** 可被 force= 覆盖的项 */
const FORCEABLE = new Set([
  "mode",
  "view",
  "preview",
  "zoom",
  "skin",
  "pos",
  "size",
  "cfg",
  "scope",
  "sort",
  "order",
  "filter",
  "cat",
  "hotkey",
  ...Object.keys(MATCH_KEYS),
]);

function dec(s: string): string {
  // tinker:// 里 '+' 不表示空格，只解百分号编码
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

function halfWidth(s: string): string {
  // 参数名允许是全角／大小写混乱，统一成小写半角
  return s
    .replace(/[\uFF01-\uFF5E]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0))
    .replace(/\u3000/g, " ")
    .trim()
    .toLowerCase();
}

/** 拆 query：只解百分号编码，不把 '+' 当空格；同名参数取第一个 */
export function parseQuery(q: string): Record<string, string> {
  const out: Record<string, string> = {};
  if (!q) return out;
  for (const pair of q.split("&")) {
    if (!pair) continue;
    const eq = pair.indexOf("=");
    const k = halfWidth(dec(eq < 0 ? pair : pair.slice(0, eq)));
    const v = eq < 0 ? "" : dec(pair.slice(eq + 1));
    if (!k || k in out) continue;
    out[k] = v;
  }
  return out;
}

/** 浏览器会把路径里的 \ 规整成 /，这里再归一回 */
export function normPath(p: string): string {
  let s = p.trim().replace(/\//g, "\\");
  if (/^[a-z]:\\/i.test(s)) s = s[0].toUpperCase() + s.slice(1);
  return s;
}

function isAbsPath(p: string): boolean {
  return /^[a-zA-Z]:\\/.test(p) || p.startsWith("\\\\");
}

function truthy(v: string | undefined): boolean | null {
  if (v === undefined) return null;
  const s = halfWidth(v);
  if (["1", "true", "yes", "on", "是", "开"].includes(s)) return true;
  if (["0", "false", "no", "off", "否", "关"].includes(s)) return false;
  return null;
}

function parseFilters(v: string): string[] | null {
  const s = v.trim();
  if (!s) return null;
  let list: string[] = [];
  if (s.startsWith("[")) {
    try {
      const j = JSON.parse(s);
      if (Array.isArray(j)) list = j.map((x) => String(x));
    } catch {
      /* 落到分隔符解析 */
    }
  }
  if (!list.length) {
    list = s
      .split(/[、,;，]/)
      .map((x) => x.trim())
      .filter(Boolean);
  }
  list = list.filter((x) => x && x !== "全部");
  return list.length ? list : ["全部"];
}

function parseScope(v: string): { path: string; rec: boolean }[] | null {
  const s = v.trim();
  if (!s) return null;
  if (s.startsWith("[") || s.startsWith("{")) {
    try {
      const j = JSON.parse(s);
      const arr = Array.isArray(j) ? j : [j];
      const out: { path: string; rec: boolean }[] = [];
      for (const it of arr) {
        if (typeof it === "string") {
          out.push({ path: normPath(it), rec: true });
        } else if (it && typeof it === "object") {
          const p = it.p ?? it.path ?? it.路径 ?? "";
          const rec = it.rec ?? it.递归子目录 ?? it.r ?? true;
          if (p) out.push({ path: normPath(String(p)), rec: rec !== false });
        }
      }
      if (out.length) return out;
    } catch {
      /* 当单条路径处理 */
    }
  }
  return [{ path: normPath(s), rec: true }];
}

function parseChain(v: string): ChainSeg[] | null {
  try {
    const j = JSON.parse(v.startsWith("[") ? v : `[${v}]`);
    if (!Array.isArray(j)) return null;
    const out: ChainSeg[] = [];
    for (const it of j) {
      if (typeof it === "string") out.push({ q: it });
      else if (it && typeof it === "object" && it.q != null) {
        out.push({ q: String(it.q), mode: it.mode != null ? String(it.mode) : undefined });
      }
    }
    return out.length ? out : null;
  } catch {
    return null;
  }
}

/** 主入口：解析一条 tinker:// 链接；不是本协议的返回 null */
export function parseUrl(raw: string): UrlIntent | null {
  const url = raw.trim();
  if (!url || url.length > MAX_URL_LEN) return null;
  const m = /^tinker:\/\/([^?]*)(?:\?(.*))?$/i.exec(url);
  if (!m) return null;

  const head = m[1].replace(/\/+$/, "");
  let qs = m[2] || "";
  const hash = qs.indexOf("#");
  if (hash >= 0) qs = qs.slice(0, hash);
  const q = parseQuery(qs);

  const it: UrlIntent = {
    action: ACTIONS[halfWidth(head)] || "new", // 不认的动作按 new 处理，不报错
    keyword: null,
    chain: null,
    modeType: null,
    modeName: null,
    filters: null,
    scope: null,
    view: null,
    skin: null,
    zoom: null,
    preview: null,
    sort: null,
    win: null,
    save: false,
    hotkey: null,
    settings: null,
    reveal: null,
    props: null,
    openFile: null,
    addMode: null,
    delMode: null,
    clearTags: false,
    match: {},
    notes: [],
  };

  // ---- 搜索参数 ----
  if (q.q != null && q.q !== "") it.keyword = q.q;
  if (q.keyword != null && it.keyword == null) it.keyword = q.keyword;
  if (q.ms) {
    const chain = parseChain(q.ms);
    if (chain) {
      it.chain = chain;
      it.modeType = -2;
    } else {
      it.notes.push("ms= 不是合法的 JSON 链，已忽略");
    }
  }
  if (q.mode) {
    const t = halfWidth(q.mode);
    if (t in MODE_ALIAS) it.modeType = MODE_ALIAS[t];
    else it.modeName = q.mode; // 交给前端按自定义模式名匹配
  }
  if (q.filter || q.cat) it.filters = parseFilters(q.filter || q.cat || "");
  if (q.scope) {
    const s = parseScope(q.scope);
    if (s) it.scope = s;
  }

  // ---- 窗口参数 ----
  const view = q.view ? VIEW_ALIAS[halfWidth(q.view)] : undefined;
  if (q.view && !view) it.notes.push(`view=${q.view} 不是认识的视图档位`);
  it.view = view || null;
  if (q.skin) it.skin = q.skin;
  if (q.zoom) {
    const z = Number(q.zoom);
    if (Number.isFinite(z)) it.zoom = Math.min(200, Math.max(50, z));
  }
  it.preview = truthy(q.preview);
  if (q.sort) {
    const order = halfWidth(q.order || "asc");
    it.sort = { field: q.sort, asc: order !== "desc" };
  } else if (q.order) {
    it.notes.push("order= 需要与 sort= 一起用");
  }
  if (q.win) it.win = q.win;
  it.save = truthy(q.save) === true;
  if (q.hotkey) {
    const hk = halfWidth(q.hotkey);
    if (hk === "none") it.hotkey = "";
    else if (!/(ctrl|alt|shift|win)\+/.test(hk)) {
      it.notes.push(`hotkey=${q.hotkey} 缺少修饰键（裸键会把该键从全系统拦下来），已忽略`);
    } else it.hotkey = hk;
  }
  if (q.settings) it.settings = SETTINGS_TAB[halfWidth(q.settings)] || "search";

  // ---- 文件动作 ----
  if (q.reveal) {
    const p = normPath(q.reveal);
    if (!isAbsPath(p)) it.notes.push(`reveal= 需要绝对路径：${q.reveal}`);
    else it.reveal = p;
  }
  if (q.props) {
    const p = normPath(q.props);
    if (!isAbsPath(p)) it.notes.push(`props= 需要绝对路径：${q.props}`);
    else it.props = p;
  }
  if (q.run) {
    const p = normPath(q.run);
    const ext = (/\.([^.\\]+)$/.exec(p)?.[1] || "").toLowerCase();
    if (!isAbsPath(p)) {
      it.notes.push(`run= 需要绝对路径：${q.run}`);
    } else if (BLOCKED_EXT.has(ext)) {
      it.notes.push(`已拒绝 run=${p}：可执行 / 脚本类文件不在白名单`);
    } else {
      it.openFile = p;
    }
  }

  // ---- 搜索模式管理 ----
  if (q.addmode) {
    const name = q.addmode.trim();
    if (!name) it.notes.push("addmode= 缺少模式名");
    else {
      const raw = halfWidth(q.mtype || "wildcard");
      it.addMode = {
        name,
        kind: KIND_ALIAS[raw] || "wildcard",
        tpl: q.tpl || "",
        desc: q.mdesc || "",
        priv: truthy(q.priv) === true,
      };
    }
  }
  if (q.delmode) it.delMode = q.delmode.trim() || null;
  it.clearTags = truthy(q.cleartags) === true;

  // ---- 匹配开关 ----
  for (const [param, field] of Object.entries(MATCH_KEYS)) {
    const b = truthy(q[param]);
    if (b != null) it.match[field] = b;
  }

  // ---- force=（单窗口实现里无需覆盖，但写错要提示） ----
  if (q.force) {
    const items = q.force
      .split(/[,,]/)
      .map((x) => halfWidth(x))
      .filter(Boolean);
    for (const item of items) {
      if (item !== "all" && !FORCEABLE.has(item)) it.notes.push(`force= 里有不认识的项：${item}`);
    }
  }

  return it;
}
