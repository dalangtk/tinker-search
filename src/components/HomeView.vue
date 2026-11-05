<script setup lang="ts">
import Icon from "./Icon.vue";
import { formatCount, formatSize, formatTime, kindOf } from "../utils/format";
import type { HomeItem, HomeSnapshot } from "../types";

const props = defineProps<{ home: HomeSnapshot | null; loading: boolean }>();

const emit = defineEmits<{
  (e: "open", path: string, isDir: boolean): void;
  (e: "reveal", path: string): void;
  (e: "use-place", path: string): void;
  (e: "pick-drive", letter: string): void;
  (e: "refresh"): void;
}>();

const ICON_OF: Record<string, string> = {
  folder: "folder",
  image: "image",
  audio: "music",
  video: "video",
  archive: "archive",
  app: "app",
  code: "code",
  pdf: "doc",
  doc: "doc",
  sheet: "doc",
  slide: "doc",
  text: "doc",
  file: "doc",
};

function iconOf(it: HomeItem) {
  return ICON_OF[kindOf(it.ext, it.is_dir)] || "doc";
}

function tone(p: number) {
  return p >= 92 ? "hot" : p >= 75 ? "warm" : "";
}

function driveName(d: { name: string; kind: number }) {
  if (d.name) return d.name;
  if (d.kind === 2) return "可移动磁盘";
  if (d.kind === 5) return "光驱";
  if (d.kind === 3) return "本地磁盘";
  return "磁盘";
}
</script>

<template>
  <div class="home">
    <div class="home-top">
      <div class="ht-title">
        <Icon name="layers" :size="15" />
        开始浏览
        <span class="ht-sub">输入关键词可直接全盘搜索；下面这些入口点一下就能筛</span>
      </div>
      <button class="ht-refresh" :disabled="props.loading" @click="emit('refresh')">
        <Icon name="refresh" :size="12" :class="{ spin: props.loading }" />
        刷新
      </button>
    </div>

    <div class="home-body">
      <template v-if="props.home">
        <!-- 磁盘 + 容量 -->
        <section class="sec">
          <div class="sec-h">
            <Icon name="drive" :size="13" />
            磁盘
            <span class="sec-sub">{{ props.home.drives.length }} 个可用卷</span>
          </div>
          <div class="drives">
            <button
              v-for="d in props.home.drives"
              :key="d.letter"
              class="dcard"
              :class="{ noindex: d.indexed === false }"
              :title="
                d.indexed === false
                  ? `${d.letter} 还没建索引，选中后不会有结果`
                  : `在 ${d.letter} 内搜索`
              "
              @click="emit('pick-drive', d.letter)"
            >
              <div class="dtop">
                <span class="dico"><Icon name="drive" :size="17" /></span>
                <span class="dl">{{ d.letter }}</span>
                <span class="dn">{{ driveName(d) }}</span>
                <span v-if="d.indexed === false" class="dni">未索引</span>
              </div>
              <div class="dbar" :class="tone(d.percent)">
                <i :style="{ width: Math.min(100, Math.max(1, d.percent)) + '%' }" />
              </div>
              <div class="dmeta">
                <span>已用 <b>{{ formatSize(d.used) }}</b> / {{ formatSize(d.total) }}</span>
                <span class="dfree">可用 {{ formatSize(d.free) }}</span>
              </div>
            </button>
          </div>
        </section>

        <!-- 桌面 -->
        <section class="sec">
          <div class="sec-h">
            <Icon name="desktop" :size="13" />
            桌面
            <span class="sec-sub">{{ formatCount(props.home.desktop.total) }} 项</span>
            <button
              class="sec-more"
              :disabled="!props.home.desktop.path"
              @click="emit('use-place', props.home.desktop.path)"
            >
              在搜索中查看 →
            </button>
          </div>
          <div v-if="props.home.desktop.items.length" class="tiles">
            <button
              v-for="it in props.home.desktop.items"
              :key="it.path"
              class="tile"
              :title="it.path"
              @dblclick="emit('open', it.path, it.is_dir)"
              @contextmenu.prevent="emit('reveal', it.path)"
            >
              <span class="tico" :data-kind="kindOf(it.ext, it.is_dir)">
                <Icon :name="iconOf(it)" :size="22" />
              </span>
              <span class="tnm">{{ it.name }}</span>
              <span class="tmeta">{{ it.is_dir ? "文件夹" : formatSize(it.size) }}</span>
            </button>
          </div>
          <div v-else class="sec-empty">
            <template v-if="props.home.desktop.error">读不到桌面：{{ props.home.desktop.error }}</template>
            <template v-else>桌面是空的</template>
          </div>
        </section>

        <!-- 各磁盘详细内容 -->
        <section v-for="s in props.home.sections" :key="s.path" class="sec">
          <div class="sec-h">
            <Icon name="folder" :size="13" />
            {{ s.path }}
            <span class="sec-sub">{{ formatCount(s.total) }} 项</span>
            <button class="sec-more" @click="emit('use-place', s.path)">在此搜索 →</button>
          </div>
          <div v-if="s.items.length" class="rows">
            <button
              v-for="it in s.items"
              :key="it.path"
              class="row"
              :title="it.path"
              @dblclick="emit('open', it.path, it.is_dir)"
              @contextmenu.prevent="emit('reveal', it.path)"
            >
              <span class="rico" :data-kind="kindOf(it.ext, it.is_dir)">
                <Icon :name="iconOf(it)" :size="15" />
              </span>
              <span class="rnm">{{ it.name }}</span>
              <span class="rsz">{{ it.is_dir ? "—" : formatSize(it.size) }}</span>
              <span class="rtm">{{ formatTime(it.mtime) }}</span>
            </button>
          </div>
          <div v-else class="sec-empty">
            <template v-if="s.error">无法读取：{{ s.error }}</template>
            <template v-else>这个盘根目录下没有可见项</template>
          </div>
        </section>
      </template>

      <div v-else class="skeleton">
        <div v-for="i in 6" :key="i" class="sk" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.home {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  background: var(--panel);
}

