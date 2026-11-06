<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { marked } from "marked";
import Icon from "./Icon.vue";
import { api } from "../api";
import { formatSize, formatTimeShort, isImage, isText, kindOf } from "../utils/format";
import type { Row } from "../types";

const props = defineProps<{ row: Row | null }>();
const emit = defineEmits<{
  (e: "open", row: Row): void;
  (e: "reveal", row: Row): void;
  (e: "props", row: Row): void;
  (e: "copy", row: Row): void;
  (e: "alias", row: Row): void;
  (e: "rate", row: Row, delta: number): void;
}>();

const iconUrl = ref("");
const text = ref("");
const truncated = ref(false);
const binary = ref(false);
const loading = ref(false);
const showAll = ref(false);

// 文件夹：直接子项（不递归）。改名/内容变化后重新拉。
const kids = ref<Row[]>([]);
const kidsTotal = ref(0);
const kidsLoading = ref(false);
const kidsErr = ref("");
let kidsSeq = 0;

const isDir = computed(() => !!props.row?.is_dir);
const kind = computed(() => (props.row ? kindOf(props.row.ext, props.row.is_dir) : "file"));
const canImage = computed(() => !!props.row && !props.row.is_dir && isImage(props.row.ext));
const canText = computed(
  () => !!props.row && !props.row.is_dir && isText(props.row.ext) && props.row.size < 4_000_000
);
const imgSrc = computed(() => (props.row ? convertFileSrc(props.row.path) : ""));

const mdHtml = computed(() => {
  if (!props.row) return "";
  const esc = text.value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
  try {
    return marked.parse(esc, { async: false }) as string;
  } catch {
    return `<pre>${esc}</pre>`;
  }
});

watch(
  () => props.row?.id,
  async () => {
    iconUrl.value = "";
    text.value = "";
    truncated.value = false;
    binary.value = false;
    showAll.value = false;
    const r = props.row;
    if (!r) return;

    if (r.is_dir) void loadKids(r.path);
    else {
      kids.value = [];
      kidsTotal.value = 0;
      kidsErr.value = "";
    }

    loading.value = true;
    try {
      const ic = await api.previewIcon(r.id, 128);
      if (ic) iconUrl.value = ic;
    } catch {
      /* 图标失败不影响其它预览 */
    }
    if (canText.value && r.size > 0) {
      try {
        const res = await api.readText(r.path, 160_000);
        binary.value = !!res.binary;
        text.value = res.text || "";
        truncated.value = !!res.truncated;
      } catch {
        text.value = "";
      }
    }
    loading.value = false;
  },
  { immediate: true }
);

/** 拉取目录的直接子项；连点多个文件夹时只认最后一次请求 */
async function loadKids(path: string) {
  const my = ++kidsSeq;
  kidsLoading.value = true;
  kidsErr.value = "";
  kids.value = [];
  kidsTotal.value = 0;
  try {
    const res = await api.listDir(path, 200);
    if (my !== kidsSeq) return;
    kids.value = res.rows;
    kidsTotal.value = res.total;
  } catch (e) {
    if (my !== kidsSeq) return;
    kidsErr.value = String(e);
  } finally {
    if (my === kidsSeq) kidsLoading.value = false;
  }
}

const ext = computed(() => (props.row?.ext || "").toUpperCase());
</script>

