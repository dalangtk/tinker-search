<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, nextTick, ref, watch } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { ElMessage, ElMessageBox } from "element-plus";
import Icon from "./components/Icon.vue";
import TitleBar from "./components/TitleBar.vue";
import SearchBar from "./components/SearchBar.vue";
import ResultList from "./components/ResultList.vue";
import ResultGrid from "./components/ResultGrid.vue";
import PreviewPane from "./components/PreviewPane.vue";
import StatusBar from "./components/StatusBar.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import SqlTable from "./components/SqlTable.vue";
import SideBar from "./components/SideBar.vue";
import HomeView from "./components/HomeView.vue";
import { api, IS_MOCK, onEvent } from "./api";
import {
  MODES,
  NL_MODE,
  type AppConfig,
  type FacetTag,
  type Facets,
  type FilterCat,
  type HomeSnapshot,
  type IndexStats,
  type ModeDef,
  type NlQuery,
  type Row,
} from "./types";
import {
  epoch,
  ensure,
  ensureIndices,
  errorMsg,
  elapsed,
  fingerprint,
  reset,
  resetPages,
  rowAt,
  searching,
  setIcon,
  sqlMode,
  total,
} from "./store/results";
import { formatCount, formatSize } from "./utils/format";
import { parseUrl, type ChainSeg } from "./url-protocol";

/* ------------------------------ 状态 ------------------------------ */

const info = ref<any>({});
const cfg = ref<AppConfig | null>(null);
const stats = ref<IndexStats | null>(null);
const keyword = ref("");
const modeKey = ref("wildcard");
const filters = ref<FilterCat[]>([]);
const sortFields = ref<string[]>([]);
const selectedFilters = ref<string[]>(["全部"]);
/** 侧栏「常用位置」选中的目录（多选） */
const facetPlaces = ref<string[]>([]);
/** 侧栏「磁盘」选中的盘符（多选） */
const facetDrives = ref<string[]>([]);
/** 首页快照：磁盘容量 / 桌面 / 各盘根目录 */
const home = ref<HomeSnapshot | null>(null);
const homeLoading = ref(false);
const selected = ref(0);
/** Ctrl 多选的行号集合；始终包含主选中行 */
const picked = ref<Set<number>>(new Set([0]));
const pickedCount = computed(() => picked.value.size);
const previewRow = ref<Row | null>(null);
const scope = ref<string | null>(null);
const sqlData = ref<any>(null);
/** SQL 表格里选中的行（从 0 起）以及它对应的完整记录，供右侧预览与双击打开 */
const sqlSel = ref(0);
const sqlRow = ref<Row | null>(null);
const showSettings = ref(false);
const settingsTab = ref("search");
const kwFocused = ref(false);
const history = ref<string[]>([]);
const ctx = ref<{ row: Row; x: number; y: number } | null>(null);
const bootNotice = ref("");
const maximized = ref(false);
const showOverlayClosed = ref(false);
const theme = ref("default");
const needAdmin = ref(false);
/** 多重搜索链（tinker://…?ms=[…]）：非空时以引擎 MULTI 模式提交 */
const chain = ref<ChainSeg[] | null>(null);
/** 可用的皮肤名，来自 app.css 里的 html[data-theme] */
const SKINS = ["default", "dark", "sakura", "mint", "cyber", "glass"];

/**
 * 自然语言搜索由插件提供：没装或没启用时，模式列表里不出现这一项。
 * 这也是「插件化」的实际意义——不想要的人一句都不会被这套词表影响。
 */
const nlReady = ref(false);
const nlPluginName = ref("");
const nlParsed = ref<NlQuery | null>(null);

async function loadNlStatus() {
  try {
    const st = await api.nlStatus();
    nlReady.value = !!st.ready;
    nlPluginName.value = st.name || "";
    if (!nlReady.value && modeKey.value === NL_MODE.key) {
      modeKey.value = MODES[0].key;
    }
  } catch {
    nlReady.value = false;
  }
}

/** 输入时实时显示「已理解为…」，不用等提交 */
let nlTimer: ReturnType<typeof setTimeout> | null = null;
watch([keyword, modeKey], ([text, key]) => {
  const isNl = key === NL_MODE.key && nlReady.value;
  if (nlTimer) clearTimeout(nlTimer);
  if (!isNl) {
    nlParsed.value = null;
    return;
  }
  if (!text.trim()) {
    nlParsed.value = null;
    return;
  }
  nlTimer = setTimeout(() => {
    api.nlParse(text).then(
      (q) => {
        if (modeKey.value === NL_MODE.key) nlParsed.value = q;
      },
      () => {
        /* 插件被停用时静默失败，loadNlStatus 会收拾状态 */
      }
    );
  }, 250);
});

const searchBar = ref<InstanceType<typeof SearchBar> | null>(null);

const allModes = computed<ModeDef[]>(() => {
  const custom: ModeDef[] = (cfg.value?.custom_modes || []).map((m) => ({
    key: `custom:${m.id}`,
    label: m.name,
    type: kindToType(m.kind),
    hint: m.desc || m.template,
    custom: m,
  }));
  return [...MODES, ...(nlReady.value ? [NL_MODE] : []), ...custom];
});

function kindToType(kind: string): number {
  switch (kind) {
    case "regex":
      return 1;
    case "sql":
      return 2;
    default:
      return 0;
  }
}

const mode = computed<ModeDef>(
  () => allModes.value.find((m) => m.key === modeKey.value) || MODES[0]
);

const view = computed(() => cfg.value?.view.mode || "list");
const previewVisible = computed(() => !!cfg.value?.view.preview);
const tileW = computed(() => {
  const z = cfg.value?.view.zoom || 0;
  return z >= 100 ? z : 116;
});
const rowH = computed(() => (view.value === "dense" ? 26 : 38));
const admin = computed(() => !!info.value.admin);
const isDark = computed(() => ["dark", "cyber"].includes(theme.value));
const ready = computed(() => !!stats.value?.ready);
const scanning = computed(() => !!stats.value?.scanning);
const showOverlay = computed(
  () =>
    !showOverlayClosed.value &&
    (needAdmin.value || (scanning.value && total.value === 0))
);

const interacted = ref(false);
const showHistory = computed(
  () =>
    kwFocused.value &&
    interacted.value &&
    keyword.value.trim() === "" &&
    history.value.length > 0 &&
    !showSettings.value
);

/** 侧栏选中项的统一形态：同一类内「或」，跨类「且」 */
const facets = computed<Facets>(() => ({
  places: facetPlaces.value,
  types: selectedFilters.value.filter((n) => n !== "全部"),
  drives: facetDrives.value,
}));

const hasFacets = computed(
  () =>
    facets.value.places.length > 0 ||
    facets.value.types.length > 0 ||
    facets.value.drives.length > 0
);

/** 地址栏里显示的标签：每种分类一个 */
const tags = computed<FacetTag[]>(() => {
  const out: FacetTag[] = [];
  const pn = facets.value.places.map(
    (p) => home.value?.places.find((x) => x.path === p)?.name || p.split("\\").pop() || p
  );
  if (pn.length) out.push({ kind: "places", label: "位置", text: pn[0], count: pn.length });
  const tn = facets.value.types;
  if (tn.length) out.push({ kind: "types", label: "类型", text: tn[0], count: tn.length });
  const dn = facets.value.drives;
  if (dn.length) out.push({ kind: "drives", label: "磁盘", text: dn.join(" "), count: dn.length });
  return out;
});

/** 空结果时的一句话解释：最需要说清的是「这个盘还没建索引」 */
const emptyHint = computed(() => {
  const ds = home.value?.drives || [];
  const missing = facets.value.drives.filter(
    (l) => ds.find((d) => d.letter === l)?.indexed === false
  );
  if (missing.length) {
    return `${missing.join(" ")} 还没建索引，先在设置里把这块盘收录进来才有结果`;
  }
  return "";
});

/** 首屏：没有关键词、没有筛选、也没有进入某个目录时，列表区显示「磁盘 / 桌面 / 各盘内容」 */
const browsingHome = computed(
  () =>
    !chain.value &&
    !sqlMode.value &&
    keyword.value.trim() === "" &&
    !hasFacets.value &&
    !scope.value
);

/* ------------------------------ 初始化 ------------------------------ */

let unlisteners: (() => void)[] = [];
let pollTimer: number | null = null;
let searchSeq = 0;
let firstReady = false;
let lastUrl = "";
let lastUrlAt = 0;
let lastNotice = "";

function applyTheme(name: string) {
  theme.value = name;
  document.documentElement.dataset.theme = name;
  document.documentElement.classList.toggle("dark", ["dark", "cyber"].includes(name));
}

