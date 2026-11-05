<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import Icon from "./Icon.vue";
import type { FacetTag, ModeDef } from "../types";

const props = defineProps<{
  modelValue: string;
  mode: ModeDef;
  modes: ModeDef[];
  /** 左侧分类栏已选中的项，每种分类一个标签显示在这里 */
  tags?: FacetTag[];
  searching: boolean;
  scope: string | null;
  placeholder?: string;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  (e: "update:modelValue", v: string): void;
  (e: "mode-change", m: ModeDef): void;
  (e: "clear-tag", kind: "places" | "types" | "drives"): void;
  (e: "clear-tags"): void;
  (e: "open-settings"): void;
  (e: "submit"): void;
  (e: "escape"): void;
  (e: "pick-scope"): void;
  (e: "clear-scope"): void;
  (e: "focus"): void;
  (e: "blur"): void;
  (e: "interact"): void;
}>();

const inputEl = ref<HTMLInputElement | null>(null);
const modeOpen = ref(false);
const moreModes = ref(false);
const scopeName = computed(() => {
  if (!props.scope) return "范围";
  const parts = props.scope.split("\\").filter(Boolean);
  return parts[parts.length - 1] || props.scope;
});

function closeAll() {
  modeOpen.value = false;
  moreModes.value = false;
}

function onInput(e: Event) {
  emit("update:modelValue", (e.target as HTMLInputElement).value);
  if (modeOpen.value) closeAll();
}

function pickMode(m: ModeDef) {
  emit("mode-change", m);
  closeAll();
  focus();
}

function focus() {
  nextTick(() => inputEl.value?.focus());
}

defineExpose({ focus });

onMounted(() => {
  if (!props.disabled) focus();
});
</script>

<template>
  <div class="searchbar">
    <div class="box" :class="{ busy: props.searching }" @click="emit('interact')">
      <button class="mode" :class="{ open: modeOpen }" @click.stop="modeOpen = !modeOpen">
        <span class="mdot" />
        <span class="mtext">{{ props.mode.label }}</span>
        <Icon name="chevron" :size="12" class="chev" />
      </button>

      <div class="inputwrap">
        <Icon name="search" :size="17" class="sicon" />
        <input
          ref="inputEl"
          :value="props.modelValue"
          :placeholder="props.placeholder || props.mode.hint"
          spellcheck="false"
          autocomplete="off"
          :disabled="props.disabled"
          @input="onInput"
          @keydown="emit('interact')"
          @focus="emit('focus')"
          @blur="emit('blur')"
          @keydown.enter.prevent="emit('submit')"
          @keydown.esc.prevent.stop="emit('escape')"
        />
        <span v-if="props.searching" class="spinner" />
        <button v-else-if="props.modelValue" class="clear" title="清空" @click="emit('update:modelValue', ''); focus()">
          <Icon name="x" :size="13" />
        </button>
      </div>

      <div class="sep" />

      <button
        class="chip"
        :class="{ on: !!props.scope }"
        :title="props.scope ? `搜索范围：${props.scope}` : '限定搜索目录'"
        @click.stop="props.scope ? emit('clear-scope') : emit('pick-scope')"
      >
        <Icon name="folder" :size="13" />
        <span class="chiptext">{{ scopeName }}</span>
      </button>

      <button class="icon-btn" title="设置" @click="emit('open-settings')">
        <Icon name="sliders" :size="16" />
      </button>
    </div>

    <!-- 分类标签：每种分类一个 tag，点 × 只清这一类 -->
    <div v-if="props.tags && props.tags.length" class="tags">
      <span v-for="t in props.tags" :key="t.kind" class="ftag" :class="t.kind">
        <b>{{ t.label }}</b>
        <span class="ftxt">{{ t.text }}</span>
        <i v-if="t.count > 1" class="fmore">+{{ t.count - 1 }}</i>
        <button class="fx" :title="`清除${t.label}筛选`" @click="emit('clear-tag', t.kind)">
          <Icon name="x" :size="10" />
        </button>
      </span>
      <button class="fclear" @click="emit('clear-tags')">清除筛选</button>
    </div>

    <!-- 模式菜单 -->
    <div v-if="modeOpen" class="backdrop" @click="closeAll" />
    <transition name="pop">
      <div v-if="modeOpen" class="pop-card pop mode-pop">
        <div class="pop-head">搜索模式</div>
        <button
          v-for="m in modes.slice(0, moreModes ? modes.length : 5)"
          :key="m.key"
          class="mode-item"
          :class="{ sel: m.key === props.mode.key }"
          @click="pickMode(m)"
        >
          <div class="mi-main">
            <div class="mi-title">
              {{ m.label }}
              <span v-if="m.custom" class="tag">自定义</span>
            </div>
            <div class="mi-hint">{{ m.hint }}</div>
          </div>
          <Icon v-if="m.key === props.mode.key" name="check" :size="14" class="tick" />
        </button>
        <button v-if="modes.length > 5 && !moreModes" class="more" @click="moreModes = true">
          更多模式…
        </button>
        <div class="pop-foot">
          提示：<b>空格</b> = 且，<b>|</b> = 或；拼音首拼 <b>wd</b> 可命中「文档」
        </div>
      </div>
    </transition>

  </div>
