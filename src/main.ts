import { createApp } from "vue";
import ElementPlus from "element-plus";
import zhCn from "element-plus/es/locale/lang/zh-cn";
import "element-plus/dist/index.css";
import "element-plus/theme-chalk/dark/css-vars.css";
import "./styles/app.css";
import App from "./App.vue";

const app = createApp(App);
app.use(ElementPlus, { locale: zhCn });
app.mount("#app");

// 禁用浏览器默认的右键菜单与拖拽打开文件（窗口是一个桌面应用）
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement;
  if (!t.closest("input, textarea, .md, pre, td")) e.preventDefault();
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => e.preventDefault());