async function boot() {
  try {
    info.value = await api.info();
  } catch (e) {
    ElMessage.error(`引擎初始化失败：${e}`);
    return;
  }
  try {
  cfg.value = info.value.config as AppConfig;
  applyTheme(cfg.value.theme || "default");
  history.value = cfg.value.history || [];
  // 启动一律回到「全部」：首屏该是磁盘 / 桌面概览，
  // 而不是被上次会话残留的分类筛选直接拖进结果列表。
  selectedFilters.value = ["全部"];
  if (cfg.value.view.zoom && cfg.value.view.zoom < 100) cfg.value.view.zoom = 116;

  api.alwaysOnTop(!!cfg.value.window.always_on_top).catch(() => {});
  // 全局快捷键由后端在启动时按 config 应用（支持自定义串，见 set_hotkey）

  await refreshMeta();
  registerEvents();
  void refreshStats();
  pollTimer = window.setInterval(refreshStats, 1000);
  // 侧栏与首页的数据：磁盘容量、桌面、各盘根目录
  void loadHome();
  // 把持久化的「文件类型」选中态同步给引擎（原生后端需要显式下发）
  api.setFilter(selectedFilters.value).catch(() => {});

    if (ready.value) {
      firstReady = true;
      if (!browsingHome.value) setTimeout(() => runSearch(keyword.value), 60);
    }

    // 启动参数里带进来的 tinker:// 链接（配置已就绪，此时应用才完整）
    const pending = await api.takePendingUrl().catch(() => null);
    if (pending) await handleUrl(pending);
    else if (IS_MOCK) {
      // 仅界面预览模式：?mock=1&url=<编码后的 tinker:// 链接>
      const u = new URLSearchParams(location.search).get("url");
      if (u) await handleUrl(u);
    }
  } catch (e) {
    console.error("[boot] 初始化流程异常", e);
  }
}

async function refreshMeta() {
  try {
    filters.value = await api.filters();
  } catch {
    /* 索引未就绪 */
  }
  try {
    sortFields.value = await api.sortFields();
  } catch {
    /* ignore */
  }
}

async function refreshStats() {
  try {
    const s = await api.indexStatus();
    const wasScanning = stats.value?.scanning;
    stats.value = s;
    if (s.ready && !firstReady) {
      firstReady = true;
      await refreshMeta();
      if (!browsingHome.value) void runSearch(keyword.value);
    }
    if (wasScanning && !s.scanning) {
      showOverlayClosed.value = false;
      void refreshMeta();
      // 新库的目录结构变了，首页快照要重新取
      void loadHome(true);
      setTimeout(() => !browsingHome.value && runSearch(keyword.value), 200);
    }
    if (s.scanning) {
      showOverlayClosed.value = false;
    }
    // 引擎在非管理员下建索引会静默失败，所以"要不要提权"由后端判定后下发，
    // 不依赖 boot 阶段的事件（那时前端还没订阅）
    if (s.needs_admin) needAdmin.value = true;
    // 同一条提示只弹一次（stats 每秒都会带上它）
    if (s.notice && s.notice !== lastNotice) {
      lastNotice = s.notice;
      bootNotice.value = s.notice;
    }
  } catch {
    /* ignore */
  }
}

/* ------------------------------ 搜索 ------------------------------ */

/** 清空结果区（回到首屏或等待新查询） */
function emptyNow() {
  reset(0, fingerprint.value);
  total.value = 0;
  searching.value = false;
  errorMsg.value = "";
  previewRow.value = null;
}

async function runSearch(kw?: string) {
  if (!ready.value || scanning.value) return;
  const text = (kw ?? keyword.value).trim();

  // 空输入：SQL 模式只显示语法提示，其余情况回首页
  if (!text) {
    if (mode.value.type === 2) {
      ++searchSeq;
      emptyNow();
      sqlMode.value = true;
      sqlData.value = null;
      return;
    }
    if (!hasFacets.value && !chain.value?.length && !scope.value) {
      ++searchSeq;
      emptyNow();
      return;
    }
  }

  const my = ++searchSeq;
  searching.value = true;
  try {
    const useChain = !!chain.value?.length;

    // 自然语言模式：翻译成 关键词 + 类型 + 时间区间 后再交给引擎
    if (!useChain && mode.value.type === 3) {
      const res = await api.nlSearch(
        text,
        scope.value,
        facets.value.places,
        facets.value.drives
      );
      if (my !== searchSeq) return;
      fingerprint.value = res.fingerprint;
      nlParsed.value = res.nl;
      return;
    }

    const payload = useChain ? JSON.stringify(chain.value) : kw ?? keyword.value;
    const res = await api.search(
      payload,
      useChain ? -2 : mode.value.type,
      scope.value,
      facets.value.places,
      facets.value.drives
    );
    if (my !== searchSeq) return;
    fingerprint.value = res.fingerprint;
  } catch (e) {
    if (my !== searchSeq) return;
    searching.value = false;
    reset(0, fingerprint.value);
    total.value = 0;
    errorMsg.value = String(e);
    // SQL 语法/执行错误：留住 SQL 面板，把错误显示在里面
    if (mode.value.type === 2) {
      sqlMode.value = true;
      sqlData.value = null;
    }
  }
}

/** 语义重排的序号：新搜索进来就作废上一次重排结果 */
let rerankSeq = 0;
/** 最近一次重排耗时（毫秒），供状态栏显示 */
const rerankMs = ref(0);

/**
 * 语义重排：只在「普通搜索 + 关键词够长」时做。
 *
 * 自然语言模式的关键词经常是空的（条件全在时间/后缀里），此时重排没有意义；
 * 带通配符的查询本身就是精确匹配，也不该被语义分打乱。
 * 任何失败都静默跳过——语义只是加分项，不该让搜索报错。
 */
async function maybeRerank(fp: number) {
  const s = cfg.value?.semantic;
  if (!s?.enabled) return;
  const q = keyword.value.trim();
  if (q.length < 2 || /[*?]/.test(q)) return;
  const my = ++rerankSeq;
  try {
    const r = await api.semanticRerank(q, s.top);
    // 期间又搜了新词，或指纹已变，丢弃这次结果
    if (my !== rerankSeq || fp !== fingerprint.value) return;
    if (r.moved <= 0) return;
    resetPages();
    await ensure(0, 40);
    rerankMs.value = Math.round(r.elapsed_ms);
  } catch {
    // 模型不可用 / 结果太少：保持原顺序
  }
}

/** 首页快照（磁盘容量 / 桌面 / 各盘根目录） */
async function loadHome(force = false) {
  if (homeLoading.value) return;
  if (home.value && !force) return;
  homeLoading.value = true;
  try {
    home.value = await api.homeSnapshot();
  } catch (e) {
    console.error("[home] 读取失败", e);
  } finally {
    homeLoading.value = false;
  }
}

/* ------------------------------ SQL 面板 ------------------------------ */

const SQL_SAMPLES = [
  "SELECT Path, Size FROM alltable WHERE Ext = 'pdf' AND Size > '5M' ORDER BY Size DESC LIMIT 50",
  "SELECT Path, Size, Mtime FROM alltable WHERE FName LIKE '%报告%' ORDER BY Mtime DESC LIMIT 100",
  "SELECT Ext, COUNT(*) FROM alltable GROUP BY Ext ORDER BY COUNT(*) DESC LIMIT 10",
  "SELECT Drive, COUNT(*) FROM alltable GROUP BY Drive",
];

function useSqlSample(sql: string) {
  const m = allModes.value.find((x) => x.type === 2);
  if (m) modeKey.value = m.key;
  keyword.value = sql;
  chain.value = null;
  sqlMode.value = true;
  sqlData.value = null;
  void runSearch();
}

/* ------------------------------ 分类筛选 ------------------------------ */

function onFacetToggle(kind: "places" | "types" | "drives", value: string) {
  if (kind === "types") {
    void toggleType(value);
    return;
  }
  const list = kind === "places" ? facetPlaces : facetDrives;
  const set = new Set(list.value);
  if (set.has(value)) set.delete(value);
  else set.add(value);
  list.value = Array.from(set);
  void runSearch();
}

/** 文件类型走引擎的 filter 通道，两条后端都认 */
async function toggleType(name: string) {
  const set = new Set(selectedFilters.value.filter((n) => n !== "全部"));
  if (set.has(name)) set.delete(name);
  else set.add(name);
  selectedFilters.value = set.size ? Array.from(set) : ["全部"];
  await applyFilter();
  void runSearch();
}

async function clearFacet(kind: "places" | "types" | "drives") {
  if (kind === "places") facetPlaces.value = [];
  else if (kind === "drives") facetDrives.value = [];
  else selectedFilters.value = ["全部"];
  if (kind === "types") await applyFilter();
  void runSearch();
}

async function clearAllFacets() {
  facetPlaces.value = [];
  facetDrives.value = [];
  selectedFilters.value = ["全部"];
  await applyFilter();
  void runSearch();
}