<template>
  <aside class="preview">
    <template v-if="props.row">
      <div class="head">
        <div class="bigicon" :data-kind="kind">
          <img v-if="iconUrl" :src="iconUrl" alt="" />
          <Icon v-else :name="props.row.is_dir ? 'folder' : 'doc'" :size="40" />
        </div>
        <div class="title" :title="props.row.name">{{ props.row.name }}</div>
        <div class="sub">
          <span v-if="props.row.alias" class="alias"><Icon name="tag" :size="10" />{{ props.row.alias }}</span>
          <span>{{ props.row.is_dir ? "文件夹" : formatSize(props.row.size) }}</span>
          <template v-if="isDir && kidsTotal">
            <span class="dot">·</span>
            <span>{{ kidsTotal }} 项</span>
          </template>
          <template v-if="ext">
            <span class="dot">·</span>
            <span>{{ ext }}</span>
          </template>
        </div>
        <div v-if="isDir" class="where" :title="props.row.parent">
          <Icon name="location" :size="10" />
          <span>{{ props.row.parent }}</span>
        </div>
      </div>

      <div class="body">
        <!-- 选中文件夹：直接把里面摆的东西列出来，双击交给系统打开 -->
        <div v-if="isDir" class="kids">
          <div class="khead">
            <span>文件夹内容</span>
            <span class="knum">{{ kidsLoading ? "读取中…" : `${kidsTotal} 项` }}</span>
          </div>
          <div class="ktip">双击任意一项直接打开</div>
          <div v-if="kidsLoading" class="kempty">读取中…</div>
          <div v-else-if="kidsErr" class="kempty">{{ kidsErr }}</div>
          <div v-else-if="!kids.length" class="kempty">空文件夹</div>
          <div v-else class="klist">
            <button
              v-for="k in kids"
              :key="k.id"
              class="krow"
              :title="k.path"
              @dblclick="emit('open', k)"
            >
              <span class="kico" :data-kind="kindOf(k.ext, k.is_dir)">
                <Icon :name="k.is_dir ? 'folder' : 'doc'" :size="13" />
              </span>
              <span class="kname" :class="{ dir: k.is_dir }">{{ k.name }}</span>
              <span class="ksize num">{{ k.is_dir ? "" : formatSize(k.size) }}</span>
            </button>
          </div>
          <div v-if="kidsTotal > kids.length" class="kmore">
            只列前 {{ kids.length }} 项，双击文件夹在资源管理器中看全部
          </div>
        </div>

        <div v-else-if="canImage" class="media">
          <img :src="imgSrc" :alt="props.row.name" loading="lazy" />
        </div>

        <div v-else-if="canText && !binary" class="textbox" :class="{ expanded: showAll }">
          <div v-if="props.row.ext.toLowerCase() === 'md'" class="md" v-html="mdHtml" />
          <pre v-else>{{ text || (loading ? "读取中…" : "（空文件）") }}</pre>
          <button v-if="truncated" class="more" @click="showAll = !showAll">
            {{ showAll ? "收起" : "文件较大，已截断显示" }}
          </button>
        </div>

        <div v-else class="meta-list">
          <div class="mrow">
            <span class="k">位置</span>
            <span class="v" :title="props.row.parent">{{ props.row.parent }}</span>
          </div>
          <div class="mrow">
            <span class="k">大小</span>
            <span class="v num">{{ props.row.is_dir ? "—" : formatSize(props.row.size) }}</span>
          </div>
          <div class="mrow">
            <span class="k">修改时间</span>
            <span class="v num">{{ formatTimeShort(props.row.mtime) || "—" }}</span>
          </div>
          <div class="mrow">
            <span class="k">创建时间</span>
            <span class="v num">{{ formatTimeShort(props.row.ctime) || "—" }}</span>
          </div>
          <div class="mrow">
            <span class="k">类型</span>
            <span class="v">{{ props.row.file_type || (props.row.is_dir ? "文件夹" : "文件") }}</span>
          </div>
          <div class="mrow">
            <span class="k">评分</span>
            <span class="v num rating">
              {{ props.row.rating }}
              <button class="mini" title="降低评分" @click="emit('rate', props.row, -5)">−</button>
              <button class="mini" title="提高评分" @click="emit('rate', props.row, 5)">+</button>
            </span>
          </div>
        </div>
      </div>

      <div class="actions">
        <button class="primary" @click="emit('open', props.row)">
          <Icon name="external" :size="13" />打开
        </button>
        <button @click="emit('reveal', props.row)"><Icon name="location" :size="13" />定位</button>
        <button @click="emit('copy', props.row)"><Icon name="copy" :size="13" />复制路径</button>
        <button @click="emit('alias', props.row)"><Icon name="tag" :size="13" />别名</button>
        <button @click="emit('props', props.row)"><Icon name="info" :size="13" />属性</button>
      </div>
    </template>

    <div v-else class="placeholder">
      <div class="ph-ico"><Icon name="eye" :size="22" /></div>
      <div>选中文件即可预览</div>
    </div>
  </aside>
</template>

<style scoped>
.preview {
  width: 318px;
  flex: none;
  display: flex;
  flex-direction: column;
  border-left: 1px solid var(--border);
  background: var(--panel-2);
  min-height: 0;
}

.head {
  padding: 16px 18px 12px;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  border-bottom: 1px solid var(--border);
  flex: none;
}

.bigicon {
  width: 74px;
  height: 74px;
  display: grid;
  place-items: center;
  border-radius: 16px;
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: var(--shadow-sm);
  color: var(--text-3);
}
.bigicon img {
  width: 56px;
  height: 56px;
  object-fit: contain;
}
.bigicon[data-kind="folder"] {
  color: #f0b429;
}
.bigicon[data-kind="image"] {
  color: #4aa8ff;
}
.bigicon[data-kind="code"] {
  color: #8b7cf6;
}
.bigicon[data-kind="pdf"] {
  color: #e5645c;
}

.title {
  font-size: 13.5px;
  font-weight: 620;
  text-align: center;
  line-height: 1.4;
  word-break: break-all;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  max-width: 100%;
}

