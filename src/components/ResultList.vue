<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import Icon from "./Icon.vue";
import { ensure, epoch, iconFor, requestIcons, rowAt, total } from "../store/results";
import { formatSize, formatTime, highlight, kindOf } from "../utils/format";
import type { Row } from "../types";

const props = defineProps<{
  rowH: number;
  selected: number;
  sortField: string;
  asc: boolean;
  dense?: boolean;
  searching?: boolean;
  ready?: boolean;
  /** 多选中的行号（Ctrl 点击加入，不含主选中时也要高亮） */
  picked?: Set<number>;
  /** 空结果时额外的一句解释（例如选中的磁盘还没建索引） */
  emptyHint?: string;
}>();

const emit = defineEmits<{
  (e: "select", index: number, additive: boolean, range: boolean): void;
  (e: "open", row: Row): void;
  (e: "menu", payload: { row: Row; index: number; x: number; y: number }): void;
  (e: "sort", field: string): void;
}>();

const scroller = ref<HTMLDivElement | null>(null);
const scrollTop = ref(0);
const viewH = ref(600);
let ro: ResizeObserver | null = null;

const BUFFER = 6;

const start = computed(() =>
  Math.max(0, Math.floor(scrollTop.value / props.rowH) - BUFFER)
);
const end = computed(() =>
  Math.min(total.value, Math.ceil((scrollTop.value + viewH.value) / props.rowH) + BUFFER)
);

type Item = { index: number; row: Row | undefined };
const items = computed<Item[]>(() => {
  const out: Item[] = [];
  for (let i = start.value; i < end.value; i++) {
    const r = rowAt(i);
    if (r && r.id < 0) continue;
    out.push({ index: i, row: r });
  }
  return out;
});

watch(
  () => [start.value, end.value, total.value, epoch.value],
  () => {
    void ensure(start.value, end.value - start.value);
  },
  { immediate: true }
);

watch(
  items,
  (list) => {
    const ids = list.map((it) => it.row?.id).filter((id): id is number => !!id && id > 0);
    if (ids.length) requestIcons(Array.from(new Set(ids)), 32);
  },
  { immediate: true, deep: false }
);

watch(
  () => props.selected,
  (idx) => {
    if (idx < 0) return;
    const top = idx * props.rowH;
    const el = scroller.value;
    if (!el) return;
    if (top < el.scrollTop + 2) el.scrollTop = Math.max(0, top - props.rowH);
    else if (top + props.rowH > el.scrollTop + el.clientHeight - 2)
      el.scrollTop = top - el.clientHeight + props.rowH * 2;
  }
);

function onScroll() {
  const el = scroller.value;
  if (!el) return;
  scrollTop.value = el.scrollTop;
}

function onWheel(e: WheelEvent) {
  const el = scroller.value;
  if (!el) return;
  if (e.ctrlKey) return;
  el.scrollTop += e.deltaY;
}

onMounted(() => {
  const el = scroller.value;
  if (!el) return;
  viewH.value = el.clientHeight;
  ro = new ResizeObserver(() => {
    viewH.value = el.clientHeight;
  });
  ro.observe(el);
});
onBeforeUnmount(() => ro?.disconnect());

function segs(text: string, keys: string[]) {
  return highlight(text, keys);
}

function rowClass(i: number) {
  return {
    sel: i === props.selected,
    picked: !!props.picked?.has(i),
    odd: i % 2 === 1,
  };
}

function onClick(index: number, e: MouseEvent) {
  emit("select", index, e.ctrlKey || e.metaKey, e.shiftKey);
}

function sortBy(field: string) {
  emit("sort", field);
}

function isSorted(field: string) {
  return props.sortField === field;
}
</script>