.home-top {
  flex: none;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 14px;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
}
.ht-title {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 12px;
  font-weight: 650;
  color: var(--text-2);
}
.ht-sub {
  font-weight: 400;
  color: var(--text-3);
  font-size: 11px;
  margin-left: 2px;
}
.ht-refresh {
  display: flex;
  align-items: center;
  gap: 4px;
  height: 22px;
  padding: 0 8px;
  border-radius: 6px;
  font-size: 11.5px;
  color: var(--text-3);
}
.ht-refresh:hover {
  background: var(--hover);
  color: var(--text);
}
.spin {
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.home-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 12px 14px 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.sec {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.sec-h {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 650;
  color: var(--text-2);
}
.sec-h > svg {
  color: var(--text-3);
}
.sec-sub {
  font-weight: 400;
  font-size: 11px;
  color: var(--text-3);
}
.sec-more {
  margin-left: auto;
  font-size: 11px;
  font-weight: 500;
  color: var(--accent);
  padding: 1px 5px;
  border-radius: 5px;
}
.sec-more:hover {
  background: var(--accent-soft);
}
.sec-empty {
  font-size: 11.5px;
  color: var(--text-3);
  padding: 6px 2px;
}

/* ---------- 磁盘卡片 ---------- */
.drives {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(196px, 1fr));
  gap: 8px;
}
.dcard {
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 10px 11px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--panel-2);
  text-align: left;
  transition: border-color 0.14s, transform 0.14s, box-shadow 0.14s;
}
.dcard:hover {
  border-color: var(--accent);
  box-shadow: var(--shadow-sm);
  transform: translateY(-1px);
}
.dtop {
  display: flex;
  align-items: center;
  gap: 6px;
}
.dico {
  color: var(--accent);
  display: grid;
  place-items: center;
}
.dl {
  font-size: 13px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}
.dn {
  font-size: 11px;
  color: var(--text-3);
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.dbar {
  height: 5px;
  border-radius: 3px;
  background: var(--panel-3);
  overflow: hidden;
}
.dbar i {
  display: block;
  height: 100%;
  background: linear-gradient(90deg, var(--accent), var(--accent-2));
  transition: width 0.4s;
}
.dbar.warm i {
  background: linear-gradient(90deg, #e0a34a, #f0b429);
}
.dbar.hot i {
  background: linear-gradient(90deg, var(--danger), #ff8a5c);
}
.dmeta {
  display: flex;
  justify-content: space-between;
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.dmeta b {
  color: var(--text-2);
  font-weight: 600;
}
/* 这个盘还没进索引：卡片整体淡一点，并在右上角挂个「未索引」 */
.dcard.noindex {
  opacity: 0.62;
}
.dni {
  flex: none;
  margin-left: auto;
  padding: 1px 5px;
  border-radius: 4px;
  background: var(--panel-3);
  color: var(--text-3);
  font-size: 9.5px;
  letter-spacing: 0.2px;
}

/* ---------- 桌面图块 ---------- */
.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(148px, 1fr));
  gap: 6px;
}
.tile {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 8px;
  border: 1px solid transparent;
  text-align: left;
  min-width: 0;
}
.tile:hover {
  background: var(--hover);
  border-color: var(--border);
}
.tico {
  width: 26px;
  height: 26px;
  flex: none;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.tico[data-kind="folder"] {
  color: #f0b429;
}
.tico[data-kind="image"] {
  color: #4aa8ff;
}
.tico[data-kind="code"] {
  color: #8b7cf6;
}
.tico[data-kind="archive"] {
  color: #e0a34a;
}
.tico[data-kind="app"] {
  color: #56b6a4;
}
.tico[data-kind="pdf"] {
  color: #e5645c;
}
.tico[data-kind="doc"],
.tico[data-kind="text"] {
  color: #4a86e8;
}
.tico[data-kind="audio"],
.tico[data-kind="video"] {
  color: #d469c9;
}
.tnm {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.tmeta {
  flex: none;
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}

/* ---------- 磁盘根目录列表 ---------- */
.rows {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 1px 10px;
}
.row {
  display: flex;
  align-items: center;
  gap: 7px;
  height: 26px;
  padding: 0 8px;
  border-radius: 6px;
  text-align: left;
  min-width: 0;
}
.row:hover {
  background: var(--hover);
}
.rico {
  width: 16px;
  flex: none;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.rico[data-kind="folder"] {
  color: #f0b429;
}
.rnm {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.rsz {
  flex: none;
  width: 62px;
  text-align: right;
  font-size: 11px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.rtm {
  flex: none;
  width: 104px;
  text-align: right;
  font-size: 10.5px;
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}

/* ---------- 骨架 ---------- */
.skeleton {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(196px, 1fr));
  gap: 8px;
}
.sk {
  height: 74px;
  border-radius: 10px;
  background: var(--panel-3);
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  50% {
    opacity: 0.5;
  }
}
</style>
