<script setup lang="ts">
import { computed, ref } from "vue";
import Icon from "./Icon.vue";
import { formatCount, formatSize } from "../utils/format";
import type { IndexStats } from "../types";

const props = defineProps<{
  /** 首屏（磁盘/桌面概览）时不显示「0 个项目」 */
  home?: boolean;  count: number;
  elapsed: number;
  searching: boolean;
  error: string;
  stats: IndexStats | null;
  sortField: string;
  asc: boolean;
  sortFields: string[];
  view: string;
  preview: boolean;
  /** SQL 模式：用「N 行 / N 个分组」代替「N 个项目」，避免和聚合结果对不上 */
  sqlText?: string;
  /** Ctrl 多选中的数量，大于 1 时提示 */
  pickedCount?: number;
}>();

const emit = defineEmits<{
  (e: "sort", field: string): void;
  (e: "view", mode: string): void;
  (e: "toggle-preview"): void;
  (e: "open-settings", tab?: string): void;
}>();

const sortOpen = ref(false);

/** 引擎返回的耗时是浮点毫秒，直接显示会得到 4914.71250000000001 这种数 */
const elapsedText = computed(() => {
  const ms = props.elapsed;
  if (!Number.isFinite(ms) || ms < 0) return "";
  return ms >= 10 ? `${Math.round(ms)} ms` : `${Math.round(ms * 10) / 10} ms`;
});

/** 引擎侧内存（索引 + 同步表 + 缓冲），点开看明细 */
const memText = computed(() => {
  const s = props.stats;
  if (!s?.ready || !s.memory_bytes) return "";
  return formatSize(s.memory_bytes);
});

const idxText = computed(() => {
  const s = props.stats;
  if (!s) return "索引未就绪";
  if (s.scanning) return `正在索引 ${Math.max(0, s.progress).toFixed(1)}%`;
  if (!s.ready) return "索引未就绪";
  return `${formatCount(s.file_count)} 条索引`;
});

function pick(f: string) {
  sortOpen.value = false;
  emit("sort", f);
}
</script>

<template>
  <footer class="status">
    <div class="left">
      <template v-if="props.home">
        <span class="unit home-lbl">首页 · 磁盘与桌面概览</span>
      </template>
      <template v-else-if="props.sqlText">
        <span class="count num">{{ props.sqlText }}</span>
        <span v-if="props.elapsed >= 0" class="elapsed num">{{ elapsedText }}</span>
      </template>
      <template v-else>
      <span class="count num">{{ formatCount(props.count) }}</span>
      <span class="unit">个项目</span>
      <span v-if="props.elapsed >= 0 && props.count > 0" class="elapsed num">
        {{ elapsedText }}
      </span>
      <span v-if="(props.pickedCount || 0) > 1" class="picked num">
        已选 {{ props.pickedCount }} 项
      </span>
      </template>
      <span v-if="props.error" class="err" :title="props.error">
        <Icon name="info" :size="12" />{{ props.error }}
      </span>
      <span v-if="props.stats?.syncing" class="sync" title="实时监控文件变化">
        <i class="dot" />实时同步
      </span>
      <span
        v-else-if="props.stats?.ready && !props.stats.admin"
        class="nosync"
        title="实时同步依赖 USN 日志，需要管理员权限。当前是静态快照：应用运行期间新建、改名、删除的文件都不会出现在结果里。以管理员身份重启即可恢复跟踪。"
      >
        <Icon name="info" :size="12" />索引只读
      </span>
    </div>

    <div class="right">
      <div class="sortwrap">
        <button class="pill" @click.stop="sortOpen = !sortOpen">
          <Icon name="list" :size="12" />
          {{ props.sortField }}
          <Icon :name="props.asc ? 'up' : 'down'" :size="11" />
        </button>
        <div v-if="sortOpen" class="backdrop" @click="sortOpen = false" />
        <transition name="fade">
          <div v-if="sortOpen" class="pop-card sortmenu">
            <div class="ph">排序方式</div>
            <button
              v-for="f in props.sortFields"
              :key="f"
              class="si"
              :class="{ sel: f === props.sortField }"
              @click="pick(f)"
            >
              {{ f }}
              <Icon v-if="f === props.sortField" name="check" :size="12" />
            </button>
            <div class="ph" style="border-top: 1px solid var(--border); margin-top: 4px">
              点击表头可切换升降序
            </div>
          </div>
        </transition>
      </div>

      <div class="vsw">
        <button
          class="vb"
          :class="{ on: props.view === 'list' }"
          title="详情列表"
          @click="emit('view', 'list')"
        >
          <Icon name="list" :size="14" />
        </button>
        <button
          class="vb"
          :class="{ on: props.view === 'dense' }"
          title="紧凑列表"
          @click="emit('view', 'dense')"
        >
          <Icon name="dense" :size="14" />
        </button>
        <button
          class="vb"
          :class="{ on: props.view === 'grid' }"
          title="图标网格"
          @click="emit('view', 'grid')"
        >
          <Icon name="grid" :size="14" />
        </button>
      </div>

      <button class="icon-btn" :class="{ active: props.preview }" title="预览面板" @click="emit('toggle-preview')">
        <Icon name="eye" :size="15" />
      </button>

      <button class="idx" :title="props.stats?.db_path || ''" @click="emit('open-settings', 'index')">
        <Icon name="database" :size="13" />
        <span>{{ idxText }}</span>
        <span v-if="props.stats?.scanning" class="bar">
          <i :style="{ width: Math.min(100, Math.max(0, props.stats.progress)) + '%' }" />
        </span>
      </button>

      <button
        v-if="memText"
        class="idx mem"
        title="性能与内存"
        @click="emit('open-settings', 'memory')"
      >
        <Icon name="memory" :size="13" />
        <span>{{ memText }}</span>
      </button>

      <button class="icon-btn" title="设置" @click="emit('open-settings')">
        <Icon name="sliders" :size="15" />
      </button>
    </div>
  </footer>