<template>
  <div class="wrap">
    <div class="head" :class="{ dense: props.dense }">
      <button class="h name" @click="sortBy('文件名')">
        名称
        <Icon v-if="isSorted('文件名')" :name="props.asc ? 'up' : 'down'" :size="11" />
      </button>
      <button class="h parent" @click="sortBy('文件夹')">
        所在位置
        <Icon v-if="isSorted('文件夹')" :name="props.asc ? 'up' : 'down'" :size="11" />
      </button>
      <button class="h size" @click="sortBy('文件大小')">
        大小
        <Icon v-if="isSorted('文件大小')" :name="props.asc ? 'up' : 'down'" :size="11" />
      </button>
      <button class="h time" @click="sortBy('修改时间')">
        修改时间
        <Icon v-if="isSorted('修改时间')" :name="props.asc ? 'up' : 'down'" :size="11" />
      </button>
      <button class="h type" @click="sortBy('文件类型')">
        类型
        <Icon v-if="isSorted('文件类型')" :name="props.asc ? 'up' : 'down'" :size="11" />
      </button>
    </div>

    <div ref="scroller" class="scroller" @scroll.passive="onScroll" @wheel.passive="onWheel">
      <div class="spacer" :style="{ height: total * props.rowH + 'px' }">
        <div
          v-for="it in items"
          :key="it.index"
          class="row"
          :class="rowClass(it.index)"
          :style="{ top: it.index * props.rowH + 'px', height: props.rowH + 'px' }"
          @click="onClick(it.index, $event)"
          @dblclick="it.row && emit('open', it.row)"
          @contextmenu.prevent="
            it.row &&
              emit('menu', {
                row: it.row,
                index: it.index,
                x: $event.clientX,
                y: $event.clientY,
              })
          "
        >
          <template v-if="!it.row">
            <div class="c name"><span class="skel" style="width: 46%" /></div>
            <div class="c parent"><span class="skel" style="width: 60%" /></div>
            <div class="c size"><span class="skel" style="width: 40%" /></div>
            <div class="c time"><span class="skel" style="width: 70%" /></div>
            <div class="c type"><span class="skel" style="width: 50%" /></div>
          </template>
          <template v-else>
            <div class="c name">
              <span class="ico" :data-kind="kindOf(it.row.ext, it.row.is_dir)">
                <img v-if="iconFor(it.row.id, 32)" :src="iconFor(it.row.id, 32)" alt="" />
                <Icon v-else :name="it.row.is_dir ? 'folder' : 'doc'" :size="16" />
              </span>
              <span class="nm">
                <template v-for="(s, i) in segs(it.row.name, it.row.hl_file)" :key="i">
                  <mark v-if="s.hit">{{ s.t }}</mark><template v-else>{{ s.t }}</template>
                </template>
              </span>
              <span v-if="it.row.alias" class="alias">
                <Icon name="tag" :size="10" />{{ it.row.alias }}
              </span>
            </div>
            <div class="c parent">
              <span class="pt">
                <template v-for="(s, i) in segs(it.row.parent, it.row.hl_dir)" :key="i">
                  <mark v-if="s.hit">{{ s.t }}</mark><template v-else>{{ s.t }}</template>
                </template>
              </span>
            </div>
            <div class="c size num">{{ it.row.is_dir ? "—" : formatSize(it.row.size) }}</div>
            <div class="c time num">{{ formatTime(it.row.mtime) }}</div>
            <div class="c type">{{ it.row.is_dir ? "文件夹" : it.row.ext || "—" }}</div>
          </template>
        </div>
      </div>

      <div v-if="total === 0 && !props.searching" class="empty">
        <div class="empty-ico"><Icon name="search" :size="26" /></div>
        <div class="empty-title">{{ props.ready === false ? "索引尚未就绪" : "没有匹配的文件" }}</div>
        <div class="empty-sub">
          {{
            props.ready === false
              ? "在设置中选择磁盘并建立索引后即可搜索"
              : props.emptyHint || "试试更短的关键词、拼音首拼，或换一种搜索模式"
          }}
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.wrap {
  display: flex;
  flex-direction: column;
  min-height: 0;
  flex: 1;
  position: relative;
}