/** 侧栏「回到首页」：清空关键词与所有筛选 */
async function goHome() {
  keyword.value = "";
  scope.value = null;
  chain.value = null;
  modeKey.value = "wildcard";
  facetPlaces.value = [];
  facetDrives.value = [];
  selectedFilters.value = ["全部"];
  // 从 SQL 模式回首页时得把 SQL 面板一起收掉，
  // 否则空关键词 + 非首页判定会让主区域一直停在旧表格上
  sqlMode.value = false;
  sqlData.value = null;
  await applyFilter();
  emptyNow();
  void loadHome();
}

/** 首页里点磁盘卡片 / 目录标题，直接落成筛选 */
function pickDrive(letter: string) {
  facetDrives.value = [letter];
  void runSearch();
}

function usePlace(path: string) {
  if (!path) return;
  facetPlaces.value = [path];
  void runSearch();
}

function homeOpen(path: string, isDir = false) {
  if (isDir) {
    void enterFolder(path);
    return;
  }
  api.openFile(path).catch((e) => ElMessage.error(String(e)));
}

function revealRowPath(path: string) {
  api.reveal(path).catch((e) => ElMessage.error(String(e)));
}

/** 清掉多重搜索链，回到普通搜索框 */
function clearChain() {
  if (!chain.value) return;
  chain.value = null;
  void runSearch();
}

function onInput(v: string) {
  keyword.value = v;
  // 开始输入即视为回到普通搜索，链尾那一段由输入接管
  chain.value = null;
  if (ctx.value) ctx.value = null;
  void runSearch(v);
}

function onSubmit() {
  if (!keyword.value.trim()) return;
  history.value = [
    keyword.value.trim(),
    ...history.value.filter((h) => h !== keyword.value.trim()),
  ].slice(0, 200);
  api.historyAdd(keyword.value).catch(() => {});
  void runSearch();
}

function changeMode(m: ModeDef) {
  modeKey.value = m.key;
  chain.value = null;
  // SQL 模式没有「结果列表」，切过去就先摆出语法提示面板
  sqlMode.value = m.type === 2;
  if (sqlMode.value) sqlData.value = null;
  if (m.custom) {
    // 自定义模式：模板里的 <keyword> 替换为当前输入
    const tpl = m.custom.template || "";
    if (tpl.includes("<keyword>")) {
      keyword.value = tpl.replace("<keyword>", keyword.value);
    }
  }
  void runSearch();
}

