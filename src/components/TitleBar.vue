<script setup lang="ts">
import Icon from "./Icon.vue";

const props = defineProps<{
  pinned: boolean;
  maximized: boolean;
  /** 只在「没索引 + 没管理员权限」时为真：此时搜索无内容可用，需要提示 */
  warn: boolean;
  engineVersion: string;
}>();
const emit = defineEmits<{
  (e: "pin"): void;
  (e: "minimize"): void;
  (e: "maximize"): void;
  (e: "close"): void;
}>();
</script>

<template>
  <div class="titlebar" data-tauri-drag-region @dblclick="emit('maximize')">
    <div class="brand" data-tauri-drag-region>
      <span class="logo">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none">
          <circle cx="10.5" cy="10.5" r="6.2" stroke="currentColor" stroke-width="2.1" />
          <path d="m15.4 15.4 4.2 4.2" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" />
        </svg>
      </span>
      <span class="name">tinker搜索</span>
      <span class="ver">Rust · {{ props.engineVersion || "—" }}</span>
      <span v-if="props.warn" class="warn" title="还没有索引：建库需要管理员权限">待建索引</span>
    </div>

    <div class="actions">
      <button
        class="icon-btn"
        :class="{ active: props.pinned }"
        title="窗口置顶"
        @click.stop="emit('pin')"
      >
        <Icon name="pin" :size="14" />
      </button>
      <button class="icon-btn" title="最小化" @click.stop="emit('minimize')">
        <Icon name="minus" :size="14" />
      </button>
      <button class="icon-btn" title="最大化" @click.stop="emit('maximize')">
        <Icon :name="props.maximized ? 'restore' : 'maximize'" :size="13" />
      </button>
      <button class="icon-btn close" title="收进托盘" @click.stop="emit('close')">
        <Icon name="x" :size="14" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.titlebar {
  height: var(--title-h);
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 6px 0 12px;
  flex: none;
  position: relative;
  z-index: 30;
}

.brand {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.logo {
  width: 21px;
  height: 21px;
  border-radius: 7px;
  display: grid;
  place-items: center;
  color: #fff;
  background: linear-gradient(140deg, var(--accent-2), var(--accent));
  box-shadow: 0 2px 8px var(--accent-soft-2);
}

.name {
  font-size: 12.5px;
  font-weight: 650;
  letter-spacing: 0.02em;
}

.ver {
  font-size: 11px;
  color: var(--text-3);
  padding-left: 2px;
}

.warn {
  font-size: 10.5px;
  color: #b25b00;
  background: rgba(255, 170, 0, 0.16);
  border-radius: 5px;
  padding: 1px 5px;
}

.actions {
  display: flex;
  align-items: center;
  gap: 1px;
}

.close:hover {
  background: #e5484d;
  color: #fff;
}
</style>
