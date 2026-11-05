<script setup lang="ts">
import { computed, reactive } from "vue";
import Icon from "./Icon.vue";
import type { DriveInfo, Facets, FilterCat, Place } from "../types";

const props = defineProps<{
  places: Place[];
  types: FilterCat[];
  drives: DriveInfo[];
  selected: Facets;
}>();

const emit = defineEmits<{
  (e: "toggle", kind: "places" | "types" | "drives", value: string): void;
  (e: "clear", kind: "places" | "types" | "drives"): void;
  (e: "clear-all"): void;
  (e: "reset"): void;
}>();

const OPEN_KEY = "ss.sidebar.open";
const open = reactive<Record<string, boolean>>({ places: true, types: true, drives: true });
try {
  const saved = JSON.parse(localStorage.getItem(OPEN_KEY) || "{}");
  for (const k of Object.keys(open)) {
    if (typeof saved[k] === "boolean") open[k] = saved[k];
  }
} catch {
  /* 配置坏了就用默认展开 */
}

function toggleOpen(k: string) {
  open[k] = !open[k];
  try {
    localStorage.setItem(OPEN_KEY, JSON.stringify({ ...open }));
  } catch {
    /* 无痕模式忽略 */
  }
}

/** 文件类型 → 图标 */
const TYPE_ICON: Record<string, string> = {
  文件夹: "folder",
  快捷方式: "external",
  程序: "app",
  音频: "music",
  视频: "video",
  图片: "image",
  文档: "doc",
  办公: "doc",
  压缩包: "archive",
  代码: "code",
};

const PLACE_ICON: Record<string, string> = {
  home: "home",
  desktop: "desktop",
  downloads: "download",
  documents: "doc",
  pictures: "image",
  music: "music",
  videos: "video",
};

const typeList = computed(() => props.types.filter((t) => t.名称 !== "全部"));
const hasAny = computed(
  () =>
    props.selected.places.length > 0 ||
    props.selected.types.length > 0 ||
    props.selected.drives.length > 0
);

function isOn(kind: "places" | "types" | "drives", v: string) {
  return props.selected[kind].includes(v);
}

function count(kind: "places" | "types" | "drives") {
  return props.selected[kind].length;
}

