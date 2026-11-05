<script setup lang="ts">
import { computed, onUnmounted, reactive, ref, watch } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { open } from "@tauri-apps/plugin-dialog";
import Icon from "./Icon.vue";
import { api } from "../api";
import { formatSize, formatCount } from "../utils/format";
import type { AppConfig, IndexStats } from "../types";

const props = defineProps<{
  visible: boolean;
  config: AppConfig;
  stats: IndexStats | null;
  admin: boolean;
  initialTab?: string;
  appInfo: any;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  (e: "save", cfg: AppConfig): void;
  (e: "theme", name: string): void;
  (e: "refresh-stats"): void;
}>();

const tab = ref("search");
const draft = reactive<AppConfig>(JSON.parse(JSON.stringify(props.config)));

watch(
  () => props.visible,
  (v) => {
    if (v) {
      Object.assign(draft, JSON.parse(JSON.stringify(props.config)));
      tab.value = props.initialTab || "search";
      void loadIndexStuff();
      void loadLastError();
      // 协议开关以注册表实际状态为准，不用配置值猜
      api
        .protocolRegistered()
        .then((on) => (protocolOn.value = !!on))
        .catch(() => {});
      api
        .hotkeyHookOk()
        .then((ok) => (hookOk.value = !!ok))
        .catch(() => {});
    } else {
      stopRecording();
    }
  }
);

const drives = ref<any[]>([]);
const excluded = ref<string[]>([]);
const perf = ref<any>({});
const scanning = ref(false);
const protocolOn = ref(false);
const reindexDrives = ref<string[]>([]);
const showPerf = ref(false);
/** 引擎最近一次错误，仅在「关于」页做诊断展示 */
const lastErr = ref("");
/** 全局呼出快捷键的键盘钩子是否真的装上了 */
const hookOk = ref<boolean | null>(null);

// ---- 自定义全局呼出快捷键 ----
const HOTKEY_PRESETS = [
  { label: "双击 Ctrl", value: "DoubleCtrl" },
  { label: "双击 Alt", value: "DoubleAlt" },
  { label: "双击 Shift", value: "DoubleShift" },
  { label: "双击 Win", value: "DoubleWin" },
  { label: "Ctrl + Alt + S", value: "Ctrl+Alt+S" },
  { label: "Ctrl + Shift + F", value: "Ctrl+Shift+F" },
  { label: "Alt + Space", value: "Alt+Space" },
  { label: "Win + Q", value: "Win+Q" },
];
const recording = ref(false);
const hotkeyError = ref("");

function shortcutLabel(spec: string) {
  const s = spec || "";
  if (!s || s.toLowerCase() === "none") return "未启用";
  const hit = HOTKEY_PRESETS.find((p) => p.value.toLowerCase() === s.toLowerCase());
  return hit ? hit.label : s.replace(/\+/g, " + ");
}

/** 用户录出预设之外的值时，也让下拉框能正确显示它 */
const hotkeyOptions = computed(() => {
  const cur = draft.window.shortcut || "";
  const list = HOTKEY_PRESETS.slice();
  if (cur && !list.some((p) => p.value.toLowerCase() === cur.toLowerCase())) {
    list.unshift({ label: shortcutLabel(cur), value: cur });
  }
  return list;
});

/** KeyboardEvent.code → 后端 hotkey::key_vk 认识的键名 */
function recordKeyName(e: KeyboardEvent): string {
  const c = e.code;
  if (/^Key[A-Z]$/.test(c)) return c.slice(3);
  if (/^Digit[0-9]$/.test(c)) return c.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(c)) return c;
  if (/^Numpad[0-9]$/.test(c)) return c;
  const map: Record<string, string> = {
    Space: "Space", Tab: "Tab", Enter: "Enter", NumpadEnter: "Enter",
    Escape: "Escape", Backspace: "Backspace", Delete: "Delete", Insert: "Insert",
    Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown",
    ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
    Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]",
    Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/",
    Backslash: "\\", Backquote: "`", PrintScreen: "PrintScreen",
    ScrollLock: "ScrollLock", Pause: "Pause",
  };
  return map[c] || "";
}

