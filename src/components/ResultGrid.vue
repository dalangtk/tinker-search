<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import Icon from "./Icon.vue";
import { ensure, epoch, iconFor, requestIcons, rowAt, total } from "../store/results";
import { formatSize, highlight, kindOf } from "../utils/format";
import type { Row } from "../types";

const props = defineProps<{
  tileW: number;
  selected: number;
  searching?: boolean;
  /** 多选中的行号（Ctrl 点击加入） */
  picked?: Set<number>;
}>();

const emit = defineEmits<{
  (e: "select", index: number, additive: boolean, range: boolean): void;
  (e: "open", row: Row): void;
  (e: "menu", payload: { row: Row; index: number; x: number; y: number }): void;
}>();

function onClick(index: number, e: MouseEvent) {
  emit("select", index, e.ctrlKey || e.metaKey, e.shiftKey);
}

const scroller = ref<HTMLDivElement | null>(null);
const scrollTop = ref(0);
const viewW = ref(800);
const viewH = ref(500);
let ro: ResizeObserver | null = null;

const GAP = 10;
const PAD = 14;
const tileH = computed(() => props.tileW + 30);

const cols = computed(() => {
  const usable = Math.max(viewW.value - PAD * 2, props.tileW);
  return Math.max(1, Math.floor((usable + GAP) / (props.tileW + GAP)));
});

const rowCount = computed(() => Math.ceil(total.value / cols.value));
const rowH = computed(() => tileH.value + GAP);

const startRow = computed(() => Math.max(0, Math.floor(scrollTop.value / rowH.value) - 2));
const endRow = computed(() =>
  Math.min(rowCount.value, Math.ceil((scrollTop.value + viewH.value) / rowH.value) + 2)
);

const tiles = computed(() => {
  const out: { index: number; row: Row | undefined }[] = [];
  const from = startRow.value * cols.value;
  const to = Math.min(total.value, endRow.value * cols.value);
  for (let i = from; i < to; i++) {
    const r = rowAt(i);
    if (r && r.id < 0) continue;
    out.push({ index: i, row: r });
  }
  return out;
});

watch(
  () => [startRow.value, endRow.value, total.value, epoch.value, cols.value],
  () => {
    void ensure(startRow.value * cols.value, (endRow.value - startRow.value) * cols.value);
  },
  { immediate: true }
);

watch(
  tiles,
  (list) => {
    const ids = list.map((t) => t.row?.id).filter((id): id is number => !!id && id > 0);
    if (ids.length) requestIcons(Array.from(new Set(ids)), 48);
  },
  { immediate: true }
);

watch(
  () => props.selected,
  (idx) => {
    const el = scroller.value;
    if (!el || idx < 0) return;
    const r = Math.floor(idx / cols.value);
    const top = r * rowH.value;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + rowH.value > el.scrollTop + el.clientHeight)
      el.scrollTop = top - el.clientHeight + rowH.value;
  }
);

function onScroll() {
  if (scroller.value) scrollTop.value = scroller.value.scrollTop;
}

onMounted(() => {
  const el = scroller.value;
  if (!el) return;
  viewW.value = el.clientWidth;
  viewH.value = el.clientHeight;
  ro = new ResizeObserver(() => {
    viewW.value = el.clientWidth;
    viewH.value = el.clientHeight;
  });
  ro.observe(el);
});
onBeforeUnmount(() => ro?.disconnect());

function pos(i: number) {
  const c = i % cols.value;
  const r = Math.floor(i / cols.value);
  return {
    left: PAD + c * (props.tileW + GAP) + "px",
    top: r * rowH.value + "px",
    width: props.tileW + "px",
    height: tileH.value + "px",
  };
}
</script>

<template>
  <div ref="scroller" class="grid" @scroll.passive="onScroll">
    <div class="spacer" :style="{ height: rowCount * rowH + 'px' }">
      <div
        v-for="t in tiles"
        :key="t.index"
        class="tile"
        :class="{ sel: t.index === props.selected, picked: !!props.picked?.has(t.index) }"
        :style="pos(t.index)"
        @click="onClick(t.index, $event)"
        @dblclick="t.row && emit('open', t.row)"
        @contextmenu.prevent="
          t.row &&
            emit('menu', {
              row: t.row,
              index: t.index,
              x: $event.clientX,
              y: $event.clientY,
            })
        "
      >
        <div class="thumb" :data-kind="t.row ? kindOf(t.row.ext, t.row.is_dir) : 'file'">
          <img v-if="t.row && iconFor(t.row.id, 48)" :src="iconFor(t.row.id, 48)" alt="" />
          <Icon v-else :name="t.row?.is_dir ? 'folder' : 'doc'" :size="Math.round(props.tileW * 0.34)" />
        </div>
        <div class="caption">
          <template v-if="t.row">
            <template v-for="(s, i) in highlight(t.row.name, t.row.hl_file)" :key="i">
              <mark v-if="s.hit">{{ s.t }}</mark><template v-else>{{ s.t }}</template>
            </template>
          </template>
          <span v-else class="skel" />
        </div>
        <div class="meta">{{ t.row ? (t.row.is_dir ? "文件夹" : formatSize(t.row.size)) : "" }}</div>
      </div>
    </div>

    <div v-if="total === 0 && !props.searching" class="empty">
      <Icon name="search" :size="26" />
      <div>没有匹配的文件</div>
    </div>
  </div>
</template>

<style scoped>
.grid {
  position: relative;
  overflow-y: auto;
  overflow-x: hidden;
  flex: 1;
  min-height: 0;
  contain: strict;
}

.spacer {
  position: relative;
  width: 100%;
}

.tile {
  position: absolute;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: flex-start;
  gap: 4px;
  padding: 10px 8px 6px;
  border-radius: 10px;
  overflow: hidden;
}
.tile:hover {
  background: var(--hover);
}
.tile.sel,
.tile.picked {
  background: var(--selected);
}
.tile.sel {
  box-shadow: inset 0 0 0 1.5px var(--accent);
}
.tile.picked:not(.sel) {
  box-shadow: inset 0 0 0 1px var(--accent-soft, var(--border));
}

.thumb {
  flex: none;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.thumb img {
  width: calc(var(--tile-icon, 46px));
  width: 46px;
  height: 46px;
  object-fit: contain;
}
.thumb[data-kind="folder"] {
  color: #f0b429;
}
.thumb[data-kind="image"] {
  color: #4aa8ff;
}
.thumb[data-kind="code"] {
  color: #8b7cf6;
}
.thumb[data-kind="archive"] {
  color: #e0a34a;
}
.thumb[data-kind="app"] {
  color: #56b6a4;
}
.thumb[data-kind="pdf"] {
  color: #e5645c;
}

.caption {
  width: 100%;
  font-size: 11.5px;
  line-height: 1.35;
  text-align: center;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  word-break: break-all;
}

.meta {
  font-size: 10.5px;
  color: var(--text-3);
}

.skel {
  display: block;
  height: 8px;
  width: 70%;
  margin: 4px auto;
  border-radius: 4px;
  background: var(--hover-strong);
}

.empty {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  color: var(--text-3);
  font-size: 13px;
}
</style>
