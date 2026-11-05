import { reactive, ref } from "vue";
import { api } from "../api";
import type { Row } from "../types";

const PAGE = 120;

export const total = ref(0);
export const elapsed = ref(0);
export const searching = ref(false);
export const errorMsg = ref("");
export const errorKind = ref("");
export const sqlMode = ref(false);
export const aggregate = ref(false);
export const fingerprint = ref(0);
/** 每次新搜索自增，用于丢弃过期分页响应 */
export const epoch = ref(0);

export const cache = reactive(new Map<number, Row>());
const loaded = new Set<number>();
const inflight = new Set<number>();

export function reset(count = 0, fp = 0) {
  cache.clear();
  loaded.clear();
  inflight.clear();
  total.value = count;
  fingerprint.value = fp;
  errorMsg.value = "";
  errorKind.value = "";
  epoch.value++;
  sqlMode.value = false;
  aggregate.value = false;
}

export function rowAt(i: number): Row | undefined {
  return cache.get(i);
}

/**
 * 清空分页缓存但不重置结果总数。
 *
 * 语义重排会让后端 `view` 的顺序整体变化，已缓存的行会串位，
 * 所以重排后必须把缓存作废重新按页取。
 */
export function resetPages() {
  cache.clear();
  loaded.clear();
  inflight.clear();
  epoch.value++;
}

/**
 * 按索引加载：多选/批量操作时选中的行可能还没进入分页缓存，
 * 这里按页补齐，返回后可安全用 rowAt() 取。
 */
export async function ensureIndices(indices: number[]): Promise<void> {
  const e = epoch.value;
  const pages = new Set<number>();
  for (const i of indices) {
    if (i < 0 || i >= total.value) continue;
    const p = Math.floor(i / PAGE);
    if (!loaded.has(p)) pages.add(p);
  }
  if (!pages.size) return;
  await Promise.all(
    Array.from(pages).map(async (p) => {
      try {
        const rows = await api.rows(p * PAGE, PAGE);
        if (e !== epoch.value) return;
        rows.forEach((r, i) => cache.set(p * PAGE + i, r));
        loaded.add(p);
        if (rows.length === 0) cache.set(p * PAGE, { id: -1 } as unknown as Row);
      } catch {
        // 索引切换期间可能失败，交给下次滚动重试
      }
    })
  );
}

export async function ensure(start: number, count: number): Promise<void> {
  if (count <= 0 || total.value <= 0) return;
  const e = epoch.value;
  const first = Math.max(0, Math.floor(start / PAGE));
  const last = Math.floor((start + count - 1) / PAGE);
  const jobs: Promise<void>[] = [];
  for (let p = first; p <= last; p++) {
    if (loaded.has(p) || inflight.has(p)) continue;
    if (p * PAGE >= total.value) continue;
    inflight.add(p);
    jobs.push(
      (async () => {
        try {
          const rows = await api.rows(p * PAGE, PAGE);
          if (e !== epoch.value) return;
          rows.forEach((r, i) => cache.set(p * PAGE + i, r));
          loaded.add(p);
          // 保证空页也标记为已加载：占位行不会被渲染（id<0 视为占位）
          if (rows.length === 0) cache.set(p * PAGE, { id: -1 } as unknown as Row);
        } catch (err) {
          // 索引切换/对象重建期间可能失败，留待下次滚动重试
        } finally {
          inflight.delete(p);
        }
      })()
    );
  }
  if (jobs.length) await Promise.all(jobs);
}

/* ------------------------------ 图标 ------------------------------ */

const icons = reactive(new Map<string, string>());
const requested = new Set<string>();
const pending = new Map<number, Set<number>>();
let timer: number | null = null;

export function iconFor(id: number, size: number): string {
  return icons.get(`${id}@${size}`) || "";
}

export function setIcon(id: number, size: number, url: string) {
  icons.set(`${id}@${size}`, url);
}

export function requestIcons(ids: number[], size: number) {
  let bucket = pending.get(size);
  if (!bucket) {
    bucket = new Set();
    pending.set(size, bucket);
  }
  let added = false;
  for (const id of ids) {
    const key = `${id}@${size}`;
    if (requested.has(key)) continue;
    requested.add(key);
    bucket.add(id);
    added = true;
  }
  if (!added) return;
  if (timer == null) {
    timer = window.setTimeout(flush, 40);
  }
}

async function flush() {
  timer = null;
  const batches: { size: number; ids: number[] }[] = [];
  for (const [size, set] of pending) {
    if (set.size === 0) continue;
    batches.push({ size, ids: Array.from(set).slice(0, 80) });
    const rest = Array.from(set).slice(80);
    set.clear();
    rest.forEach((i) => set.add(i));
  }
  for (const b of batches) {
    try {
      const res = await api.icons(b.ids, b.size);
      for (const item of res) icons.set(`${item.id}@${b.size}`, item.png);
    } catch {
      b.ids.forEach((id) => requested.delete(`${id}@${b.size}`));
    }
  }
  // 还有剩余批次继续排队
  for (const set of pending.values()) {
    if (set.size > 0) {
      if (timer == null) timer = window.setTimeout(flush, 30);
      return;
    }
  }
}

export function clearIcons() {
  icons.clear();
  requested.clear();
}
