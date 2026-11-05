// 插件发布服务：零依赖，只做三件事 —— 托管市场索引、托管插件包、给出可直接填进客户端的地址。
//
//   node server.mjs            # 默认 http://127.0.0.1:8787
//   node server.mjs --port 9000
//
// 目录约定：
//   public/index.json                 市场索引（由 publish.mjs 生成）
//   public/plugins/<id>-<ver>.json    插件包（就是插件清单本身，纯数据）
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize, sep } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const ROOT = join(HERE, "public");

const argOf = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > -1 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
};

const PORT = Number(argOf("port", 8787));
const HOST = argOf("host", "127.0.0.1");

const TYPES = {
  ".json": "application/json; charset=utf-8",
  ".zip": "application/zip",
  ".md": "text/markdown; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
};

const INDEX_PAGE = `<!doctype html><meta charset="utf-8">
<title>tinker 插件市场</title>
<style>body{font:14px/1.7 -apple-system,"Segoe UI",sans-serif;max-width:720px;margin:48px auto;padding:0 20px;color:#222}
code{background:#f3f4f6;padding:2px 6px;border-radius:4px}a{color:#2d5bd7}
h1{font-size:20px}li{margin:6px 0}.muted{color:#888}</style>
<h1>tinker 插件市场</h1>
<p>把下面这个地址填到「设置 → 插件 → 市场源」即可：</p>
<p><code id="u">/index.json</code> <button onclick="navigator.clipboard.writeText(location.origin+document.getElementById('u').textContent)">复制</button></p>
<ul><li><a href="/index.json">index.json</a> <span class="muted">市场索引</span></li></ul>
<p class="muted">发布新版本：<code>node publish.mjs &lt;plugin.json&gt; --base <span id="b"></span></code></p>
<script>document.getElementById("b").textContent=location.origin</script>`;

const server = createServer(async (req, res) => {
  const url = new URL(req.url || "/", `http://${req.headers.host}`);
  const cors = {
    "Access-Control-Allow-Origin": "*",
    "Cache-Control": "no-cache",
  };

  if (req.method !== "GET" && req.method !== "HEAD") {
    res.writeHead(405, cors).end("method not allowed");
    return;
  }

  if (url.pathname === "/") {
    res.writeHead(200, { ...cors, "Content-Type": TYPES[".html"] }).end(INDEX_PAGE);
    console.log(`200 /`);
    return;
  }

  // 防目录穿越：把路径归一化后必须仍在 ROOT 下面
  const rel = normalize(decodeURIComponent(url.pathname)).replace(/^([/\\])+/, "");
  const file = join(ROOT, rel);
  if (!file.startsWith(ROOT + sep)) {
    res.writeHead(403, cors).end("forbidden");
    console.log(`403 ${url.pathname}`);
    return;
  }

  try {
    const buf = await readFile(file);
    res.writeHead(200, {
      ...cors,
      "Content-Type": TYPES[extname(file)] || "application/octet-stream",
      "Content-Length": buf.length,
    });
    res.end(req.method === "HEAD" ? undefined : buf);
    console.log(`200 ${url.pathname} (${buf.length} B)`);
  } catch {
    res.writeHead(404, cors).end("not found");
    console.log(`404 ${url.pathname}`);
  }
});

server.listen(PORT, HOST, () => {
  console.log(`插件市场已启动： http://${HOST}:${PORT}/index.json`);
  console.log(`目录： ${ROOT}`);
});