function onRecordKey(e: KeyboardEvent) {
  if (!recording.value) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
    stopRecording();
    return;
  }
  // 只按下修饰键时继续等主键
  if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Win");
  if (!mods.length) {
    hotkeyError.value = "必须带至少一个修饰键（Ctrl / Alt / Shift / Win）：裸键会把该键从整个系统拦下来";
    return;
  }
  const main = recordKeyName(e);
  if (!main) {
    hotkeyError.value = `暂不支持该按键（${e.code}），请换一个`;
    return;
  }
  draft.window.shortcut = [...mods, main].join("+");
  hotkeyError.value = "";
  stopRecording();
}

function startRecording() {
  recording.value = true;
  hotkeyError.value = "";
  window.addEventListener("keydown", onRecordKey, true);
}
function stopRecording() {
  recording.value = false;
  window.removeEventListener("keydown", onRecordKey, true);
}
onUnmounted(stopRecording);

async function loadLastError() {
  try {
    lastErr.value = (await api.lastError()) || "";
  } catch {
    lastErr.value = "";
  }
}

async function loadIndexStuff() {
  try {
    drives.value = await api.drives();
    excluded.value = await api.excluded();
    if (!reindexDrives.value.length) {
      reindexDrives.value = drives.value
        .filter((d) => d.盘类型 === 3)
        .map((d) => d.盘符);
    }
  } catch (e) {
    /* 索引未就绪时忽略 */
  }
}

const TABS = [
  { key: "search", label: "搜索匹配", icon: "search" },
  { key: "index", label: "索引管理", icon: "database" },
  { key: "appearance", label: "外观与窗口", icon: "eye" },
  { key: "about", label: "关于", icon: "info" },
];

const THEMES = [
  { key: "default", label: "靛蓝", color: "#5a5bd6" },
  { key: "dark", label: "暗夜", color: "#1b1c22" },
  { key: "sakura", label: "樱花", color: "#d6558b" },
  { key: "mint", label: "薄荷", color: "#17936a" },
  { key: "cyber", label: "赛博", color: "#26d0ff" },
  { key: "glass", label: "玻璃", color: "#4d6bfe" },
];

const stats = computed(() => props.stats);

function pickTheme(name: string) {
  draft.theme = name;
  emit("theme", name);
}

function close() {
  emit("close");
}

function save() {
  emit("save", JSON.parse(JSON.stringify(draft)));
}

async function rebuild() {
  // 建库要读 MFT，非管理员下引擎不会真的遍历（返回成功但线程立刻退出），
  // 所以这里先选择提权，避免"点了没反应"。
  if (props.stats && !props.stats.admin) {
    try {
      await ElMessageBox.confirm(
        "建立索引需要读取 NTFS 主文件表，必须以管理员权限运行。搜索本身不需要提权。\n是否以管理员身份重新启动并建库？",
        "需要管理员权限",
        { confirmButtonText: "以管理员身份重启", cancelButtonText: "取消", type: "warning" }
      );
    } catch {
      return;
    }
    await api.elevate().catch((e) => ElMessage.error(String(e)));
    return;
  }
  try {
    await ElMessageBox.confirm(
      `将重新遍历 ${
        reindexDrives.value.length ? reindexDrives.value.join("、") : "全部固定磁盘"
      } 并重建索引，期间搜索不可用。是否继续？`,
      "重建索引",
      { confirmButtonText: "开始重建", cancelButtonText: "取消", type: "warning" }
    );
  } catch {
    return;
  }
  try {
    scanning.value = true;
    await api.startScan(reindexDrives.value);
    ElMessage.success("已开始重建索引，进度见状态栏");
    emit("refresh-stats");
  } catch (e) {
    ElMessage.error(String(e));
  } finally {
    scanning.value = false;
  }
}

async function stopScan() {
  await api.stopScan();
  ElMessage.info("已请求停止遍历");
  emit("refresh-stats");
}

