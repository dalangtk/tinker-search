import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AppConfig,
  HomeSnapshot,
  IndexStats,
  MarketEntry,
  MatchSettings,
  NlQuery,
  NlStatus,
  Place,
  PluginView,
  Row,
  SearchStart,
} from "./types";
import { mockInvoke, mockListen } from "./mock";

/** 界面预览模式：浏览器打开 ?mock=1 时用假数据渲染，不依赖引擎 */
export const IS_MOCK =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).has("mock");

const MOCK = IS_MOCK;

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  MOCK ? (mockInvoke(cmd, args) as Promise<T>) : invoke<T>(cmd, args);

/** 订阅引擎事件；返回取消订阅函数 */
export function onEvent(name: string, cb: (payload: any) => void): () => void {
  if (MOCK) return mockListen(name, (e) => cb(e.payload));
  let un: (() => void) | null = null;
  let disposed = false;
  listen(name, (e) => cb(e.payload))
    .then((u) => {
      if (disposed) u();
      else un = u;
    })
    // 订阅失败（例如 capabilities 里少了 core:event:allow-listen）不能静默吞掉，
    // 否则界面会安静地永远收不到引擎事件
    .catch((err) => console.error(`[event] 订阅 ${name} 失败：${err}`));
  return () => {
    disposed = true;
    un?.();
  };
}

export const api = {
  info: () => call<any>("app_info"),
  engineReady: () => call<boolean>("engine_ready"),

  /** places / drives 来自左侧分类栏；types 走 setFilter（两条后端都认） */
  search: (
    keyword: string,
    mode: number,
    scope?: string | null,
    places?: string[],
    drives?: string[]
  ) =>
    call<SearchStart>("search", {
      keyword,
      mode,
      scope: scope ?? null,
      places: places ?? [],
      drives: drives ?? [],
    }),
  cancelSearch: (fingerprint: number) => call<void>("cancel_search", { fingerprint }),
  rows: (start: number, count: number) => call<Row[]>("get_rows", { start, count }),
  row: (id: number) => call<Row>("get_row", { id }),
  icons: (ids: number[], size: number) =>
    call<{ id: number; png: string }[]>("get_icons", { ids, size }),
  previewIcon: (id: number, size: number) => call<string | null>("get_preview_icon", { id, size }),

  filters: () => call<any[]>("get_filters"),
  sortFields: () => call<string[]>("get_sort_fields"),
  setSort: (field: string, asc: boolean) => call<void>("set_sort", { field, asc }),
  setFilter: (names: string[]) => call<void>("set_filter", { names }),
  setMatch: (search: MatchSettings) => call<void>("set_match", { search }),
  matchJson: () => call<string>("match_settings_json"),
  addBoost: (kind: string, value: string, delta: number) =>
    call<void>("add_boost", { kind, value, delta }),
  sqlResult: (start: number, max: number) => call<any>("sql_result", { start, max }),
  prompt: (kind: number) => call<string>("engine_prompt", { kind }),

  indexStatus: () => call<IndexStats>("index_status"),
  drives: () => call<any[]>("list_drives"),
  homeSnapshot: () => call<HomeSnapshot>("home_snapshot"),
  knownPlaces: () => call<Place[]>("known_places"),
  startScan: (drives: string[]) => call<void>("start_scan", { drives }),
  stopScan: () => call<void>("stop_scan"),
  saveDb: () => call<string>("save_db"),
  excluded: () => call<string[]>("excluded_dirs"),
  addExcluded: (path: string) => call<void>("add_excluded", { path }),
  removeExcluded: (path: string) => call<void>("remove_excluded", { path }),
  removeIndexPath: (path: string) => call<number>("remove_index_path", { path }),
  perf: () => call<any>("perf_text"),
  lastError: () => call<string>("engine_last_error"),
  setAlias: (id: number, alias: string | null) => call<void>("set_alias", { id, alias }),
  boostRating: (id: number, delta: number) => call<number>("boost_rating", { id, delta }),

  openFile: (path: string) => call<void>("open_file", { path }),
  /** 解析 .lnk 目标；不是快捷方式或解析失败时为 null */
  resolveLink: (path: string) =>
    call<{ target: string; isDir: boolean; indexed: boolean } | null>("resolve_link", {
      path,
    }),
  reveal: (path: string) => call<void>("reveal_file", { path }),
  props: (path: string) => call<void>("file_properties", { path }),
  remove: (paths: string[]) => call<void>("delete_files", { paths }),
  rename: (path: string, newName: string) => call<string>("rename_file", { path, newName }),
  copyPaths: (paths: string[]) => call<void>("copy_paths", { paths }),
  copyText: (text: string) => call<void>("copy_text", { text }),
  openDir: (kind: string) => call<void>("open_dir", { kind }),
  readText: (path: string, max: number) => call<any>("read_text_file", { path, max }),

  cfg: () => call<AppConfig>("config_get"),
  saveCfg: (cfg: AppConfig) => call<void>("config_save", { cfg }),
  historyAdd: (keyword: string) => call<string[]>("history_add", { keyword }),
  historyRemove: (keyword: string) => call<string[]>("history_remove", { keyword }),
  historyClear: () => call<string[]>("history_clear"),

  alwaysOnTop: (on: boolean) => call<void>("set_always_on_top", { on }),
  toggleHotkey: (on: boolean) => call<void>("toggle_hotkey", { on }),
  setHotkey: (spec: string, enabled: boolean) =>
    call<boolean>("set_hotkey", { spec, enabled }),
  hide: () => call<void>("hide_window"),
  minimize: () => call<void>("minimize_window"),
  maximize: () => call<boolean>("toggle_maximize"),
  exit: () => call<void>("app_exit"),
  elevate: () => call<void>("elevate"),
  isAdmin: () => call<boolean>("is_admin"),
  registerProtocol: (enable: boolean) => call<void>("register_url_protocol", { enable }),
  takePendingUrl: () => call<string | null>("take_pending_url"),
  protocolRegistered: () => call<boolean>("url_protocol_registered"),
  hotkeyHookOk: () => call<boolean>("hotkey_hook_ok"),

  // ---- 自然语言搜索（需要插件）----
  nlStatus: () => call<NlStatus>("nl_status"),
  nlParse: (text: string) => call<NlQuery>("nl_parse", { text }),
  nlSearch: (text: string, scope?: string | null, places?: string[], drives?: string[]) =>
    call<{ fingerprint: number; count: number; scanned: number; nl: NlQuery }>("nl_search", {
      text,
      scope: scope ?? null,
      places: places ?? [],
      drives: drives ?? [],
    }),

  // ---- 插件管理 ----
  plugins: () => call<PluginView[]>("plugins_list"),
  pluginMarket: (refresh = false) => call<MarketEntry[]>("plugin_market", { refresh }),
  pluginInstall: (id: string) => call<PluginView>("plugin_install", { id }),
  pluginInstallUrl: (url: string) => call<PluginView>("plugin_install_url", { url }),
  pluginUpdate: (id: string) => call<PluginView>("plugin_update", { id }),
  pluginSetEnabled: (id: string, enabled: boolean) =>
    call<void>("plugin_set_enabled", { id, enabled }),
  pluginUninstall: (id: string) => call<void>("plugin_uninstall", { id }),
  pluginSetEngine: (id: string, provider: string, model: string, endpoint: string) =>
    call<void>("plugin_set_engine", { id, provider, model, endpoint }),
  pluginMarketUrl: () => call<string>("plugin_market_url"),
  setPluginMarketUrl: (url: string) => call<void>("set_plugin_market_url", { url }),
};