</template>

<style scoped>
.searchbar {
  position: relative;
  padding: 0 12px 10px;
  flex: none;
  z-index: 20;
}

.box {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 50px;
  padding: 0 8px 0 8px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow);
  transition: border-color 0.16s, box-shadow 0.16s;
}

.box:focus-within {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow);
}

.mode {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 9px;
  border-radius: 9px;
  background: var(--accent-soft);
  color: var(--accent);
  font-size: 12.5px;
  font-weight: 600;
  transition: background 0.14s;
  flex: none;
}
.mode:hover {
  background: var(--accent-soft-2);
}
.mdot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}
.chev {
  opacity: 0.7;
}

.inputwrap {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  padding-left: 6px;
}

.sicon {
  color: var(--text-3);
  flex: none;
}

input {
  flex: 1;
  min-width: 0;
  border: none;
  outline: none;
  background: transparent;
  font-size: 15.5px;
  letter-spacing: 0.01em;
  padding: 0;
}
input::placeholder {
  color: var(--text-3);
  font-size: 13px;
}

.clear {
  width: 22px;
  height: 22px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  color: var(--text-3);
  flex: none;
}
.clear:hover {
  background: var(--hover-strong);
  color: var(--text);
}

.spinner {
  width: 15px;
  height: 15px;
  border-radius: 50%;
  border: 2px solid var(--accent-soft-2);
  border-top-color: var(--accent);
  animation: spin 0.6s linear infinite;
  flex: none;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.sep {
  width: 1px;
  height: 22px;
  background: var(--border);
  margin: 0 2px;
  flex: none;
}

.chip {
  display: flex;
  align-items: center;
  gap: 5px;
  height: 30px;
  padding: 0 8px;
  border-radius: 8px;
  color: var(--text-2);
  font-size: 12px;
  transition: background 0.12s, color 0.12s;
  max-width: 160px;
  flex: none;
}
.chip:hover {
  background: var(--hover);
  color: var(--text);
}
.chip.on {
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}
.chiptext {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tags {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  padding: 7px 2px 0;
}
.ftag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 22px;
  padding: 0 3px 0 8px;
  border-radius: 999px;
  background: var(--accent-soft);
  border: 1px solid color-mix(in srgb, var(--accent) 22%, transparent);
  color: var(--accent);
  font-size: 11.5px;
  max-width: 280px;
}
.ftag b {
  font-weight: 600;
  font-size: 10.5px;
  opacity: 0.72;
}
.ftxt {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 600;
}
.fmore {
  font-style: normal;
  font-size: 10.5px;
  opacity: 0.7;
}
.fx {
  width: 16px;
  height: 16px;
  display: grid;
  place-items: center;
  border-radius: 50%;
  color: inherit;
  opacity: 0.6;
  flex: none;
}
.fx:hover {
  background: var(--accent-soft-2);
  opacity: 1;
}
.fclear {
  font-size: 11px;
  color: var(--text-3);
  padding: 2px 7px;
  border-radius: 6px;
}
.fclear:hover {
  color: var(--danger);
  background: var(--hover);
}

.backdrop {
  position: fixed;
  inset: 0;
  z-index: 40;
}

.pop {
  position: absolute;
  top: 52px;
  z-index: 41;
}

.mode-pop {
  left: 12px;
  width: 330px;
  padding: 6px;
}

.pop-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 11px;
  font-weight: 600;
  color: var(--text-3);
  letter-spacing: 0.06em;
  padding: 6px 8px 8px;
}

.link {
  color: var(--accent);
  font-size: 11.5px;
  font-weight: 500;
}
.link:hover {
  text-decoration: underline;
}

.mode-item {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 9px;
  border-radius: 8px;
  text-align: left;
}
.mode-item:hover {
  background: var(--hover);
}
.mode-item.sel {
  background: var(--accent-soft);
}
.mi-main {
  flex: 1;
  min-width: 0;
}
.mi-title {
  font-size: 13px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 6px;
}
.mi-hint {
  font-size: 11.5px;
  color: var(--text-3);
  margin-top: 2px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-family: var(--font-mono);
}
.tag {
  font-size: 10px;
  font-weight: 500;
  padding: 1px 4px;
  border-radius: 4px;
  background: var(--accent-soft-2);
  color: var(--accent);
}
.tick {
  color: var(--accent);
  flex: none;
}
.more {
  width: 100%;
  padding: 7px;
  font-size: 12px;
  color: var(--text-2);
  border-radius: 8px;
}
.more:hover {
  background: var(--hover);
}

.pop-foot {
  margin-top: 4px;
  padding: 8px;
  font-size: 11px;
  color: var(--text-3);
  border-top: 1px solid var(--border);
  line-height: 1.5;
}
.pop-foot b {
  color: var(--text-2);
  font-weight: 600;
}

.pop-enter-active,
.pop-leave-active {
  transition: opacity 0.12s ease, transform 0.12s ease;
}
.pop-enter-from,
.pop-leave-to {
  opacity: 0;
  transform: translateY(-6px) scale(0.99);
}
</style>
