// 发布一个插件版本到本地市场目录。
//
//   node publish.mjs ../src-tauri/assets/plugins/tinker.nl-zh.json --base http://127.0.0.1:8787
//   node publish.mjs ./my-plugin.zip --base https://plugins.example.com
//
// 做的事：校验清单 → 复制到 public/plugins/ → 算 sha256 → 合并进 public/index.json。
// 同一个 id 已存在时覆盖该条（版本号升上去，客户端就会显示「可更新」）。
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const PUBLIC = join(HERE, "public");

const argv = process.argv.slice(2);
const src = argv.find((a) => !a.startsWith("--"));
const argOf = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i > -1 && argv[i + 1] ? argv[i + 1] : fallback;
};

if (!src) {
  console.error("用法：node publish.mjs <plugin.json|plugin.zip> [--base http://host:port]");
  process.exit(1);
}

const base = argOf("base", "http://127.0.0.1:8787").replace(/\/+$/, "");
const buf = await readFile(src);
const sha256 = createHash("sha256").update(buf).digest("hex");

let entry;
let fileName;

if (extname(src).toLowerCase() === ".zip") {
  // 压缩包形式：包名就是 id-version.zip，需要索引里已有同名条目来补元数据
  fileName = basename(src);
  const m = fileName.match(/^(.+)-(\d+\.\d+\.\d+)\.zip$/);
  if (!m) {
    console.error("压缩包命名必须是 <id>-<version>.zip，例如 tinker.nl-zh-1.0.0.zip");
    process.exit(1);
  }
  entry = { id: m[1], version: m[2] };
} else {
  const manifest = JSON.parse(buf.toString("utf8"));
  for (const k of ["id", "name", "version", "type"]) {
    if (!manifest[k]) {
      console.error(`插件清单缺少字段 ${k}`);
      process.exit(1);
    }
  }
  fileName = `${manifest.id}-${manifest.version}.json`;
  entry = {
    id: manifest.id,
    name: manifest.name,
    version: manifest.version,
    type: manifest.type,
    description: manifest.description || "",
    author: manifest.author || "",
    min_app: manifest.min_app || "1.0.0",
    changelog: argOf("changelog", ""),
  };
}

await mkdir(join(PUBLIC, "plugins"), { recursive: true });
await writeFile(join(PUBLIC, "plugins", fileName), buf);

const indexPath = join(PUBLIC, "index.json");
let index = { updated: "", plugins: [] };
try {
  index = JSON.parse(await readFile(indexPath, "utf8"));
} catch {
  // 首次发布，索引还不存在
}

const item = {
  ...entry,
  download: `${base}/plugins/${fileName}`,
  sha256,
  size: buf.length,
};
const at = index.plugins.findIndex((p) => p.id === item.id);
if (at > -1) index.plugins[at] = { ...index.plugins[at], ...item };
else index.plugins.push(item);

index.updated = new Date().toISOString().slice(0, 10);
await writeFile(indexPath, JSON.stringify(index, null, 2) + "\n");

console.log(`已发布 ${item.id} v${item.version}`);
console.log(`  文件 ${fileName}（${buf.length} B）`);
console.log(`  sha256 ${sha256}`);
console.log(`  索引 ${indexPath}`);
console.log(`  地址 ${item.download}`);
console.log(`\n填到客户端「设置 → 插件 → 市场源」： ${base}/index.json`);