async function applyFilter() {
  try {
    await api.setFilter(selectedFilters.value);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function pickScope() {
  try {
    const dir = await openDialog({ directory: true, multiple: false, title: "限定搜索目录" });
    if (typeof dir === "string") {
      scope.value = dir;
      void runSearch();
    }
  } catch (e) {
    ElMessage.error(`无法打开目录选择框：${e}`);
  }
}

/** 第一次点某列时的默认方向：时间/大小/相关度从大到小（新→旧、大→小），名称类从小到大 */
const DESC_FIRST = new Set(["相关度", "文件大小", "修改时间", "创建时间", "访问时间"]);

async function changeSort(field: string) {
  if (!cfg.value) return;
  const prev = cfg.value.sort;
  const same = prev.field === field;
  const asc = same ? !prev.asc : !DESC_FIRST.has(field);
  cfg.value.sort = { field, asc };
  try {
    await api.setSort(field, asc);
  } catch (e) {
    // 排序没生效时别让表头箭头停在错误的方向
    cfg.value.sort = prev;
    ElMessage.error(String(e));
  }
}

/* ------------------------------ 结果事件 ------------------------------ */

async function loadSql() {
  // 换了一条 SQL：行号从 0 重新开始，旧的选中行必须一起丢掉，
  // 否则会拿着上一轮的文件「假装」是新的第一行
  sqlSel.value = 0;
  sqlRow.value = null;
  try {
    const d = await api.sqlResult(0, 500);
    sqlData.value = d && d.字段数量 !== undefined ? d : null;
    if (sqlData.value) void pickSqlRow(0);
  } catch {
    sqlData.value = null;
  }
}

/** 表格一行 → 完整记录：原生后端给了「行id」，直接按 id 反查；
 *  DLL 后端没有这个字段时，退回用结果里的「路径」列临时拼一条。 */
async function sqlRowAt(i: number): Promise<Row | null> {
  const d = sqlData.value;
  if (!d) return null;
  const id = ((d["行id"] as number[]) || [])[i];
  if (typeof id === "number" && id > 0) {
    try {
      return await api.row(id);
    } catch {
      /* 索引在查询之后变动过，退回按路径拼 */
    }
  }
  const fields: string[] = d["字段"] || [];
  const cells: any[] = (d["行"] || [])[i] || [];
  const col = (...names: string[]) => {
    const k = fields.findIndex((f) => names.includes(String(f).toLowerCase()));
    return k >= 0 ? cells[k] : undefined;
  };
  const path = col("path", "路径", "fullpath");
  if (typeof path !== "string" || !path) return null;
  const name = String(col("name", "fname", "文件名") ?? path.split(/[\\/]/).pop() ?? path);
  const ext = /\.([^.\\/]+)$/.exec(name)?.[1]?.toLowerCase() ?? "";
  return {
    id: -1,
    name,
    path,
    parent: path.slice(0, Math.max(0, path.length - name.length - 1)),
    ext,
    size: Number(col("size", "大小") ?? 0),
    mtime: Number(col("mtime", "修改时间") ?? 0),
    ctime: Number(col("ctime", "创建时间") ?? 0),
    atime: 0,
    is_dir: col("dir", "是否目录") ? true : false,
    file_type: ext ? `${ext} 文件` : "文件",
    alias: "",
    rating: 0,
    attrs: 0,
    hl_file: [],
    hl_dir: [],
    hl_alias: [],
  };
}

/** 选中 SQL 表格的第 i 行：右侧预览跟着变，双击 / 回车即可打开 */
async function pickSqlRow(i: number) {
  if (sqlSel.value === i && sqlRow.value) return;
  sqlSel.value = i;
  sqlRow.value = await sqlRowAt(i);
}

/** 双击 SQL 结果行：和普通结果一样用系统默认程序打开 */
async function openSqlRow(i: number) {
  const r = sqlSel.value === i && sqlRow.value ? sqlRow.value : await sqlRowAt(i);
  if (!r) return;
  sqlSel.value = i;
  sqlRow.value = r;
  await openRow(r);
}

async function onSqlMenu(p: { i: number; x: number; y: number }) {
  const r = await sqlRowAt(p.i);
  if (!r) return;
  sqlSel.value = p.i;
  sqlRow.value = r;
  // SQL 结果表格不支持多选，右键菜单始终按单行处理
  clearPicked();
  onContextMenu({ row: r, x: p.x, y: p.y });
}

// SQL 结果被清掉（换关键词、回首页）时，选中态一起清
watch(sqlData, (v) => {
  if (!v) {
    sqlSel.value = 0;
    sqlRow.value = null;
  }
});

/** 状态栏在 SQL 模式下不报「N 个项目」，改说实际出了多少行 / 多少个分组 */
const sqlStatText = computed(() => {
  const d = sqlData.value;
  if (!sqlMode.value || !d) return "";
  if (d["聚合"]) return `${((d["分组"] as any[]) || []).length} 个分组`;
  return `${((d["行"] as any[]) || []).length} 行`;
});

watch(
  () => selected.value,
  async (i) => {
    if (total.value === 0) {
      previewRow.value = null;
      return;
    }
    await ensure(i, 1);
    const r = rowAt(i);
    if (r && r.id > 0) previewRow.value = r;
  }
);

watch(browsingHome, (v) => {
  if (v) void loadHome();
});

watch(
  () => cfg.value?.view.preview,
  (v) => {
    if (!v) previewRow.value = null;
  }
);

function registerEvents() {
  const add = async (name: string, cb: (p: any) => void) => {
    unlisteners.push(onEvent(name, cb));
  };

  void add("tinker:search-complete", async (p) => {
    // 指纹单调递增，比当前小的都是过期查询
    if (p.fingerprint < fingerprint.value) return;
    fingerprint.value = p.fingerprint;
    reset(p.count, p.fingerprint);
    total.value = p.count;
    elapsed.value = p.elapsedMs;
    rerankMs.value = 0; // 新结果：先撤掉上一次的语义标记
    searching.value = false;
    errorMsg.value = "";
    selectSingle(0);
    sqlData.value = null;

    if (p.searchType === 2) {
      sqlMode.value = true;
      await loadSql();
    } else {
      sqlMode.value = false;
    }

    if (p.count > 0) {
      await nextTick();
      void ensure(0, 40);
      setTimeout(async () => {
        const r = rowAt(0);
        if (r && r.id > 0) previewRow.value = r;
      }, 30);
      // SQL 与自然语言模式的结果本身已被条件精确约束，不做语义重排
      if (p.searchType !== 2 && p.searchType !== 3) void maybeRerank(p.fingerprint);
    } else {
      previewRow.value = null;
    }
  });

  void add("tinker:search-failed", (p) => {
    if (p.fingerprint >= 0 && p.fingerprint < fingerprint.value) return;
    searching.value = false;
    total.value = 0;
    reset(0, p.fingerprint || fingerprint.value);
    errorMsg.value = `${p.kind ? p.kind + "：" : ""}${p.message || "搜索失败"}${
      p.position >= 0 ? `（位置 ${p.position}）` : ""
    }`;
  });

  void add("tinker:result-changed", (p) => {
    // 文件系统同步变化：轻量刷新可见行
    if (p.count !== total.value) {
      total.value = p.count;
      reset(p.count, fingerprint.value);
    }
  });

  void add("tinker:icon-ready", (p) => {
    setIcon(p.id, 0, p.png);
  });

  // 托盘菜单「设置」：后端先把窗口叫回来，这里只负责把面板打开
  void add("ui:open-settings", (p) => {
    openSettings(p && p.tab ? p.tab : "search");
  });

  void add("tinker:engine-ready", (p) => {
    if (p.ok) {
      void refreshMeta();
      void refreshStats();
      setTimeout(() => runSearch(keyword.value), 100);
    } else if (p.error) {
      bootNotice.value = p.error;
    }
  });

  void add("tinker:scan-complete", (p) => {
    showOverlayClosed.value = false;
    ElMessage.success(
      `索引完成：${formatCount(p.fileCount)} 条，用时 ${(p.elapsedMs / 1000).toFixed(1)} 秒`
    );
    void refreshMeta();
    void refreshStats();
    setTimeout(() => runSearch(keyword.value), 300);
  });

  void add("tinker:scan-failed", (p) => {
    const reason = String(p.reason || "");
    if (reason.includes("管理员") || reason.includes("53")) {
      needAdmin.value = true;
    } else {
      ElMessage.error(`索引失败：${reason}`);
    }
  });

  void add("tinker:scan-terminated", () => {
    ElMessage.info("已停止遍历");
    void refreshStats();
  });

  void add("tinker:boot-notice", (p) => {
    bootNotice.value = p.message;
  });

  void add("tinker:sync-change", () => {
    /* 结果对象自身会发 result-changed，这里不额外刷新 */
  });

  void add("tinker:url", (p) => {
    handleUrl(p.url);
  });

  void add("tinker:partition-removed", (p) => {
    ElMessage.warning(`分区 ${p.drive} 已移除，相关索引数据仍保留`);
  });
}

/* ------------------------------ URL 协议 ------------------------------ */

/** 把 tinker:// 链接落到界面状态上。参数表见 url-protocol.ts */
async function handleUrl(raw: string) {
  // 同一秒内到达的同一条链接只应用一次（事件推送与启动取回可能撞车）
  const now = Date.now();
  if (raw === lastUrl && now - lastUrlAt < 1000) return;
  lastUrl = raw;
  lastUrlAt = now;

  const it = parseUrl(raw);
  if (!it) return;
  const notes = it.notes.slice();

  // 文件动作优先：与窗口当前状态无关
  try {
    if (it.reveal) await api.reveal(it.reveal);
    if (it.props) await api.props(it.props);
  } catch (e) {
    notes.push(String(e));
  }

  if (it.openFile) {
    // 非脚本类文档也要用户点头：链接是外界递进来的
    try {
      await ElMessageBox.confirm(`链接请求用系统程序打开：\n${it.openFile}`, "外部打开请求", {
        confirmButtonText: "打开",
        cancelButtonText: "取消",
        type: "warning",
      });
      await api.openFile(it.openFile);
    } catch {
      notes.push("外部请求已取消（未执行）");
    }
  }

  if (it.settings) openSettings(it.settings);

  // ---- 窗口外观 ----
  if (it.skin) {
    if (SKINS.includes(it.skin)) applyTheme(it.skin);
    else notes.push(`skin=${it.skin} 不是本机已有的皮肤名`);
  }
  if (it.view) await setView(it.view);
  if (it.zoom != null && cfg.value) {
    cfg.value.view.zoom = it.zoom;
    await api.saveCfg(cfg.value);
  }
  if (it.preview != null && cfg.value && !!cfg.value.view.preview !== it.preview) {
    await togglePreview();
  }
  if (it.sort && cfg.value) {
    cfg.value.sort = { field: it.sort.field, asc: it.sort.asc };
    try {
      await api.setSort(it.sort.field, it.sort.asc);
    } catch (e) {
      notes.push(String(e));
    }
  }
  if (it.filters) {
    selectedFilters.value = it.filters;
    await applyFilter();
  }
  if (it.scope) scope.value = it.scope[0]?.path ?? null;

  // ---- 匹配开关 ----
  if (cfg.value && Object.keys(it.match).length) {
    Object.assign(cfg.value.search, it.match);
    try {
      await api.setMatch(cfg.value.search);
      await api.saveCfg(cfg.value);
    } catch (e) {
      notes.push(String(e));
    }
  }

  // ---- 搜索模式管理 ----
  if (it.addMode) {
    await urlAddMode(it.addMode, notes);
  }
  if (it.delMode) {
    await urlDelMode(it.delMode, notes);
  }
  if (it.clearTags) {
    chain.value = null;
    keyword.value = "";
  }

  // ---- 搜索本体 ----
  if (it.chain) {
    chain.value = it.chain;
    keyword.value = "";
    await runSearch();
  } else {
    chain.value = null;
    if (it.modeName) {
      const found = allModes.value.find(
        (x) => (x.custom?.name || x.label) === it.modeName || x.key === it.modeName
      );
      if (found) modeKey.value = found.key;
      else notes.push(`不认识的搜索模式：${it.modeName}`);
    } else if (it.modeType != null) {
      const found = allModes.value.find((x) => x.type === it.modeType);
      if (found) modeKey.value = found.key;
    }
    if (it.keyword != null) keyword.value = it.keyword;
    if (it.keyword != null || it.modeType != null || it.modeName) await runSearch();
  }

  if (notes.length) ElMessage.warning(notes.join("；"));
}

/** 链接请求添加自定义搜索模式：摊开内容让用户点一次头 */
async function urlAddMode(
  m: { name: string; kind: string; tpl: string; desc: string; priv: boolean },
  notes: string[]
) {
  if (!cfg.value) return;
  if (!m.name) return;
  if ((cfg.value.custom_modes || []).some((x) => x.name === m.name)) {
    notes.push(`同名模式已存在：${m.name}`);
    return;
  }
  if ((cfg.value.custom_modes || []).length >= 40) {
    notes.push("自定义模式已达上限（40 个），已拒绝添加");
    return;
  }
  try {
    await ElMessageBox.confirm(
      `名称：${m.name}\n类型：${m.kind}\n模板：${m.tpl || "（空）"}\n` +
        `简介：${m.desc || "（空）"}\n作用范围：全局共享`,
      "链接请求添加搜索模式",
      { confirmButtonText: "添加", cancelButtonText: "取消", type: "warning" }
    );
    cfg.value.custom_modes.push({
      id: `u${Date.now().toString(36)}`,
      name: m.name,
      kind: m.kind,
      template: m.tpl,
      desc: m.desc,
    });
    await api.saveCfg(cfg.value);
    const added = allModes.value.find((x) => x.custom?.name === m.name);
    if (added) changeMode(added);
  } catch {
    notes.push("外部请求已取消（未执行）");
  }
}

async function urlDelMode(name: string, notes: string[]) {
  if (!cfg.value) return;
  const target = (cfg.value.custom_modes || []).find((x) => x.name === name);
  if (!target) {
    notes.push(`找不到模式：${name}`);
    return;
  }
  try {
    await ElMessageBox.confirm(
      `名称：${target.name}\n类型：${target.kind}\n模板：${target.template || "（空）"}\n` +
        `作用范围：全局共享\n\n删除后不可恢复。`,
      "链接请求删除搜索模式",
      { confirmButtonText: "删除", cancelButtonText: "取消", type: "warning" }
    );
    cfg.value.custom_modes = cfg.value.custom_modes.filter((x) => x.name !== name);
    if (modeKey.value === `custom:${target.id}`) modeKey.value = "wildcard";
    await api.saveCfg(cfg.value);
    void runSearch();
  } catch {
    notes.push("外部请求已取消（未执行）");
  }
}

/* ------------------------------ 文件操作 ------------------------------ */

/** 当前目录拆成面包屑：盘符 › 一级 › 二级 …，每一级都能点回去 */
const crumbs = computed(() => {
  const p = scope.value;
  if (!p) return [] as { name: string; path: string }[];
  const s = p.replace(/\//g, "\\").replace(/\\+$/, "");
  const out: { name: string; path: string }[] = [];
  let acc = "";
  for (const part of s.split("\\").filter(Boolean)) {
    acc = acc ? `${acc}\\${part}` : part;
    out.push({ name: part, path: acc });
  }
  return out;
});

/** 在应用内进入目录：目录变成搜索范围，空关键词直接浏览其内容 */
async function enterFolder(path: string) {
  const dir = path.replace(/\//g, "\\").replace(/\\+$/, "");
  if (!dir) return;
  scope.value = dir;
  keyword.value = "";
  chain.value = null;
  ctx.value = null;
  // 侧栏的「位置 / 磁盘」会和 scope 变成「且」条件，进入目录时先清掉
  facetPlaces.value = [];
  facetDrives.value = [];
  sqlMode.value = false;
  sqlData.value = null;
  if (mode.value.type === 2) modeKey.value = "wildcard";
  await applyFilter();
  await runSearch("");
}

/** 当前目录的上一级；盘符根目录返回 null（再往上就回首页） */
function parentOf(path: string): string | null {
  const p = path.replace(/[\\/]+$/, "");
  const i = p.lastIndexOf("\\");
  if (i <= 0) return null;
  const parent = p.slice(0, i);
  if (/^[A-Za-z]:$/.test(parent)) return null;
  return parent;
}

async function goUpFolder() {
  if (!scope.value) return;
  const up = parentOf(scope.value);
  if (up) await enterFolder(up);
  else await goHome();
}

async function openRow(r: Row) {
  // 目录在应用内继续下钻，不再唤起资源管理器
  if (r.is_dir) {
    await enterFolder(r.path);
    return;
  }
  // 文件夹快捷方式：目标目录在索引里就在应用内进入，否则交给系统打开
  if ((r.ext || "").toLowerCase() === "lnk") {
    try {
      const link = await api.resolveLink(r.path);
      if (link?.isDir && link.indexed && link.target) {
        await enterFolder(link.target);
        return;
      }
    } catch {
      /* 解析失败就交给系统打开 */
    }
  }
  try {
    await api.openFile(r.path);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function revealRow(r: Row) {
  try {
    await api.reveal(r.path);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function propsRow(r: Row) {
  try {
    await api.props(r.path);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function copyPath(r: Row) {
  await api.copyText(r.path);
  ElMessage.success("路径已复制");
}

async function copyFile(r: Row) {
  try {
    await api.copyPaths([r.path]);
    ElMessage.success("文件已放入剪贴板，可在资源管理器中粘贴");
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function renameRow(r: Row) {
  try {
    const { value } = await ElMessageBox.prompt("输入新的文件名（含扩展名）", "重命名", {
      inputValue: r.name,
      confirmButtonText: "重命名",
      cancelButtonText: "取消",
    });
    if (!value || value === r.name) return;
    await api.rename(r.path, value);
    ElMessage.success("已重命名");
    setTimeout(() => runSearch(), 250);
  } catch (e: any) {
    if (e !== "cancel") ElMessage.error(String(e?.message || e));
  }
}

async function aliasRow(r: Row) {
  try {
    const { value } = await ElMessageBox.prompt(
      "别名参与搜索匹配并可用于排序",
      "设置别名",
      {
        inputValue: r.alias || "",
        confirmButtonText: "保存",
        cancelButtonText: "取消",
      }
    );
    await api.setAlias(r.id, value ? value : null);
    ElMessage.success(value ? "别名已保存" : "别名已清除");
    setTimeout(() => runSearch(), 200);
  } catch (e: any) {
    if (e !== "cancel") ElMessage.error(String(e?.message || e));
  }
}

async function deleteRow(r: Row) {
  try {
    await ElMessageBox.confirm(`将“${r.name}”删除到回收站？`, "删除文件", {
      confirmButtonText: "删除到回收站",
      cancelButtonText: "取消",
      type: "warning",
    });
    await api.remove([r.path]);
    ElMessage.success("已删除到回收站");
    setTimeout(() => runSearch(), 300);
  } catch (e: any) {
    if (e !== "cancel" && e !== "close") ElMessage.error(String(e?.message || e));
  }
}

async function rate(r: Row, delta: number) {
  try {
    const v = await api.boostRating(r.id, delta);
    r.rating = v;
    ElMessage.success(`评分已调整为 ${v}（重新搜索后生效）`);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

/* ------------------------------ 多选 ------------------------------ */

/** 普通点击只选一个；Ctrl 切换；Shift 从主选中行连选 */
function onPick(index: number, additive = false, range = false) {
  if (range && selected.value >= 0) {
    const from = Math.min(selected.value, index);
    const to = Math.max(selected.value, index);
    const next = additive ? new Set(picked.value) : new Set<number>();
    for (let i = from; i <= to; i++) next.add(i);
    picked.value = next;
    selected.value = index;
    return;
  }
  if (additive) {
    const next = new Set(picked.value);
    if (next.has(index)) next.delete(index);
    else next.add(index);
    // 至少保留一个，否则后续键盘/预览没有锚点
    if (!next.size) next.add(index);
    picked.value = next;
    selected.value = index;
    return;
  }
  picked.value = new Set([index]);
  selected.value = index;
}

/** 单选状态下同步多选集合（键盘移动、搜索重置等） */
function selectSingle(index: number) {
  selected.value = index;
  picked.value = new Set([index]);
}

function clearPicked() {
  picked.value = new Set();
}

/** 把选中的行号解析成 Row；没进分页缓存的会先补加载 */
async function pickedRows(): Promise<Row[]> {
  const indices = Array.from(picked.value)
    .filter((i) => i >= 0 && i < total.value)
    .sort((a, b) => a - b);
  if (!indices.length) return [];
  await ensureIndices(indices);
  return indices
    .map((i) => rowAt(i))
    .filter((r): r is Row => !!r && r.id >= 0);
}

async function copyPickedFiles() {
  try {
    const rows = await pickedRows();
    if (!rows.length) return;
    await api.copyPaths(rows.map((r) => r.path));
    ElMessage.success(
      rows.length > 1
        ? `已复制 ${rows.length} 个文件到剪贴板，可在资源管理器中粘贴`
        : "文件已放入剪贴板，可在资源管理器中粘贴"
    );
  } catch (e: any) {
    ElMessage.error(String(e?.message || e));
  }
}

async function copyPickedPaths() {
  try {
    const rows = await pickedRows();
    if (!rows.length) return;
    await api.copyText(rows.map((r) => r.path).join("\r\n"));
    ElMessage.success(
      rows.length > 1 ? `已复制 ${rows.length} 个路径` : "路径已复制"
    );
  } catch (e: any) {
    ElMessage.error(String(e?.message || e));
  }
}

async function deletePicked() {
  try {
    const rows = await pickedRows();
    if (!rows.length) return;
    if (rows.length === 1) {
      await deleteRow(rows[0]);
      return;
    }
    await ElMessageBox.confirm(
      `将选中的 ${rows.length} 个文件删除到回收站？`,
      "批量删除",
      {
        confirmButtonText: "删除到回收站",
        cancelButtonText: "取消",
        type: "warning",
      }
    );
    await api.remove(rows.map((r) => r.path));
    ElMessage.success(`已删除 ${rows.length} 个文件到回收站`);
    clearPicked();
    setTimeout(() => runSearch(), 300);
  } catch (e: any) {
    if (e !== "cancel" && e !== "close") ElMessage.error(String(e?.message || e));
  }
}

function onContextMenu(p: { row: Row; index?: number; x: number; y: number }) {
  // 右键落在多选之外：把选择收拢到这一行；落在多选之内：保留整组
  if (p.index != null && p.index >= 0) {
    if (!picked.value.has(p.index)) {
      picked.value = new Set([p.index]);
      selected.value = p.index;
    }
  }
  const maxX = window.innerWidth - 200;
  const maxY = window.innerHeight - 300;
  ctx.value = { row: p.row, x: Math.min(p.x, maxX), y: Math.min(p.y, maxY) };
}

/* ------------------------------ 键盘 ------------------------------ */

function move(delta: number) {
  // SQL 模式的结果不在分页缓存里，得走表格自己的行号
  if (sqlMode.value) {
    const n = ((sqlData.value?.["行"] as any[]) || []).length;
    if (!n) return;
    const next = Math.max(0, Math.min(n - 1, sqlSel.value + delta));
    void pickSqlRow(next);
    return;
  }
  const n = total.value;
  if (!n) return;
  const next = Math.max(0, Math.min(n - 1, selected.value + delta));
  selectSingle(next);
}

function pageMove(dir: number) {
  const step = view.value === "grid" ? 24 : 12;
  move(dir * step);
}

/** Esc：先收菜单，再清关键词，再退出一层目录，最后隐藏窗口 */
async function onEscape() {
  if (ctx.value) {
    ctx.value = null;
    return;
  }
  if (keyword.value) {
    keyword.value = "";
    await runSearch("");
    return;
  }
  if (scope.value) {
    await goUpFolder();
    return;
  }
  api.hide();
}

function onKeydown(e: KeyboardEvent) {
  if (showSettings.value) return;
  const tag = (e.target as HTMLElement)?.tagName;
  const inInput = tag === "INPUT" || tag === "TEXTAREA";

  if (e.key === "Escape") {
    void onEscape();
    return;
  }

  if (e.key === "ArrowDown") {
    e.preventDefault();
    move(1);
    return;
  }
  if (e.key === "ArrowUp") {
    e.preventDefault();
    move(-1);
    return;
  }
  if (e.key === "PageDown") {
    e.preventDefault();
    pageMove(1);
    return;
  }
  if (e.key === "PageUp") {
    e.preventDefault();
    pageMove(-1);
    return;
  }

  // SQL 结果的合成行没有 id（DLL 后端），但路径是真的，打开/定位/改名照样能用
  const row = sqlMode.value ? sqlRow.value : rowAt(selected.value);
  if (!row) return;

  if (e.key === "Enter" && !e.shiftKey) {
    if (inInput && !e.ctrlKey) return; // 输入框里的 Enter 交给搜索框处理
    e.preventDefault();
    if (e.ctrlKey) void revealRow(row);
    else void openRow(row);
    return;
  }
  if (e.key === "F2") {
    e.preventDefault();
    void renameRow(row);
    return;
  }
  if (e.key === "Delete") {
    if (inInput) return;
    e.preventDefault();
    if (pickedCount.value > 1) void deletePicked();
    else void deleteRow(row);
    return;
  }
  if (e.ctrlKey && (e.key === "c" || e.key === "C")) {
    if (inInput && (window.getSelection()?.toString() || "").length) return;
    e.preventDefault();
    if (pickedCount.value > 1) void copyPickedPaths();
    else void copyPath(row);
    return;
  }
  if (e.key === "F5" && !inInput) {
    e.preventDefault();
    void runSearch();
  }
}

/* ------------------------------ 配置 ------------------------------ */

async function saveSettings(next: AppConfig) {
  const themeChanged = next.theme !== cfg.value?.theme;
  cfg.value = next;
  applyTheme(next.theme);
  showSettings.value = false;
  try {
    await api.saveCfg(next);
    await api.alwaysOnTop(!!next.window.always_on_top);
    // 后端 config_save 已应用，这里再推一次保证立即生效；mock 环境没有该命令，忽略即可
    api.setHotkey(next.window.shortcut || "None", !!next.window.double_ctrl).catch(() => {});
    if (themeChanged) ElMessage.success("设置已保存");
  } catch (e) {
    ElMessage.error(String(e));
  }
  void refreshStats();
}

function openSettings(tab?: string) {
  settingsTab.value = typeof tab === "string" ? tab : "search";
  showSettings.value = true;
}

async function togglePreview() {
  if (!cfg.value) return;
  cfg.value.view.preview = !cfg.value.view.preview;
  await api.saveCfg(cfg.value);
  if (cfg.value.view.preview) {
    const r = rowAt(selected.value);
    if (r && r.id > 0) previewRow.value = r;
  }
}

async function setView(v: string) {
  if (!cfg.value) return;
  cfg.value.view.mode = v;
  await api.saveCfg(cfg.value);
}

async function toggleMax() {
  maximized.value = await api.maximize();
}

/* ------------------------------ 生命周期 ------------------------------ */

onMounted(() => {
  void boot();
  void loadNlStatus();
  window.addEventListener("keydown", onKeydown);
  window.addEventListener("mousedown", (e) => {
    if (ctx.value) {
      const el = document.getElementById("ctxmenu");
      if (!el || !el.contains(e.target as Node)) ctx.value = null;
    }
  });
});

onBeforeUnmount(() => {
  unlisteners.forEach((u) => u());
  if (pollTimer) window.clearInterval(pollTimer);
  window.removeEventListener("keydown", onKeydown);
});

defineExpose({ openSettings });
</script>

<template>
  <div class="app">
    <TitleBar
      :pinned="!!cfg?.window.always_on_top"
      :maximized="maximized"
      :warn="!admin && !ready"
      :engine-version="info.engineVersion || ''"
      @pin="
        cfg &&
          (cfg.window.always_on_top = !cfg.window.always_on_top,
          api.alwaysOnTop(cfg.window.always_on_top),
          api.saveCfg(cfg))
      "
      @minimize="api.minimize()"
      @maximize="toggleMax"
      @close="api.hide()"
    />

    <SearchBar
      ref="searchBar"
      :model-value="keyword"
      :mode="mode"
      :modes="allModes"
      :tags="tags"
      :searching="searching"
      :scope="scope"
      @update:model-value="onInput"
      @mode-change="changeMode"
      @clear-tag="clearFacet"
      @clear-tags="clearAllFacets"
      @open-settings="openSettings()"
      @submit="onSubmit"
      @escape="onEscape()"
      @pick-scope="pickScope"
      @clear-scope="((scope = null), runSearch())"
      @focus="kwFocused = true"
      @interact="interacted = true"
      @blur="kwFocused = false"
    />

    <!-- 自然语言：把模型/引擎的理解结果摊开给用户看，避免「它到底搜了什么」的猜疑 -->
    <div v-if="mode.type === 3 && nlParsed" class="nl-bar">
      <span class="nl-lead">已理解为</span>
      <span v-for="n in nlParsed.notes" :key="n" class="nl-chip">{{ n }}</span>
      <span v-if="nlParsed.score < 0.45" class="nl-warn">
        没识别出条件，试试「今天下载的 pdf」「上个月改过的 word」
      </span>
    </div>

    <!-- 历史 / 提示 -->
    <Transition name="fade">
      <div
        v-if="showHistory"
        class="history pop-card"
        :style="{ top: tags.length ? '124px' : '88px' }"
      >
        <template v-if="history.length">
          <div class="hhead">
            最近搜索
            <button class="hclear" @click="history = []; api.historyClear()">清空</button>
          </div>
          <div class="hlist">
            <button
              v-for="(h, i) in history.slice(0, 9)"
              :key="h + i"
              class="hitem"
              @mousedown.prevent="((keyword = h), (showHistory = false), onSubmit())"
            >
              <Icon name="clock" :size="13" />
              <span class="htxt">{{ h }}</span>
              <span
                class="hdel"
                title="移除"
                @mousedown.prevent.stop="
                  history = history.filter((x) => x !== h); api.historyRemove(h)
                "
              >
                <Icon name="x" :size="11" />
              </span>
            </button>
          </div>
        </template>
        <div v-else class="tips">
          <div class="tip-title">开始输入即可搜索</div>
          <div class="tip-grid">
            <span><b>wd</b> · 拼音首拼命中「文档」</span>
            <span><b>*.pdf</b> · 通配符</span>
            <span><b>报表 2026</b> · 空格=且</span>
            <span><b>*.doc | *.docx</b> · 竖线=或</span>
            <span><b>\Windows</b> · 路径匹配</span>
            <span><b>Ctrl+Enter</b> · 定位文件</span>
          </div>
        </div>
      </div>
    </Transition>

    <!-- 多重搜索链（来自 ms= 参数）：输入框里继续输入就在链尾结果内过滤 -->
    <div v-if="chain && chain.length" class="chain-bar">
      <span class="cl">多重搜索链</span>
      <span v-for="(s, i) in chain" :key="i" class="cseg">
        <em>{{ i + 1 }}</em>{{ s.q }}<i v-if="s.mode">· {{ s.mode }}</i>
      </span>
      <button class="cclr" @click="clearChain()">清除</button>
    </div>

    <div class="body">
      <SideBar
        :places="home?.places || []"
        :types="filters"
        :drives="home?.drives || []"
        :selected="facets"
        @toggle="onFacetToggle"
        @clear="clearFacet"
        @clear-all="clearAllFacets"
        @reset="goHome"
      />

    <main class="main" :class="{ 'ov-dim': showOverlay && needAdmin }">
      <div class="results">
        <nav v-if="scope" class="crumbs" aria-label="当前文件夹">
          <button class="cb-home" title="回到首页" @click="goHome()">
            <Icon name="location" :size="13" />
          </button>
          <template v-for="(c, i) in crumbs" :key="c.path">
            <Icon name="chevron" :size="12" class="cb-sep" />
            <button
              class="cb-item"
              :class="{ on: i === crumbs.length - 1 }"
              :disabled="i === crumbs.length - 1"
              :title="c.path"
              @click="enterFolder(c.path)"
            >
              {{ c.name }}
            </button>
          </template>
          <span class="cb-count">{{ total }} 项</span>
        </nav>
        <HomeView
          v-if="browsingHome"
          :home="home"
          :loading="homeLoading"
          @open="homeOpen"
          @reveal="revealRowPath"
          @use-place="usePlace"
          @pick-drive="pickDrive"
          @refresh="loadHome(true)"
        />
        <div v-else-if="sqlMode && !sqlData" class="sql-empty">
          <div class="se-head">
            <span class="badge">SQL</span>
            <span class="se-title">{{ errorMsg ? "这条 SQL 没能执行" : "在搜索框里写一条 SQL" }}</span>
          </div>
          <div class="se-err" v-if="errorMsg">{{ errorMsg }}</div>
          <div class="se-hint">
            表名固定为 <b>alltable</b>；字段：<b>Path FName Ext Size Mtime Ctime Atime Dir Drive Type</b>，
            也认中文（路径 / 文件名 / 后缀 / 大小 / 修改时间…）。
          </div>
          <div class="se-samples">
            <button
              v-for="ex in SQL_SAMPLES"
              :key="ex"
              class="se-sample"
              @click="useSqlSample(ex)"
            >
              {{ ex }}
            </button>
          </div>
          <div class="se-note">
            大小与时间不存索引，只在筛出候选后按需读取；先用名称 / 后缀 / 路径缩小范围会快很多。
            排序除了字段名，也可以直接写 <b>ORDER BY COUNT(*) DESC</b> 或 SELECT 里起的别名。
          </div>
        </div>
        <SqlTable
          v-else-if="sqlMode && sqlData"
          :data="sqlData"
          :selected="sqlSel"
          @select="pickSqlRow"
          @open="openSqlRow"
          @menu="onSqlMenu"
        />
        <ResultGrid
          v-else-if="view === 'grid'"
          :tile-w="tileW"
          :selected="selected"
          :picked="picked"
          :searching="searching"
          @select="onPick"
          @open="openRow"
          @menu="onContextMenu"
        />
        <ResultList
          v-else
          :row-h="rowH"
          :dense="view === 'dense'"
          :selected="selected"
          :picked="picked"
          :sort-field="cfg?.sort.field || ''"
          :asc="!!cfg?.sort.asc"
          :searching="searching"
          :ready="ready"
          :empty-hint="emptyHint"
          @select="onPick"
          @open="openRow"
          @menu="onContextMenu"
          @sort="changeSort"
        />
      </div>

      <PreviewPane
        v-if="previewVisible && !browsingHome"
        :row="sqlMode ? sqlRow : previewRow"
        @open="openRow"
        @reveal="revealRow"
        @props="propsRow"
        @copy="copyPath"
        @alias="aliasRow"
        @rate="rate"
      />
    </main>
    </div>

    <StatusBar
      :home="browsingHome"
      :count="total"
      :elapsed="elapsed"
      :rerank-ms="rerankMs"
      :picked-count="sqlMode ? 0 : pickedCount"
      :sql-text="sqlStatText"
      :searching="searching"
      :error="errorMsg"
      :stats="stats"
      :sort-field="cfg?.sort.field || '相关度'"
      :asc="!!cfg?.sort.asc"
      :sort-fields="sortFields"
      :view="view"
      :preview="previewVisible"
      @sort="changeSort"
      @view="setView"
      @toggle-preview="togglePreview"
      @open-settings="openSettings"
    />

    <!-- 首次建库遮罩 -->
    <Transition name="fade">
      <div v-if="showOverlay" class="overlay" :class="{ soft: needAdmin }">
        <div class="ov-card">
          <button
            v-if="needAdmin"
            class="ov-x"
            title="先不用建库"
            @click="showOverlayClosed = true"
          >
            <Icon name="x" :size="13" />
          </button>
          <div class="ov-spin">
            <span v-if="!needAdmin" class="ring" />
            <Icon :name="needAdmin ? 'shield' : 'database'" :size="22" />
          </div>
          <div class="ov-title">{{ needAdmin ? "建库这一步需要管理员权限" : "正在建立全盘索引" }}</div>
          <div class="ov-sub">
            <template v-if="needAdmin">
              只有建立索引要读 NTFS 主文件表（MFT），需要管理员权限。搜索、预览、
              打开/定位/复制、托盘与快捷键都不需要；已有的索引库在普通权限下也能照常查询。
            </template>
            <template v-else>
              通过解析 NTFS MFT 快速枚举文件，索引完成后输入即搜。
              <template v-if="stats?.progress > 0">
                当前分区进度 {{ stats.progress.toFixed(1) }}%
              </template>
            </template>
          </div>
          <div v-if="!needAdmin" class="ov-bar">
            <i :style="{ width: Math.min(100, Math.max(2, stats?.progress || 2)) + '%' }" />
          </div>
          <div v-if="!needAdmin" class="ov-meta num">
            已索引 {{ formatCount(stats?.file_count || 0) }} 条
            <template v-if="stats?.memory_bytes">
              · 内存 {{ formatSize(stats.memory_bytes) }}
            </template>
          </div>
          <div class="ov-actions">
            <template v-if="needAdmin">
              <button class="primary" @click="api.elevate()">以管理员身份建立索引</button>
              <button class="ghost" @click="openSettings('index')">改用其它方式</button>
            </template>
            <template v-else>
              <button class="ghost" @click="showOverlayClosed = true">后台继续</button>
              <button class="ghost" @click="openSettings('index')">选择磁盘</button>
            </template>
          </div>
        </div>
      </div>
    </Transition>

    <div v-if="bootNotice && !showOverlay" class="notice">
      <Icon name="info" :size="13" />
      <span>{{ bootNotice }}</span>
      <button @click="bootNotice = ''"><Icon name="x" :size="11" /></button>
    </div>

    <!-- 右键菜单 -->
    <div
      v-if="ctx"
      id="ctxmenu"
      class="ctx pop-card"
      :style="{ left: ctx.x + 'px', top: ctx.y + 'px' }"
      @click.stop
    >
      <template v-if="pickedCount > 1">
        <div class="ch">已选择 {{ pickedCount }} 项</div>
        <button class="ci" @click="copyPickedFiles(); ctx = null">
          <Icon name="doc" :size="13" />复制 {{ pickedCount }} 个文件
        </button>
        <button class="ci" @click="copyPickedPaths(); ctx = null">
          <Icon name="copy" :size="13" />复制 {{ pickedCount }} 个路径<span class="sc">Ctrl+C</span>
        </button>
        <div class="csep" />
        <button class="ci danger" @click="deletePicked(); ctx = null">
          <Icon name="trash" :size="13" />删除 {{ pickedCount }} 个文件到回收站
          <span class="sc">Del</span>
        </button>
      </template>
      <template v-else>
        <button class="ci" @click="openRow(ctx.row); ctx = null">
          <Icon name="external" :size="13" />打开<span class="sc">Enter</span>
        </button>
        <button class="ci" @click="revealRow(ctx.row); ctx = null">
          <Icon name="location" :size="13" />在资源管理器中定位<span class="sc">Ctrl+↵</span>
        </button>
        <div class="csep" />
        <button class="ci" @click="copyPath(ctx.row); ctx = null">
          <Icon name="copy" :size="13" />复制路径<span class="sc">Ctrl+C</span>
        </button>
        <button class="ci" @click="copyFile(ctx.row); ctx = null">
          <Icon name="doc" :size="13" />复制文件
        </button>
        <div class="csep" />
        <button class="ci" @click="renameRow(ctx.row); ctx = null">
          <Icon name="edit" :size="13" />重命名<span class="sc">F2</span>
        </button>
        <button class="ci" @click="aliasRow(ctx.row); ctx = null">
          <Icon name="tag" :size="13" />设置别名
        </button>
        <button class="ci" @click="propsRow(ctx.row); ctx = null">
          <Icon name="info" :size="13" />属性
        </button>
        <div class="csep" />
        <button class="ci danger" @click="deleteRow(ctx.row); ctx = null">
          <Icon name="trash" :size="13" />删除到回收站<span class="sc">Del</span>
        </button>
      </template>
    </div>

    <SettingsDialog
      v-if="cfg"
      :visible="showSettings"
      :config="cfg"
      :stats="stats"
      :admin="admin"
      :initial-tab="settingsTab"
      :app-info="info"
      @close="showSettings = false"
      @save="saveSettings"
      @theme="applyTheme"
      @refresh-stats="refreshStats"
      @plugins-changed="loadNlStatus"
    />
  </div>
</template>

<style scoped>
.app {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: var(--bg);
  background-image: var(--bg-grad);
  background-attachment: fixed;
  position: relative;
  overflow: hidden;
}

.chain-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin: 0 12px 8px;
  padding: 6px 10px;
  border: 1px dashed color-mix(in srgb, var(--accent) 40%, var(--border));
  border-radius: var(--radius-md, 10px);
  background: color-mix(in srgb, var(--accent) 6%, transparent);
  font-size: 12px;
  color: var(--text-2);
}

/* 自然语言理解结果：一行小标签，告诉用户「这句话被译成了什么条件」 */
.nl-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin: 0 12px 8px;
  padding: 5px 10px;
  border-radius: var(--radius-md, 10px);
  background: color-mix(in srgb, var(--accent) 5%, transparent);
  font-size: 12px;
  color: var(--text-2);
}
.nl-bar .nl-lead {
  color: var(--text-3, var(--text-2));
}
.nl-bar .nl-chip {
  padding: 1px 8px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--text-1);
  white-space: nowrap;
}
.nl-bar .nl-warn {
  color: var(--warn, #c98a1b);
}
.chain-bar .cl {
  font-weight: 600;
  color: var(--text-1);
  margin-right: 2px;
}
.chain-bar .cseg {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 320px;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  padding: 2px 8px;
  border-radius: 999px;
  background: var(--panel);
  border: 1px solid var(--border);
}
.chain-bar .cseg em {
  font-style: normal;
  font-size: 10px;
  min-width: 14px;
  height: 14px;
  line-height: 14px;
  text-align: center;
  border-radius: 50%;
  background: var(--accent);
  color: #fff;
}
.chain-bar .cseg i {
  font-style: normal;
  color: var(--text-3, var(--text-2));
  font-size: 11px;
}
.chain-bar .cclr {
  margin-left: auto;
  border: none;
  background: transparent;
  color: var(--accent);
  cursor: pointer;
  font-size: 12px;
  padding: 2px 6px;
}
.chain-bar .cclr:hover {
  text-decoration: underline;
}

.body {
  flex: 1;
  min-height: 0;
  display: flex;
  gap: 10px;
  padding: 0 12px 10px;
}

.main {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  background: var(--panel);
  margin: 0;
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  overflow: hidden;
  box-shadow: var(--shadow-sm);
}

.results {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--panel);
}

/* 目录面包屑：每级都可点回去，过长时横向滚动而不是把布局撑开 */
.crumbs {
  flex: none;
  display: flex;
  align-items: center;
  gap: 1px;
  padding: 7px 12px 6px;
  border-bottom: 1px solid var(--border);
  overflow-x: auto;
  white-space: nowrap;
  font-size: 12px;
  scrollbar-width: none;
}
.crumbs::-webkit-scrollbar {
  display: none;
}
.crumbs .cb-home {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex: none;
  width: 24px;
  height: 24px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
}
.crumbs .cb-home:hover {
  background: var(--hover);
  color: var(--text-1);
}
.crumbs .cb-sep {
  flex: none;
  color: var(--text-3);
  transform: rotate(-90deg);
  opacity: 0.75;
}
.crumbs .cb-item {
  flex: none;
  max-width: 180px;
  overflow: hidden;
  text-overflow: ellipsis;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-2);
  font-size: 12px;
  padding: 3px 7px;
  cursor: pointer;
}
.crumbs .cb-item:hover:not(:disabled) {
  background: var(--hover);
  color: var(--text-1);
}
.crumbs .cb-item.on {
  color: var(--text-1);
  font-weight: 600;
  cursor: default;
}
.crumbs .cb-count {
  margin-left: auto;
  flex: none;
  padding-left: 12px;
  color: var(--text-3);
  font-size: 11px;
}

.sql-empty {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 26px 30px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.se-head {
  display: flex;
  align-items: center;
  gap: 9px;
}
.badge {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.06em;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--accent-soft);
  color: var(--accent);
}
.se-title {
  font-size: 14px;
  font-weight: 650;
}
.se-err {
  padding: 9px 11px;
  border-radius: 8px;
  background: color-mix(in srgb, var(--danger) 9%, transparent);
  border: 1px solid color-mix(in srgb, var(--danger) 26%, transparent);
  color: var(--danger);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  font-family: var(--font-mono);
}
.se-hint,
.se-note {
  font-size: 12px;
  color: var(--text-3);
  line-height: 1.75;
  max-width: 760px;
}
.se-hint b {
  color: var(--text-2);
  font-family: var(--font-mono);
  font-weight: 600;
  background: var(--panel-3);
  padding: 1px 4px;
  border-radius: 4px;
}
.se-samples {
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-width: 820px;
}
.se-sample {
  text-align: left;
  padding: 8px 11px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.5;
  word-break: break-all;
}
.se-sample:hover {
  border-color: var(--accent);
  color: var(--accent);
  background: var(--accent-soft);
}
.se-note {
  margin-top: auto;
  padding-top: 8px;
  font-size: 11.5px;
}

.history {
  position: absolute;
  left: 12px;
  right: 12px;
  top: 88px;
  z-index: 25;
  padding: 6px;
  max-height: 320px;
  overflow: auto;
}

.hhead {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 11px;
  font-weight: 600;
  color: var(--text-3);
  letter-spacing: 0.05em;
  padding: 6px 8px;
}
.hclear {
  font-size: 11px;
  color: var(--text-3);
}
.hclear:hover {
  color: var(--danger);
}

.hitem {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 8px;
  border-radius: 7px;
  color: var(--text-2);
  font-size: 12.5px;
  text-align: left;
}
.hitem:hover {
  background: var(--hover);
  color: var(--text);
}
.htxt {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.hdel {
  opacity: 0;
  color: var(--text-3);
  width: 18px;
  height: 18px;
  display: grid;
  place-items: center;
  border-radius: 4px;
}
.hitem:hover .hdel {
  opacity: 1;
}
.hdel:hover {
  background: var(--hover-strong);
}

.tips {
  padding: 8px 10px 10px;
}
.tip-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--text-2);
  margin-bottom: 8px;
}
.tip-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 6px 14px;
  font-size: 11.5px;
  color: var(--text-3);
}
.tip-grid b {
  color: var(--accent);
  font-family: var(--font-mono);
  font-weight: 600;
  background: var(--accent-soft);
  padding: 1px 4px;
  border-radius: 4px;
}

.overlay {
  position: absolute;
  inset: var(--title-h) 0 30px 0;
  background: color-mix(in srgb, var(--bg) 78%, transparent);
  backdrop-filter: blur(6px);
  display: grid;
  place-items: center;
  z-index: 40;
}
/* 「还没建库」不是错误状态，界面其余部分照样能用，所以不压暗、不挡操作 */
.overlay.soft {
  background: transparent;
  backdrop-filter: none;
  pointer-events: none;
}
.overlay.soft .ov-card {
  pointer-events: auto;
}
.main.ov-dim :deep(.empty),
.main.ov-dim :deep(.placeholder) {
  opacity: 0;
  pointer-events: none;
}

.ov-card {
  position: relative;
  width: 380px;
  padding: 26px 26px 20px;
  border-radius: var(--radius-lg);
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: var(--shadow-lg);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  text-align: center;
}

.ov-x {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 22px;
  height: 22px;
  display: grid;
  place-items: center;
  border-radius: 6px;
  color: var(--text-3);
}
.ov-x:hover {
  background: var(--hover);
  color: var(--text);
}

.ov-spin {
  position: relative;
  width: 54px;
  height: 54px;
  display: grid;
  place-items: center;
  color: var(--accent);
}
.ring {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  border: 3px solid var(--accent-soft-2);
  border-top-color: var(--accent);
  animation: spin 1s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.ov-title {
  font-size: 15px;
  font-weight: 700;
}
.ov-sub {
  font-size: 12px;
  color: var(--text-3);
  line-height: 1.6;
}
.ov-bar {
  width: 100%;
  height: 5px;
  border-radius: 3px;
  background: var(--panel-3);
  overflow: hidden;
  margin-top: 4px;
}
.ov-bar i {
  display: block;
  height: 100%;
  background: linear-gradient(90deg, var(--accent), var(--accent-2));
  transition: width 0.4s;
}
.ov-meta {
  font-size: 11.5px;
  color: var(--text-3);
}
.ov-actions {
  display: flex;
  gap: 8px;
  margin-top: 6px;
}

.notice {
  position: absolute;
  left: 50%;
  transform: translateX(-50%);
  bottom: 40px;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px 7px 12px;
  border-radius: 9px;
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: var(--shadow);
  font-size: 12px;
  color: var(--text-2);
  z-index: 35;
  max-width: 80%;
}
.notice button {
  color: var(--text-3);
  width: 18px;
  height: 18px;
  display: grid;
  place-items: center;
  border-radius: 4px;
}
.notice button:hover {
  background: var(--hover);
}

.ctx {
  position: fixed;
  z-index: 90;
  width: 208px;
  padding: 5px;
}
.ci {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 9px;
  border-radius: 6px;
  font-size: 12.5px;
  color: var(--text-2);
  text-align: left;
}
.ci:hover {
  background: var(--hover);
  color: var(--text);
}
.ci.danger {
  color: var(--danger);
}
.ci.danger:hover {
  background: rgba(229, 72, 77, 0.1);
}
.sc {
  margin-left: auto;
  font-size: 10px;
  color: var(--text-3);
  font-family: var(--font-mono);
}
.csep {
  height: 1px;
  background: var(--border);
  margin: 4px 2px;
}
.ch {
  padding: 6px 9px 7px;
  font-size: 11px;
  font-weight: 600;
  color: var(--text-3);
  letter-spacing: 0.03em;
}

.ghost {
  height: 30px;
  padding: 0 12px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  font-size: 12.5px;
}
.ghost:hover {
  border-color: var(--accent);
  color: var(--accent);
}

.primary {
  height: 30px;
  padding: 0 14px;
  border-radius: 8px;
  background: var(--accent);
  color: #fff;
  font-size: 12.5px;
  font-weight: 600;
  box-shadow: 0 2px 8px var(--accent-soft-2);
}
.primary:hover {
  filter: brightness(1.08);
}
</style>