function gb(n: number) {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || i < 2 ? Math.round(v) : v.toFixed(1)} ${u[i]}`;
}

function driveMeta(d: DriveInfo) {
  // 没收录进索引的盘，收益信息不如「未索引」有用——点它只会得到 0 条
  if (d.indexed === false) return "未索引";
  return `剩余 ${gb(d.free)}`;
}

function driveTip(d: DriveInfo) {
  const head = `${d.letter} ${d.name || "本地磁盘"} · 共 ${gb(d.total)}`;
  return d.indexed === false ? `${head}\n这个盘还没建索引，选中后不会有结果` : head;
}
</script>

<template>
  <aside class="side">
    <div class="side-head">
      <span class="sh-title">分类</span>
      <button v-if="hasAny" class="sh-clear" @click="emit('clear-all')">清除</button>
    </div>

    <div class="scroll">
      <!-- 常用位置 -->
      <section class="grp">
        <button class="gh" :class="{ open: open.places }" @click="toggleOpen('places')">
          <Icon name="chevron" :size="11" class="caret" />
          <Icon name="location" :size="13" class="gico" />
          <span class="gt">常用位置</span>
          <span v-if="count('places')" class="gn">{{ count("places") }}</span>
        </button>
        <div v-show="open.places" class="items">
          <button
            v-for="p in props.places"
            :key="p.key"
            class="it"
            :class="{ on: isOn('places', p.path) }"
            :title="p.path"
            @click="emit('toggle', 'places', p.path)"
          >
            <Icon :name="PLACE_ICON[p.key] || 'folder'" :size="13" class="iico" />
            <span class="inm">{{ p.name }}</span>
            <Icon v-if="isOn('places', p.path)" name="check" :size="11" class="ick" />
          </button>
          <div v-if="!props.places.length" class="empty-hint">读取常用位置…</div>
        </div>
      </section>

      <!-- 文件类型 -->
      <section class="grp">
        <button class="gh" :class="{ open: open.types }" @click="toggleOpen('types')">
          <Icon name="chevron" :size="11" class="caret" />
          <Icon name="filter" :size="13" class="gico" />
          <span class="gt">文件类型</span>
          <span v-if="count('types')" class="gn">{{ count("types") }}</span>
        </button>
        <div v-show="open.types" class="items">
          <button
            v-for="t in typeList"
            :key="t.名称"
            class="it"
            :class="{ on: isOn('types', t.名称) }"
            :title="t.后缀 || t.名称"
            @click="emit('toggle', 'types', t.名称)"
          >
            <Icon :name="TYPE_ICON[t.名称] || 'doc'" :size="13" class="iico" />
            <span class="inm">{{ t.名称 }}</span>
            <Icon v-if="isOn('types', t.名称)" name="check" :size="11" class="ick" />
          </button>
        </div>
      </section>

      <!-- 磁盘 -->
      <section class="grp">
        <button class="gh" :class="{ open: open.drives }" @click="toggleOpen('drives')">
          <Icon name="chevron" :size="11" class="caret" />
          <Icon name="drive" :size="13" class="gico" />
          <span class="gt">磁盘</span>
          <span v-if="count('drives')" class="gn">{{ count("drives") }}</span>
        </button>
        <div v-show="open.drives" class="items">
          <button
            v-for="d in props.drives"
            :key="d.letter"
            class="it drive"
            :class="{ on: isOn('drives', d.letter), noindex: d.indexed === false }"
            :title="driveTip(d)"
            @click="emit('toggle', 'drives', d.letter)"
          >
            <Icon name="drive" :size="13" class="iico" />
            <span class="inm">
              {{ d.letter }}
              <em>{{ d.name || (d.kind === 2 ? "可移动" : d.kind === 5 ? "光驱" : "") }}</em>
            </span>
            <span class="imeta">{{ driveMeta(d) }}</span>
            <Icon v-if="isOn('drives', d.letter)" name="check" :size="11" class="ick" />
            <i class="dbar" :class="{ warn: d.percent > 88 }">
              <b :style="{ width: Math.min(100, d.percent) + '%' }" />
            </i>
          </button>
        </div>
      </section>
    </div>

    <button class="side-foot" :disabled="!hasAny" @click="emit('reset')">
      <Icon name="refresh" :size="12" />
      回到首页
    </button>
  </aside>
</template>

<style scoped>
.side {
  width: 186px;
  flex: none;
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-sm);
  overflow: hidden;
}

.side-head {
  height: 30px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 10px 0 12px;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
}
.sh-title {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.08em;
  color: var(--text-3);
}
.sh-clear {
  font-size: 11px;
  color: var(--accent);
  padding: 1px 4px;
  border-radius: 4px;
}
.sh-clear:hover {
  background: var(--accent-soft);
}

.scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 4px;
  scrollbar-width: thin;
}

.grp + .grp {
  margin-top: 2px;
  padding-top: 2px;
  border-top: 1px solid var(--border);
}

.gh {
  width: 100%;
  height: 28px;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 6px 0 2px;
  border-radius: 6px;
  color: var(--text-2);
  font-size: 11.5px;
  font-weight: 600;
}
.gh:hover {
  background: var(--hover);
  color: var(--text);
}
.caret {
  transform: rotate(-90deg);
  transition: transform 0.16s ease;
  color: var(--text-3);
  flex: none;
}
.gh.open .caret {
  transform: rotate(0deg);
}
.gico {
  color: var(--text-3);
  flex: none;
}
.gt {
  flex: 1;
  text-align: left;
}
.gn {
  min-width: 15px;
  height: 15px;
  line-height: 15px;
  text-align: center;
  border-radius: 8px;
  background: var(--accent);
  color: #fff;
  font-size: 10px;
  font-weight: 600;
  padding: 0 4px;
}

.items {
  padding: 1px 0 3px;
}

.it {
  position: relative;
  width: 100%;
  min-height: 25px;
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 0 7px 0 20px;
  border-radius: 6px;
  color: var(--text-2);
  font-size: 12px;
  text-align: left;
}
.it:hover {
  background: var(--hover);
  color: var(--text);
}
.it.on {
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}
.iico {
  flex: none;
  color: var(--text-3);
}
.it.on .iico {
  color: var(--accent);
}
.inm {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.inm em {
  font-style: normal;
  color: var(--text-3);
  font-size: 10.5px;
  margin-left: 3px;
  font-weight: 400;
}
.imeta {
  flex: none;
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.it.on .imeta {
  color: var(--accent);
  opacity: 0.75;
}
.ick {
  flex: none;
  color: var(--accent);
}

.it.drive {
  padding-bottom: 4px;
}
/* 索引里没有这个盘：压低存在感，但仍可点（点了会得到 0 条，并在提示里说明原因） */
.it.noindex .iico,
.it.noindex .inm,
.it.noindex .imeta {
  opacity: 0.45;
}
.it.noindex .dbar {
  display: none;
}
.dbar {
  position: absolute;
  left: 20px;
  right: 7px;
  bottom: 2px;
  height: 2px;
  border-radius: 2px;
  background: var(--panel-3);
  overflow: hidden;
}
.dbar b {
  display: block;
  height: 100%;
  background: var(--accent);
  opacity: 0.6;
}
.dbar.warn b {
  background: var(--danger);
}

.empty-hint {
  padding: 4px 20px 6px;
  font-size: 11px;
  color: var(--text-3);
}

.side-foot {
  height: 30px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 5px;
  border-top: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text-3);
  font-size: 11.5px;
}
.side-foot:hover:not(:disabled) {
  color: var(--accent);
}

@media (max-width: 900px) {
  .side {
    width: 158px;
  }
}
</style>