.sub {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
  color: var(--text-3);
}
.alias {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  color: var(--accent);
  background: var(--accent-soft);
  border-radius: 4px;
  padding: 1px 5px;
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dot {
  opacity: 0.5;
}

.body {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 12px;
}

.media {
  display: grid;
  place-items: center;
  border-radius: 10px;
  background: var(--panel);
  border: 1px solid var(--border);
  padding: 8px;
  min-height: 120px;
}
.media img {
  max-width: 100%;
  max-height: 260px;
  object-fit: contain;
  border-radius: 6px;
}

.textbox {
  position: relative;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 10px 12px;
  max-height: 360px;
  overflow: auto;
}
.textbox pre {
  margin: 0;
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--text-2);
  user-select: text;
}
.md {
  font-size: 12.5px;
  line-height: 1.65;
  color: var(--text-2);
  user-select: text;
  pointer-events: none;
}
.md :deep(h1),
.md :deep(h2),
.md :deep(h3) {
  color: var(--text);
  margin: 8px 0 6px;
  font-size: 13.5px;
}
.md :deep(pre) {
  background: var(--panel-3);
  padding: 8px;
  border-radius: 6px;
  overflow: auto;
}
.md :deep(code) {
  font-family: var(--font-mono);
  font-size: 11px;
  background: var(--panel-3);
  padding: 1px 4px;
  border-radius: 4px;
}
.md :deep(a) {
  color: var(--accent);
  text-decoration: none;
}
.md :deep(ul) {
  padding-left: 18px;
}
.more {
  width: 100%;
  margin-top: 8px;
  font-size: 11px;
  color: var(--accent);
  padding: 5px;
  border-radius: 6px;
  background: var(--accent-soft);
}

.kids {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.khead {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 5px 6px;
  font-size: 11.5px;
  color: var(--text-3);
}
.klist {
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.krow {
  display: flex;
  align-items: center;
  gap: 7px;
  width: 100%;
  padding: 5px 6px;
  border: none;
  border-radius: 6px;
  background: none;
  font-size: 12px;
  color: var(--text-2);
  text-align: left;
  user-select: none;
}
.krow:hover {
  background: var(--hover);
}
.kico {
  flex: none;
  width: 16px;
  height: 16px;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.kico[data-kind="folder"] {
  color: #f0b429;
}
.kico[data-kind="image"] {
  color: #4aa8ff;
}
.kico[data-kind="code"] {
  color: #8b7cf6;
}
.kico[data-kind="pdf"] {
  color: #e5645c;
}
.kname {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.kname.dir {
  color: var(--text);
}
.ksize {
  flex: none;
  font-family: var(--font-mono);
  font-size: 10.5px;
  color: var(--text-3);
}
.kempty {
  padding: 20px 6px;
  text-align: center;
  font-size: 12px;
  color: var(--text-3);
}
.kmore {
  margin-top: 6px;
  padding: 0 5px;
  font-size: 11px;
  color: var(--text-3);
}
.ktip {
  margin-bottom: 4px;
  padding: 0 5px 4px;
  font-size: 11px;
  color: var(--text-3);
  opacity: 0.7;
}
.where {
  display: flex;
  align-items: center;
  gap: 4px;
  max-width: 100%;
  padding: 0 4px;
  font-size: 11px;
  color: var(--text-3);
}
.where span {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  text-align: center;
}

.meta-list {
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.mrow {
  display: flex;
  gap: 10px;
  padding: 6px 8px;
  border-radius: 6px;
  font-size: 12px;
}
.mrow:hover {
  background: var(--hover);
}
.k {
  width: 62px;
  flex: none;
  color: var(--text-3);
  font-size: 11.5px;
}
.v {
  flex: 1;
  min-width: 0;
  color: var(--text-2);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  user-select: text;
}
.rating {
  display: flex;
  align-items: center;
  gap: 6px;
}
.mini {
  width: 17px;
  height: 17px;
  border-radius: 5px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  font-size: 12px;
  line-height: 1;
  display: grid;
  place-items: center;
}
.mini:hover {
  border-color: var(--accent);
  color: var(--accent);
}

.actions {
  flex: none;
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 6px;
  padding: 10px 12px 12px;
  border-top: 1px solid var(--border);
}
.actions button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 5px;
  height: 28px;
  padding: 0 8px;
  border-radius: 7px;
  background: var(--panel);
  border: 1px solid var(--border);
  font-size: 11.5px;
  color: var(--text-2);
  transition: all 0.12s;
}
.actions button:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.actions .primary {
  background: var(--accent);
  border-color: var(--accent);
  color: #fff;
  font-weight: 600;
}
.actions .primary:hover {
  filter: brightness(1.08);
  color: #fff;
}

.placeholder {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--text-3);
  font-size: 12px;
}
.ph-ico {
  width: 46px;
  height: 46px;
  border-radius: 14px;
  display: grid;
  place-items: center;
  background: var(--hover);
}
</style>
