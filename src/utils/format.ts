export function formatSize(n: number): string {
  if (n < 0) return "";
  if (n === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const fixed = v >= 100 || i === 0 ? 0 : v >= 10 ? 1 : 2;
  return `${v.toFixed(fixed)} ${units[i]}`;
}

const pad = (n: number) => (n < 10 ? `0${n}` : `${n}`);

export function formatTime(ms: number): string {
  if (!ms || ms <= 0) return "";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(
    d.getHours()
  )}:${pad(d.getMinutes())}`;
}

export function formatTimeShort(ms: number): string {
  if (!ms || ms <= 0) return "";
  const d = new Date(ms);
  return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(
    d.getHours()
  )}:${pad(d.getMinutes())}`;
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}

export interface Seg {
  t: string;
  hit: boolean;
}

/** 把关键词命中区间切成分段，供高亮渲染；不区分大小写，长词优先 */
export function highlight(text: string, keywords: string[]): Seg[] {
  if (!text) return [];
  const ks = (keywords || [])
    .map((k) => k.trim())
    .filter((k) => k.length > 0)
    .sort((a, b) => b.length - a.length);
  if (!ks.length) return [{ t: text, hit: false }];

  const lower = text.toLowerCase();
  const marks = new Array(text.length).fill(false);
  for (const k of ks) {
    const lk = k.toLowerCase();
    let from = 0;
    while (true) {
      const idx = lower.indexOf(lk, from);
      if (idx < 0) break;
      for (let i = idx; i < idx + lk.length && i < marks.length; i++) marks[i] = true;
      from = idx + lk.length;
    }
  }
  const out: Seg[] = [];
  let cur = "";
  let curHit = marks[0];
  for (let i = 0; i < text.length; i++) {
    if (marks[i] === curHit) {
      cur += text[i];
    } else {
      out.push({ t: cur, hit: curHit });
      cur = text[i];
      curHit = marks[i];
    }
  }
  if (cur) out.push({ t: cur, hit: curHit });
  return out;
}

const KIND_MAP: Record<string, string> = {
  jpg: "image",
  jpeg: "image",
  png: "image",
  gif: "image",
  bmp: "image",
  webp: "image",
  ico: "image",
  svg: "image",
  tif: "image",
  tiff: "image",
  psd: "image",
  raw: "image",
  mp3: "audio",
  wav: "audio",
  flac: "audio",
  ape: "audio",
  m4a: "audio",
  ogg: "audio",
  aac: "audio",
  mp4: "video",
  mkv: "video",
  avi: "video",
  mov: "video",
  wmv: "video",
  flv: "video",
  webm: "video",
  ts: "video",
  m2ts: "video",
  zip: "archive",
  rar: "archive",
  "7z": "archive",
  tar: "archive",
  gz: "archive",
  iso: "archive",
  txt: "text",
  md: "text",
  log: "text",
  json: "code",
  js: "code",
  vue: "code",
  html: "code",
  css: "code",
  py: "code",
  rs: "code",
  c: "code",
  cpp: "code",
  h: "code",
  java: "code",
  go: "code",
  sql: "code",
  exe: "app",
  bat: "app",
  cmd: "app",
  msi: "app",
  lnk: "app",
  pdf: "pdf",
  doc: "doc",
  docx: "doc",
  xls: "sheet",
  xlsx: "sheet",
  csv: "sheet",
  ppt: "slide",
  pptx: "slide",
};

export function kindOf(ext: string, isDir: boolean): string {
  if (isDir) return "folder";
  return KIND_MAP[(ext || "").toLowerCase()] || "file";
}

export function extLabel(row: { ext: string; is_dir: boolean; file_type: string }): string {
  if (row.is_dir) return "文件夹";
  if (row.ext) return row.ext.toUpperCase() + " 文件";
  return row.file_type || "文件";
}

export function fileNameOf(path: string): string {
  const i = path.lastIndexOf("\\");
  return i >= 0 ? path.slice(i + 1) : path;
}

export function dirOf(path: string): string {
  const i = path.lastIndexOf("\\");
  return i > 0 ? path.slice(0, i) : path;
}

export function isImage(ext: string): boolean {
  return ["jpg", "jpeg", "png", "gif", "bmp", "webp", "ico", "svg", "tif", "tiff"].includes(
    (ext || "").toLowerCase()
  );
}

export function isText(ext: string): boolean {
  return [
    "txt",
    "md",
    "log",
    "json",
    "xml",
    "ini",
    "yml",
    "yaml",
    "js",
    "ts",
    "vue",
    "html",
    "css",
    "scss",
    "rs",
    "c",
    "cpp",
    "h",
    "hpp",
    "py",
    "java",
    "go",
    "cs",
    "sql",
    "sh",
    "bat",
    "cmd",
    "conf",
    "toml",
    "lua",
    "csv",
  ].includes((ext || "").toLowerCase());
}