async function saveDb() {
  try {
    const msg = await api.saveDb();
    ElMessage.success(msg);
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function addExclude() {
  const dir = await open({ directory: true, multiple: false, title: "选择不建立索引的目录" });
  if (typeof dir !== "string") return;
  try {
    await api.addExcluded(dir);
    excluded.value = await api.excluded();
    ElMessage.success("已排除该目录");
    emit("refresh-stats");
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function delExclude(p: string) {
  try {
    await api.removeExcluded(p);
    excluded.value = await api.excluded();
    ElMessage.success("已取消排除（重新遍历后恢复收录）");
  } catch (e) {
    ElMessage.error(String(e));
  }
}

async function togglePerf() {
  showPerf.value = !showPerf.value;
  if (showPerf.value) {
    try {
      perf.value = await api.perf();
    } catch {
      perf.value = {};
    }
  }
}

async function registerProtocol(on: boolean) {
  try {
    await api.registerProtocol(on);
    // 回读注册表实际状态，界面不做假承诺
    protocolOn.value = await api.protocolRegistered().catch(() => on);
    ElMessage.success(on ? "已注册 tinker:// 协议" : "已取消协议注册");
  } catch (e) {
    protocolOn.value = !on;
    ElMessage.error(`注册失败：${e}`);
  }
}

function openPath(kind: string) {
  api.openDir(kind).catch(() => {});
}
</script>

<template>
  <Transition name="fade">
    <div v-if="props.visible" class="mask" @click.self="close">
      <div class="dlg">
        <aside class="rail">
          <div class="rail-title">设置</div>
          <button
            v-for="t in TABS"
            :key="t.key"
            class="rail-item"
            :class="{ on: tab === t.key }"
            @click="tab = t.key"
          >
            <Icon :name="t.icon" :size="15" />
            {{ t.label }}
          </button>
          <div class="rail-foot">
            <span class="vtxt">引擎 {{ props.appInfo?.engineVersion || "—" }}</span>
          </div>
        </aside>

        <section class="main">
          <header class="mhead">
            <span>{{ TABS.find((t) => t.key === tab)?.label }}</span>
            <button class="icon-btn" @click="close"><Icon name="x" :size="15" /></button>
          </header>

          <div class="content">
            <!-- ============ 搜索匹配 ============ -->
            <div v-show="tab === 'search'" class="pane">
              <div class="group">
                <div class="gh">拼音</div>
                <label class="row">
                  <div>
                    <div class="t">支持首拼</div>
                    <div class="d">如 <code>wd</code> 命中「文档」</div>
                  </div>
                  <el-switch v-model="draft.search.pinyin_initial" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">支持全拼</div>
                    <div class="d">如 <code>wendang</code> 命中「文档」</div>
                  </div>
                  <el-switch v-model="draft.search.pinyin_full" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">拼音完整匹配</div>
                    <div class="d">整段拼音须完整匹配，而非前缀</div>
                  </div>
                  <el-switch v-model="draft.search.pinyin_exact" />
                </label>
              </div>

              <div class="group">
                <div class="gh">通配符与匹配</div>
                <label class="row">
                  <div>
                    <div class="t">支持星号 <code>*</code></div>
                    <div class="d">关闭后 <code>*</code> 按普通字符处理</div>
                  </div>
                  <el-switch v-model="draft.search.wildcard_star" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">支持问号 <code>?</code></div>
                    <div class="d">匹配单个字符</div>
                  </div>
                  <el-switch v-model="draft.search.wildcard_q" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">匹配全角字符</div>
                    <div class="d">全角字母数字与半角互相匹配</div>
                  </div>
                  <el-switch v-model="draft.search.fullwidth" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">区分大小写</div>
                    <div class="d">同时作用于通配符与正则</div>
                  </div>
                  <el-switch v-model="draft.search.case_sensitive" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">删除搜索词首尾空格</div>
                    <div class="d">避免复制粘贴带来的多余空格</div>
                  </div>
                  <el-switch v-model="draft.search.trim" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">搜索词为空时显示全部文件</div>
                    <div class="d">关闭后空搜索词不出结果</div>
                  </div>
                  <el-switch v-model="draft.search.empty_all" />
                </label>
              </div>

              <div class="group">
                <div class="gh">多关键词组合</div>
                <div class="row">
                  <div>
                    <div class="t">「且」分隔符</div>
                    <div class="d">两个词都要命中，默认空格</div>
                  </div>
                  <el-input v-model="draft.search.and_sep" size="small" style="width: 90px" />
                </div>
                <div class="row">
                  <div>
                    <div class="t">「或」分隔符</div>
                    <div class="d">任一命中即可，默认 |</div>
                  </div>
                  <el-input v-model="draft.search.or_sep" size="small" style="width: 90px" />
                </div>
              </div>
            </div>

            <!-- ============ 索引 ============ -->
            <div v-show="tab === 'index'" class="pane">
              <div class="cards">
                <div class="card">
                  <div class="cl">索引条目</div>
                  <div class="cv num">{{ formatCount(stats?.file_count || 0) }}</div>
                </div>
                <div class="card">
                  <div class="cl">内存占用</div>
                  <div class="cv num">{{ formatSize(stats?.memory_bytes || 0) }}</div>
                </div>
                <div class="card">
                  <div class="cl">磁盘库</div>
                  <div class="cv num">
                    {{ stats?.db_version ? "v" + stats.db_version : "—" }}
                  </div>
                </div>
              </div>

              <div class="group">
                <div class="gh">状态</div>
                <div class="kv">
                  <span class="k">引擎状态</span>
                  <span class="v">
                    {{
                      stats?.scanning
                        ? `正在遍历 ${Math.max(0, stats?.progress || 0).toFixed(1)}%`
                        : stats?.ready
                        ? "就绪"
                        : "未就绪"
                    }}
                    <template v-if="stats?.syncing"> · 实时监控中</template>
                    <template v-if="stats?.pending_sync"> · 待处理 {{ stats?.pending_sync }}</template>
                  </span>
                </div>
                <div class="kv">
                  <span class="k">索引文件</span>
                  <span class="v mono">{{ stats?.db_path }}</span>
                </div>
              </div>

              <div class="group">
                <div class="gh">磁盘分区（勾选后重建）</div>
                <div class="drives">
                  <label v-for="d in drives" :key="d.盘符" class="drive">
                    <el-checkbox v-model="reindexDrives" :value="d.盘符" />
                    <span class="dname">{{ d.盘符 }}</span>
                    <span class="dtype">{{ d.盘类型 === 3 ? "固定磁盘" : d.盘类型 === 2 ? "可移动" : "其它" }}</span>
                  </label>
                  <div v-if="!drives.length" class="d note">尚未建立索引，暂无分区信息。</div>
                </div>
                <div class="btnrow">
                  <button class="primary" :disabled="stats?.scanning" @click="rebuild">
                    <Icon name="refresh" :size="13" />重建索引
                  </button>
                  <button class="ghost" :disabled="!stats?.scanning" @click="stopScan">停止遍历</button>
                  <button class="ghost" @click="saveDb"><Icon name="save" :size="13" />立即保存</button>
                  <button class="ghost" @click="loadIndexStuff">刷新信息</button>
                </div>
              </div>

              <div class="group">
                <div class="gh">排除目录（不进入索引）</div>
                <div class="exlist">
                  <div v-for="p in excluded" :key="p" class="ex">
                    <Icon name="folder" :size="13" />
                    <span class="exp" :title="p">{{ p }}</span>
                    <button class="del" @click="delExclude(p)"><Icon name="x" :size="12" /></button>
                  </div>
                  <div v-if="!excluded.length" class="d note">暂无排除目录</div>
                </div>
                <button class="ghost" @click="addExclude">+ 添加排除目录</button>
              </div>

              <div class="group">
                <div class="gh">
                  引擎能力开关
                  <button class="link" @click="togglePerf">性能统计</button>
                </div>
                <label class="row">
                  <div>
                    <div class="t">目录大小级联同步</div>
                    <div class="d">目录大小 = 全部后代文件之和（遍历时重算）</div>
                  </div>
                  <el-switch v-model="draft.index.parent_size_sync" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">默认查询顺序落盘</div>
                    <div class="d">空搜索词的顺序随库保存，加载更快（库文件约 +4 字节/条）</div>
                  </div>
                  <el-switch v-model="draft.index.default_query_save" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">首次启动自动建立索引</div>
                    <div class="d">没有索引文件时自动遍历全部固定磁盘</div>
                  </div>
                  <el-switch v-model="draft.index.auto_scan_first_run" />
                </label>
                <div v-if="showPerf" class="perf">
                  <pre>{{ perf.scan || "（暂无遍历统计）" }}</pre>
                  <pre>{{ perf.load || "（暂无加载/保存统计）" }}</pre>
                  <pre v-if="perf.sync">{{ perf.sync }}</pre>
                </div>
              </div>
            </div>

            <!-- ============ 外观 ============ -->
            <div v-show="tab === 'appearance'" class="pane">
              <div class="group">
                <div class="gh">主题皮肤</div>
                <div class="themes">
                  <button
                    v-for="t in THEMES"
                    :key="t.key"
                    class="theme"
                    :class="{ on: draft.theme === t.key }"
                    @click="pickTheme(t.key)"
                  >
                    <span class="sw" :style="{ background: t.color }" />
                    <span class="tn">{{ t.label }}</span>
                    <Icon v-if="draft.theme === t.key" name="check" :size="12" />
                  </button>
                </div>
              </div>

              <div class="group">
                <div class="gh">视图</div>
                <div class="row">
                  <div><div class="t">默认视图</div><div class="d">列表 / 紧凑 / 网格</div></div>
                  <el-select v-model="draft.view.mode" size="small" style="width: 110px">
                    <el-option label="详情列表" value="list" />
                    <el-option label="紧凑列表" value="dense" />
                    <el-option label="图标网格" value="grid" />
                  </el-select>
                </div>
                <label class="row">
                  <div><div class="t">显示预览面板</div><div class="d">选中即预览图片 / 文本 / 属性</div></div>
                  <el-switch v-model="draft.view.preview" />
                </label>
                <div class="row">
                  <div><div class="t">网格图标大小</div><div class="d">100 ~ 168 px</div></div>
                  <el-slider
                    v-model="draft.view.zoom"
                    :min="100"
                    :max="168"
                    :step="8"
                    style="width: 150px"
                  />
                </div>
              </div>

              <div class="group">
                <div class="gh">窗口</div>
                <label class="row">
                  <div><div class="t">窗口置顶</div><div class="d">始终显示在最前面</div></div>
                  <el-switch v-model="draft.window.always_on_top" />
                </label>
                <label class="row">
                  <div><div class="t">失焦时隐藏</div><div class="d">像启动器一样点别处就收起</div></div>
                  <el-switch v-model="draft.window.hide_on_blur" />
                </label>
                <label class="row">
                  <div>
                    <div class="t">启用全局呼出快捷键</div>
                    <div class="d">在任意界面按下快捷键，把最小化的窗口还原到前台（不会隐藏窗口）</div>
                  </div>
                  <el-switch v-model="draft.window.double_ctrl" />
                </label>
                <div class="row">
                  <div>
                    <div class="t">呼出快捷键</div>
                    <div class="d">
                      当前：{{ shortcutLabel(draft.window.shortcut) }}
                      <span v-if="hookOk === false" class="hk-warn">（键盘钩子未安装，可能被安全软件拦截）</span>
                    </div>
                  </div>
                  <div class="hk-actions">
                    <el-select
                      v-model="draft.window.shortcut"
                      size="small"
                      style="width: 138px"
                      :disabled="!draft.window.double_ctrl"
                    >
                      <el-option
                        v-for="p in hotkeyOptions"
                        :key="p.value"
                        :label="p.label"
                        :value="p.value"
                      />
                    </el-select>
                    <el-button
                      size="small"
                      :type="recording ? 'danger' : 'default'"
                      :disabled="!draft.window.double_ctrl"
                      @click="recording ? stopRecording() : startRecording()"
                    >
                      {{ recording ? "请按键…（Esc 取消）" : "自定义" }}
                    </el-button>
                  </div>
                </div>
                <div v-if="hotkeyError" class="hk-err">{{ hotkeyError }}</div>
                <label class="row">
                  <div>
                    <div class="t">注册 tinker:// 协议</div>
                    <div class="d">浏览器与运行框可直接唤起搜索</div>
                  </div>
                  <el-switch v-model="protocolOn" @change="registerProtocol(protocolOn)" />
                </label>
              </div>
            </div>

            <!-- ============ 关于 ============ -->
            <div v-show="tab === 'about'" class="pane">
              <div class="about">
                <div class="alogo">搜</div>
                <div class="aname">tinker搜索 · Rust</div>
                <div class="adesc">
                  Rust + Tauri 2 + Vue 3 编写。搜索内核 sscore 直接用 Rust 读 NTFS
                  主文件表建列式索引，靠卷级 USN 日志做增量同步，全程在本进程内完成。
                </div>
              </div>

              <div class="group">
                <div class="kv"><span class="k">应用版本</span><span class="v num">1.0.0</span></div>
                <div class="kv">
                  <span class="k">搜索引擎</span>
                  <span class="v">内置内核（sscore，Rust 原生 + USN 实时同步）</span>
                </div>
                <div class="kv"><span class="k">引擎版本</span><span class="v num">{{ props.appInfo?.engineVersion || "—" }}</span></div>
                <div class="kv"><span class="k">库结构版本</span><span class="v num">{{ props.appInfo?.dbVersion || "—" }}</span></div>
                <div class="kv">
                  <span class="k">运行权限</span>
                  <span class="v">
                    {{
                      props.admin
                        ? "管理员（建库与实时同步可用）"
                        : "普通用户（搜索/预览/文件操作可用；建库与实时同步需管理员）"
                    }}
                    <button v-if="!props.admin" class="link inline" @click="api.elevate()">
                      以管理员重启
                    </button>
                  </span>
                </div>
                <div class="kv"><span class="k">配置目录</span><span class="v mono">{{ props.appInfo?.dataDir }}</span></div>
                <div class="kv">
                  <span class="k">全局热键</span>
                  <span class="v" :class="{ err: hookOk === false }">
                    {{
                      hookOk === null
                        ? "—"
                        : hookOk
                          ? "双击 Ctrl 已就绪"
                          : "键盘钩子未装上（可能被安全软件拦截）"
                    }}
                  </span>
                </div>
                <div class="kv">
                  <span class="k">引擎最近错误</span>
                  <span class="v mono" :class="{ err: !!lastErr && !lastErr.startsWith('OK') }">
                    {{ lastErr || "无" }}
                  </span>
                </div>
                <div class="kv">
                  <span class="k">快捷操作</span>
                  <span class="v">
                    Enter 打开 · Ctrl+Enter 定位 · F2 重命名 · Delete 删除 · Ctrl+C 复制路径 · Esc 清空
                  </span>
                </div>
              </div>

              <div class="btnrow">
                <button class="ghost" @click="openPath('config')">打开配置目录</button>
                <button class="ghost" @click="openPath('db')">打开索引目录</button>
                <button class="ghost" @click="ElMessage.info('搜索模式语法可点击搜索栏左侧模式菜单查看示例')">
                  使用说明
                </button>
              </div>
            </div>
          </div>

          <footer class="mfoot">
            <span class="fnote">设置立即生效并自动保存</span>
            <div class="fbtns">
              <button class="ghost" @click="close">取消</button>
              <button class="primary" @click="save">保存设置</button>
            </div>
          </footer>
        </section>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.mask {
  position: fixed;
  inset: 0;
  background: rgba(12, 14, 24, 0.34);
  backdrop-filter: blur(3px);
  display: grid;
  place-items: center;
  z-index: 100;
}

.dlg {
  width: min(880px, calc(100vw - 48px));
  height: min(620px, calc(100vh - 48px));
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-lg);
  display: flex;
  overflow: hidden;
}

.rail {
  width: 168px;
  flex: none;
  background: var(--panel-2);
  border-right: 1px solid var(--border);
  padding: 14px 9px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.rail-title {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.1em;
  color: var(--text-3);
  padding: 4px 10px 12px;
}
.rail-item {
  display: flex;
  align-items: center;
  gap: 9px;
  height: 34px;
  padding: 0 10px;
  border-radius: 8px;
  font-size: 12.5px;
  color: var(--text-2);
  text-align: left;
}
.rail-item:hover {
  background: var(--hover);
  color: var(--text);
}
.rail-item.on {
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}
.rail-foot {
  margin-top: auto;
  padding: 8px 10px;
}
.vtxt {
  font-size: 10.5px;
  color: var(--text-3);
  font-family: var(--font-mono);
}

.main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.mhead {
  height: 46px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 12px 0 18px;
  border-bottom: 1px solid var(--border);
  font-size: 13.5px;
  font-weight: 650;
}
.content {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 18px 20px;
}
.pane {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.group {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.gh {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 11px;
  font-weight: 700;
  color: var(--text-3);
  letter-spacing: 0.06em;
  padding: 2px 2px 7px;
}

.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 9px 10px;
  border-radius: 9px;
  cursor: default;
}
.row:hover {
  background: var(--hover);
}
.t {
  font-size: 12.5px;
  font-weight: 550;
}
.d {
  font-size: 11.5px;
  color: var(--text-3);
  margin-top: 2px;
  line-height: 1.5;
}
.d.note {
  padding: 0 10px 6px;
}
.hk-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.hk-err {
  font-size: 11.5px;
  color: #e5484d;
  padding: 0 10px 6px;
}
.hk-warn {
  color: #e5484d;
}
code {
  font-family: var(--font-mono);
  font-size: 11px;
  background: var(--panel-3);
  padding: 1px 4px;
  border-radius: 4px;
}

.cards {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
}
.card {
  background: var(--panel-2);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 11px 13px;
}
.cl {
  font-size: 11px;
  color: var(--text-3);
}
.cv {
  font-size: 17px;
  font-weight: 650;
  margin-top: 3px;
  letter-spacing: -0.01em;
}

.kv {
  display: flex;
  gap: 12px;
  padding: 6px 10px;
  font-size: 12px;
  border-radius: 7px;
}
.kv:hover {
  background: var(--hover);
}
.k {
  width: 84px;
  flex: none;
  color: var(--text-3);
  font-size: 11.5px;
}
.v {
  flex: 1;
  min-width: 0;
  color: var(--text-2);
  word-break: break-all;
}
.mono {
  font-family: var(--font-mono);
  font-size: 11px;
}
.v.err {
  color: var(--danger, #e45);
}

.drives {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.drive {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 10px;
  border-radius: 7px;
}
.drive:hover {
  background: var(--hover);
}
.dname {
  font-size: 12.5px;
  font-weight: 600;
  width: 44px;
}
.dtype {
  font-size: 11px;
  color: var(--text-3);
}

.btnrow {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  padding: 10px 2px 2px;
}

.primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 30px;
  padding: 0 14px;
  border-radius: 8px;
  background: var(--accent);
  color: #fff;
  font-size: 12.5px;
  font-weight: 600;
  box-shadow: 0 2px 8px var(--accent-soft-2);
}
.primary:hover {
  filter: brightness(1.08);
}
.primary:disabled {
  opacity: 0.5;
  box-shadow: none;
}
.ghost {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 30px;
  padding: 0 12px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  font-size: 12.5px;
}
.ghost:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.ghost:disabled {
  opacity: 0.45;
}
.link {
  font-size: 11.5px;
  color: var(--accent);
  font-weight: 500;
}
.link:hover {
  text-decoration: underline;
}
.link.inline {
  margin-left: 8px;
}

.exlist {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-bottom: 6px;
}
.ex {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border-radius: 7px;
  font-size: 12px;
  color: var(--text-2);
}
.ex:hover {
  background: var(--hover);
}
.exp {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-mono);
  font-size: 11.5px;
}
.del {
  width: 20px;
  height: 20px;
  border-radius: 5px;
  display: grid;
  place-items: center;
  color: var(--text-3);
}
.del:hover {
  background: rgba(229, 72, 77, 0.12);
  color: var(--danger);
}

.boost {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 4px 10px;
}

.themes {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
}
.theme {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 10px;
  border-radius: 9px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  font-size: 12.5px;
  color: var(--text-2);
}
.theme:hover {
  border-color: var(--border-strong);
}
.theme.on {
  border-color: var(--accent);
  color: var(--accent);
  background: var(--accent-soft);
  font-weight: 600;
}
.sw {
  width: 16px;
  height: 16px;
  border-radius: 50%;
  border: 1px solid rgba(0, 0, 0, 0.08);
}
.tn {
  flex: 1;
  text-align: left;
}

.perf {
  margin: 6px 10px 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.perf pre {
  margin: 0;
  background: var(--panel-3);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px 10px;
  font-size: 10.5px;
  line-height: 1.55;
  max-height: 180px;
  overflow: auto;
  color: var(--text-2);
  font-family: var(--font-mono);
  user-select: text;
}

.about {
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  gap: 6px;
  padding: 6px 0 12px;
}
.alogo {
  width: 54px;
  height: 54px;
  border-radius: 16px;
  display: grid;
  place-items: center;
  color: #fff;
  font-size: 22px;
  font-weight: 700;
  background: linear-gradient(140deg, var(--accent-2), var(--accent));
  box-shadow: 0 8px 22px var(--accent-soft-2);
}
.aname {
  font-size: 15px;
  font-weight: 700;
}
.adesc {
  font-size: 12px;
  color: var(--text-3);
  line-height: 1.7;
  max-width: 460px;
}
.adesc b {
  color: var(--text-2);
}

.mfoot {
  height: 52px;
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 16px;
  border-top: 1px solid var(--border);
  background: var(--panel-2);
}
.fnote {
  font-size: 11px;
  color: var(--text-3);
}
.fbtns {
  display: flex;
  gap: 8px;
}
</style>