.head {
  display: grid;
  grid-template-columns: minmax(180px, 1.4fr) minmax(130px, 1.25fr) 82px 126px 72px;
  gap: 10px;
  /* 右侧多留 10px 抵掉滚动条宽度，保证与行内列严格对齐 */
  padding: 0 24px 0 14px;
  height: 28px;
  align-items: center;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
  flex: none;
}

.h {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  font-weight: 600;
  color: var(--text-3);
  letter-spacing: 0.04em;
  padding: 0;
  text-align: left;
  height: 100%;
}
.h:hover {
  color: var(--text);
}
.h.size,
.h.time {
  justify-content: flex-end;
}
.h.type {
  justify-content: flex-start;
}

.scroller {
  position: relative;
  overflow-y: auto;
  overflow-x: hidden;
  flex: 1;
  min-height: 0;
  padding: 0;
  scrollbar-gutter: stable;
  contain: strict;
}

.spacer {
  position: relative;
  width: 100%;
}

.row {
  position: absolute;
  left: 0;
  right: 0;
  display: grid;
  grid-template-columns: minmax(180px, 1.4fr) minmax(130px, 1.25fr) 82px 126px 72px;
  gap: 10px;
  align-items: center;
  padding: 0 14px;
  border-radius: 7px;
  cursor: default;
  content-visibility: auto;
}
.row:hover {
  background: var(--hover);
}
.row.sel,
.row.picked {
  background: var(--selected);
}
.row.sel::before {
  content: "";
  position: absolute;
  left: 0;
  top: 6px;
  bottom: 6px;
  width: 2.5px;
  border-radius: 2px;
  background: var(--accent);
}

.c {
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  font-size: 12.5px;
}
.c.size,
.c.time {
  text-align: right;
  color: var(--text-2);
  font-size: 11.5px;
}
.c.type {
  color: var(--text-3);
  font-size: 11px;
  text-transform: uppercase;
}
.c.parent {
  color: var(--text-3);
  font-size: 11.5px;
  direction: ltr;
}
.pt {
  unicode-bidi: plaintext;
}

.c.name {
  display: flex;
  align-items: center;
  gap: 8px;
}

.ico {
  width: 18px;
  height: 18px;
  display: grid;
  place-items: center;
  flex: none;
  color: var(--text-3);
}
.ico img {
  width: 17px;
  height: 17px;
  object-fit: contain;
  image-rendering: -webkit-optimize-contrast;
}
.ico[data-kind="folder"] {
  color: #f0b429;
}
.ico[data-kind="image"] {
  color: #4aa8ff;
}
.ico[data-kind="code"] {
  color: #8b7cf6;
}
.ico[data-kind="archive"] {
  color: #e0a34a;
}
.ico[data-kind="app"] {
  color: #56b6a4;
}
.ico[data-kind="pdf"] {
  color: #e5645c;
}
.ico[data-kind="doc"] {
  color: #4a86e8;
}
.ico[data-kind="sheet"] {
  color: #3aa76d;
}
.ico[data-kind="slide"] {
  color: #e07a4a;
}
.ico[data-kind="audio"],
.ico[data-kind="video"] {
  color: #d469c9;
}

.nm {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.alias {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  font-size: 10.5px;
  color: var(--accent);
  background: var(--accent-soft);
  border-radius: 4px;
  padding: 1px 5px;
  flex: none;
  max-width: 120px;
  overflow: hidden;
  text-overflow: ellipsis;
}

.skel {
  display: inline-block;
  height: 8px;
  border-radius: 4px;
  background: var(--hover-strong);
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  50% {
    opacity: 0.45;
  }
}

.empty {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  color: var(--text-3);
  pointer-events: none;
}
.empty-ico {
  width: 52px;
  height: 52px;
  border-radius: 16px;
  display: grid;
  place-items: center;
  background: var(--hover);
  margin-bottom: 4px;
}
.empty-title {
  font-size: 13.5px;
  font-weight: 600;
  color: var(--text-2);
}
.empty-sub {
  font-size: 12px;
}
</style>
