# 插件发布服务

一个零依赖的 Node 服务，用来托管 tinker 的插件市场。发布一个插件版本只要一条命令，客户端把地址填进「设置 → 插件 → 市场源」就能看到安装、更新。

## 快速开始

```bash
cd market
node publish.mjs ../src-tauri/assets/plugins/tinker.nl-zh.json --base http://127.0.0.1:8787
node server.mjs
```

打开 <http://127.0.0.1:8787/>，把页面上的 `http://127.0.0.1:8787/index.json` 填到客户端「设置 → 插件 → 市场源」。

要求 Node 18+，没有任何 npm 依赖。

## 目录结构

```
market/
├── publish.mjs          # 发布 CLI：校验清单 → 复制 → 算 sha256 → 合并索引
├── server.mjs           # 静态托管（CORS 全开，只读，防目录穿越）
└── public/
    ├── index.json       # 市场索引，客户端拉的就是它
    └── plugins/
        └── tinker.nl-zh-1.0.0.json
```

## 命令

| 命令 | 说明 |
| --- | --- |
| `node server.mjs` | 默认 `127.0.0.1:8787` |
| `node server.mjs --port 9000 --host 0.0.0.0` | 换端口 / 对外监听 |
| `node publish.mjs <plugin.json> --base <url>` | 发布清单式插件 |
| `node publish.mjs <id>-<ver>.zip --base <url>` | 发布打包式插件 |
| `--changelog "..."` | 写进索引的更新说明，客户端会显示 |

`--base` 是**客户端能访问到的地址**，会拼进 `download` 字段。本机自测填 `http://127.0.0.1:8787`；正式发布填公网域名。

## 索引格式

```json
{
  "updated": "2026-11-05",
  "plugins": [
    {
      "id": "tinker.nl-zh",
      "name": "自然语言搜索（中文）",
      "version": "1.0.1",
      "type": "nlu",
      "description": "……",
      "author": "tinker",
      "min_app": "1.0.0",
      "changelog": "补了「季度 / 上半年」这类说法",
      "download": "http://127.0.0.1:8787/plugins/tinker.nl-zh-1.0.1.json",
      "sha256": "12765b47…",
      "size": 4322
    }
  ]
}
```

客户端按 `id` 匹配本地已安装插件：`version` 比本地新就显示「可更新」。同一个 id 重复发布会覆盖索引里的那一条，不会出现两条。

## 版本升级流程

1. 改插件清单里的 `version`（例如 `1.0.0` → `1.0.1`）
2. `node publish.mjs <plugin.json> --base <url> --changelog "这次改了什么"`
3. 客户端「设置 → 插件 → 刷新」→ 卡片上出现「可更新 1.0.1」→ 点更新

## 安全边界

插件是**纯数据包**（一个 JSON 清单：词表 + 引擎配置），宿主不存在“执行插件代码”的路径，所以恶意插件能做的事仅限于「把搜索结果翻译得不准」，无法读文件、执行命令或联网。

服务端本身也是只读的：只处理 `GET`/`HEAD`，路径归一化后必须落在 `public/` 内（`/../server.mjs` 返回 404），不写任何文件。要放到公网建议前面再套一层 Nginx / Caddy 做 TLS 与限流。

## 用自己的 CDN / 对象存储

不需要跑这个服务也行：`public/` 是一堆静态文件，直接整目录扔到对象存储或 Pages 上，把 `--base` 换成对应域名即可。客户端只认 `index.json` 和其中的 `download` 地址。