</template>

<style scoped>
.status {
  height: 30px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 0 8px 0 14px;
  border-top: 1px solid var(--border);
  background: var(--panel-2);
  font-size: 11.5px;
  color: var(--text-3);
}

.left {
  display: flex;
  align-items: center;
  gap: 7px;
  min-width: 0;
}
.count {
  font-weight: 650;
  color: var(--text);
  font-size: 12px;
}
.unit {
  color: var(--text-3);
}
.home-lbl {
  color: var(--text-2);
  font-weight: 600;
}
.elapsed {
  color: var(--text-3);
  padding-left: 6px;
  border-left: 1px solid var(--border);
  margin-left: 2px;
}
.picked {
  color: var(--accent);
  background: var(--accent-soft);
  border-radius: 5px;
  padding: 1px 7px;
  font-size: 11px;
  font-weight: 600;
}
.err {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--danger);
  max-width: 420px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  padding-left: 8px;
  border-left: 1px solid var(--border);
}
.sync {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--ok);
}
.nosync {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--text-3);
  padding-left: 8px;
  border-left: 1px solid var(--border);
}
.dot {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: var(--ok);
  animation: blink 2s ease-in-out infinite;
}
@keyframes blink {
  50% {
    opacity: 0.3;
  }
}

.right {
  display: flex;
  align-items: center;
  gap: 4px;
}

.sortwrap {
  position: relative;
}
.pill {
  display: flex;
  align-items: center;
  gap: 4px;
  height: 22px;
  padding: 0 7px;
  border-radius: 6px;
  font-size: 11.5px;
  color: var(--text-2);
}
.pill:hover {
  background: var(--hover);
  color: var(--text);
}
.backdrop {
  position: fixed;
  inset: 0;
  z-index: 50;
}
.sortmenu {
  position: absolute;
  right: 0;
  bottom: 26px;
  width: 178px;
  padding: 5px;
  z-index: 51;
}
.ph {
  font-size: 10.5px;
  color: var(--text-3);
  padding: 5px 8px;
  letter-spacing: 0.04em;
}
.si {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 8px;
  border-radius: 6px;
  font-size: 12px;
  text-align: left;
}
.si:hover {
  background: var(--hover);
}
.si.sel {
  color: var(--accent);
  background: var(--accent-soft);
  font-weight: 600;
}

.vsw {
  display: flex;
  gap: 1px;
  background: var(--panel-3);
  border-radius: 7px;
  padding: 1px;
  margin-right: 2px;
}
.vb {
  width: 24px;
  height: 20px;
  display: grid;
  place-items: center;
  border-radius: 5px;
  color: var(--text-3);
}
.vb:hover {
  color: var(--text);
}
.vb.on {
  background: var(--panel);
  color: var(--accent);
  box-shadow: var(--shadow-sm);
}

.icon-btn {
  width: 26px;
  height: 22px;
}

.idx {
  display: flex;
  align-items: center;
  gap: 5px;
  height: 22px;
  padding: 0 8px;
  border-radius: 6px;
  font-size: 11.5px;
  color: var(--text-3);
  max-width: 210px;
  position: relative;
  overflow: hidden;
}
.idx:hover {
  background: var(--hover);
  color: var(--text-2);
}
.bar {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  height: 2px;
  background: var(--accent-soft);
}
.bar i {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.3s;
}
</style>
