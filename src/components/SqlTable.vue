<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { formatCount, formatSize } from "../utils/format";

const props = defineProps<{ data: any; loading?: boolean; selected?: number }>();

const emit = defineEmits<{
  (e: "select", i: number): void;
  (e: "open", i: number): void;
  (e: "menu", p: { i: number; x: number; y: number }): void;
}>();

/** 键盘上下换行时把选中行带进视野 */
const wrap = ref<HTMLElement | null>(null);
watch(
  () => props.selected,
  async (i) => {
    if (i === undefined || i < 0) return;
    await nextTick();
    const el = wrap.value?.querySelector<HTMLElement>("tbody tr.on");
    el?.scrollIntoView({ block: "nearest" });
  }
);

const fields = computed<string[]>(() => (props.data?.字段 as string[]) || []);
const types = computed<number[]>(() => (props.data?.类型 as number[]) || []);
const isAgg = computed(() => !!props.data?.聚合);
const rows = computed<any[][]>(() => (props.data?.行 as any[][]) || []);
const groups = computed<any[]>(() => (props.data?.分组 as any[]) || []);
const totalRows = computed(() => props.data?.总数量 ?? 0);

/** 分组字段名（没写 GROUP BY 时为 null）；其余列都是聚合结果 */
const groupLabel = computed<string | null>(() => props.data?.分组字段 || null);

/** 只显示这条 SQL 里真写了的聚合：只查 COUNT 就不该冒出求和/最小/最大三列 0 */
const aggCols = computed<string[]>(() =>
  fields.value.filter((f) => f !== groupLabel.value)
);

function fmt(v: any, t: number) {
  if (v === null || v === undefined) return "—";
  if (t === 2 || t === 3) return formatCount(Number(v));
  return String(v);
}

/** 路径列给宽一点的截断上限，其余文本列窄一些，免得一列吃掉整个表格 */
function isPathCol(name: string | undefined) {
  const n = String(name || "").toLowerCase();
  return n === "path" || n === "路径" || n === "fullpath";
}

/** 聚合列 → 分组里的值。SUM(Size) 这种按字节数显示成人看的单位。 */
function aggValue(g: any, name: string): string {
  const n = name.toLowerCase();
  const bySize = /\(\s*(size|大小)\s*\)/.test(n);
  const num = (x: number) => (bySize ? formatSize(x) : formatCount(x));
  if (n.startsWith("count")) return formatCount(g.计数 || 0);
  if (n.startsWith("sum")) return num(g.求和 || 0);
  if (n.startsWith("avg"))
    return num(g.计数 ? (g.求和 || 0) / g.计数 : 0);
  if (n.startsWith("min")) return num(g.最小值 || 0);
  if (n.startsWith("max")) return num(g.最大值 || 0);
  return "—";
}
</script>

<template>
  <div class="sqlwrap">
    <div class="sqlhead">
      <span class="badge">SQL</span>
      <span class="info">
        {{ isAgg ? (groupLabel ? `${groups.length} 个分组` : `聚合结果`) : `${formatCount(totalRows)} 行` }}
        <template v-if="aggCols.length"> · {{ aggCols.length }} 列</template>
      </span>
      <span class="hint">
        {{ isAgg ? "结果由引擎 SQL 模式直接输出" : "单击预览 · 双击打开 · 右键更多操作" }}
      </span>
    </div>

    <div v-if="!isAgg" ref="wrap" class="scroll">
      <table>
        <thead>
          <tr>
            <th class="idx">#</th>
            <th v-for="(f, i) in fields" :key="i">{{ f }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="(r, ri) in rows"
            :key="ri"
            :class="{ on: props.selected === ri }"
            @click="emit('select', ri)"
            @dblclick="emit('open', ri)"
            @contextmenu.prevent="emit('menu', { i: ri, x: $event.clientX, y: $event.clientY })"
          >
            <td class="idx">{{ ri + 1 }}</td>
            <td v-for="(v, ci) in r" :key="ci" :class="{ numc: types[ci] === 2 || types[ci] === 3 }">
              <span
                class="cell"
                :class="{ wide: isPathCol(fields[ci]) }"
                :title="String(v ?? '')"
                >{{ fmt(v, types[ci]) }}</span
              >
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-else class="scroll">
      <table class="tight">
        <thead>
          <tr>
            <th class="idx">#</th>
            <th v-if="groupLabel">{{ groupLabel }}</th>
            <th v-for="(c, i) in aggCols" :key="i" class="numc">{{ c }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(g, i) in groups" :key="i">
            <td class="idx">{{ i + 1 }}</td>
            <td v-if="groupLabel">{{ g.键文本 || g.键整数 }}</td>
            <td v-for="(c, ci) in aggCols" :key="ci" class="numc">{{ aggValue(g, c) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>

<style scoped>
.sqlwrap {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.sqlhead {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 14px;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
  font-size: 11.5px;
  color: var(--text-3);
  flex: none;
}
.badge {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.06em;
  padding: 1px 5px;
  border-radius: 4px;
  background: var(--accent-soft);
  color: var(--accent);
}
.info {
  color: var(--text-2);
  font-weight: 600;
}
.hint {
  margin-left: auto;
  font-size: 11px;
}

.scroll {
  flex: 1;
  min-height: 0;
  overflow: auto;
}

table {
  border-collapse: separate;
  border-spacing: 0;
  width: 100%;
  font-size: 12px;
}

/* 聚合表通常只有两三列，铺满整行会把「#」和数值拉到屏幕两端 */
table.tight {
  width: auto;
  min-width: 0;
}
table.tight th,
table.tight td {
  padding-left: 14px;
  padding-right: 14px;
}

th {
  position: sticky;
  top: 0;
  z-index: 2;
  background: var(--panel);
  text-align: left;
  font-weight: 600;
  color: var(--text-3);
  font-size: 11px;
  padding: 7px 12px;
  border-bottom: 1px solid var(--border);
  white-space: nowrap;
}

td {
  padding: 6px 12px;
  border-bottom: 1px solid var(--border);
  color: var(--text-2);
  white-space: nowrap;
  max-width: 460px;
  overflow: hidden;
  text-overflow: ellipsis;
  user-select: text;
}

/* 截断交给内层 span：table-cell 上的 max-width 在自动布局里不生效，
   不加这层的话一个长路径就会把后面几列全推到视口外 */
.cell {
  display: inline-block;
  vertical-align: bottom;
  max-width: 150px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cell.wide {
  max-width: 280px;
}

tbody tr {
  cursor: default;
}

tbody tr:hover td {
  background: var(--hover);
}

/* 选中行和普通结果列表保持同一套配色 */
tbody tr.on td {
  background: var(--accent-soft);
  color: var(--text);
}
tbody tr.on td.idx {
  color: var(--accent);
  font-weight: 600;
}

.numc {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.idx {
  width: 46px;
  color: var(--text-3);
  text-align: right;
  font-size: 11px;
}
</style>
