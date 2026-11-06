//! 列式索引存储。
//!
//! 设计要点：
//! - 不保存完整路径，只保存 `(父记录 id, 文件名)`，取路径时沿父链现场拼装；
//! - 按"字段一列"而不是"一条记录"连续存放，名字集中在名字池里；
//! - 变长整数编码不是必需的，先用定宽换实现简单，后续可平滑替换。

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"SSCORE\x00\x02";
pub const VERSION: u32 = 2;
/// 没有父记录（磁盘根目录的直接子项）。
pub const ROOT_PARENT: u32 = u32::MAX;

pub const FLAG_DIR: u8 = 1;
/// 墓碑：文件已被（或疑似被）删除。记录本身不搬移，只打标记，
/// 这样同步线程追加新记录时不需要移动任何已有下标。
pub const FLAG_TOMB: u8 = 2;
/// 隐藏：位于回收站 / 系统目录里的记录。文件在磁盘上确实存在，
/// 但对用户来说「已经删了」，搜索、浏览、SQL 都不该把它列出来。
/// 和墓碑的区别是保留了名字与父指针，所以从回收站还原时还能原样复活。
pub const FLAG_HIDE: u8 = 4;

/// 一条记录 = 名字池里的一段字节 + 一个父指针。
#[derive(Default)]
pub struct Index {
    pub volumes: Vec<String>,
    pub parent: Vec<u32>,
    pub name_off: Vec<u32>,
    pub name_len: Vec<u16>,
    pub vol: Vec<u16>,
    pub flags: Vec<u8>,
    pub pool: Vec<u8>,
    /// 卷内唯一文件 id（NTFS 上是 FileReferenceNumber 的低 48 位）。
    /// 实时同步靠它把 USN 记录映射回索引条目；walk 建的库填 0（同步不可用）。
    pub frn: Vec<u64>,
    /// 名字池里因重命名而废弃的字节数，用于判断何时该压缩重建。
    pub wasted: u64,
}

impl Index {
    #[inline]
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    #[inline]
    pub fn name_bytes(&self, id: u32) -> &[u8] {
        let i = id as usize;
        let off = self.name_off[i] as usize;
        let len = self.name_len[i] as usize;
        &self.pool[off..off + len]
    }

    #[inline]
    pub fn name(&self, id: u32) -> &str {
        // 建库时只写入合法 UTF-8，因此这里可以免检。
        unsafe { std::str::from_utf8_unchecked(self.name_bytes(id)) }
    }

    #[inline]
    pub fn is_dir(&self, id: u32) -> bool {
        self.flags[id as usize] & FLAG_DIR != 0
    }

    #[inline]
    pub fn is_live(&self, id: u32) -> bool {
        self.flags[id as usize] & FLAG_TOMB == 0
    }

    #[inline]
    pub fn is_hidden(&self, id: u32) -> bool {
        self.flags[id as usize] & FLAG_HIDE != 0
    }

    /// 该不该出现在结果里：既没被删、也不在回收站/系统目录中。
    #[inline]
    pub fn is_visible(&self, id: u32) -> bool {
        self.flags[id as usize] & (FLAG_TOMB | FLAG_HIDE) == 0
    }

    /// 打/清隐藏位。从回收站还原文件时要用它把记录放回可见区。
    #[inline]
    pub fn set_hidden(&mut self, id: u32, on: bool) {
        let f = self.flags[id as usize];
        self.flags[id as usize] = if on { f | FLAG_HIDE } else { f & !FLAG_HIDE };
    }

    /// 已隐藏（回收站/系统目录内）的记录条数。
    pub fn hidden_count(&self) -> usize {
        self.flags.iter().filter(|f| *f & FLAG_HIDE != 0).count()
    }

    /// 目录条数（不含墓碑）。扫描一遍 flags，500 万条约 2 ms。
    pub fn dir_count(&self) -> usize {
        self.flags.iter().filter(|f| **f & FLAG_DIR != 0 && **f & FLAG_TOMB == 0).count()
    }

    /// 已删除记录的条数（墓碑）。
    pub fn tomb_count(&self) -> usize {
        self.flags.iter().filter(|f| *f & FLAG_TOMB != 0).count()
    }

    /// 索引里是否带可用的 FRN 列（只有 MFT 建的库才有）。
    pub fn has_frn(&self) -> bool {
        self.frn.len() == self.len() && self.frn.iter().any(|v| *v != 0)
    }

    /// 有效（未删除）记录条数。
    pub fn live_count(&self) -> usize {
        self.len() - self.tomb_count()
    }

    // ---------- 实时修改（同步线程用） ----------

    /// 追加一条新记录，返回它的下标。父指针先置根，由调用方回填。
    pub fn push_live(&mut self, vol: u16, frn: u64, name: &[u8], is_dir: bool) -> u32 {
        let id = self.parent.len() as u32;
        self.parent.push(ROOT_PARENT);
        self.name_off.push(self.pool.len() as u32);
        let len = name.len().min(u16::MAX as usize);
        self.name_len.push(len as u16);
        self.vol.push(vol);
        self.flags.push(if is_dir { FLAG_DIR } else { 0 });
        self.frn.push(frn);
        self.pool.extend_from_slice(&name[..len]);
        id
    }

    /// 改名 / 换父目录。名字池是只追加的，所以旧名字的字节会留成垃圾，
    /// 累计到一定程度由 `should_compact` 触发重建。
    pub fn rename(&mut self, id: u32, name: &[u8], parent: Option<u32>) {
        let i = id as usize;
        self.wasted += self.name_len[i] as u64;
        self.name_off[i] = self.pool.len() as u32;
        let len = name.len().min(u16::MAX as usize);
        self.name_len[i] = len as u16;
        self.pool.extend_from_slice(&name[..len]);
        if let Some(p) = parent {
            self.parent[i] = p;
        }
    }

    /// 打墓碑。名字段清空，路径拼装和查询都会自然跳过它。
    pub fn kill(&mut self, id: u32) {
        let i = id as usize;
        self.flags[i] |= FLAG_TOMB;
        self.wasted += self.name_len[i] as u64;
        self.name_len[i] = 0;
        self.name_off[i] = 0;
    }

    /// 复活一条墓碑记录（FRN 被复用、或文件从回收站还原）。
    pub fn revive(&mut self, id: u32, name: &[u8], parent: u32, is_dir: bool) {
        let i = id as usize;
        self.flags[i] = if is_dir { FLAG_DIR } else { 0 };
        self.rename(id, name, Some(parent));
    }

    /// 把卷根下名字命中的目录（回收站、系统卷信息）整棵子树打上隐藏位。
    ///
    /// 返回本次新隐藏的记录数。已经在隐藏区里的节点不再往下走，
    /// 既省一次遍历，也避免脏数据成环时来回绕。
    pub fn mark_hidden_subtrees(&mut self, names: &[&str]) -> usize {
        let n = self.len();
        if n == 0 || names.is_empty() {
            return 0;
        }
        let mut roots: Vec<u32> = Vec::new();
        for i in 0..n {
            if self.parent[i] != ROOT_PARENT || !self.is_live(i as u32) {
                continue;
            }
            let name = self.name(i as u32);
            if names.iter().any(|w| name.eq_ignore_ascii_case(w)) {
                roots.push(i as u32);
            }
        }
        if roots.is_empty() {
            return 0;
        }

        // CSR 子节点索引：start[i]..start[i+1] 是 i 的子节点在 children 里的区间
        let mut start = vec![0u32; n + 1];
        for &p in &self.parent {
            let pi = p as usize;
            if pi < n {
                start[pi + 1] += 1;
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut cursor = start.clone();
        let mut children = vec![0u32; start[n] as usize];
        for (i, &p) in self.parent.iter().enumerate() {
            let pi = p as usize;
            if pi < n {
                children[cursor[pi] as usize] = i as u32;
                cursor[pi] += 1;
            }
        }

        let mut stack = roots;
        let mut marked = 0usize;
        while let Some(id) = stack.pop() {
            let i = id as usize;
            if i >= n || self.is_hidden(id) || !self.is_live(id) {
                continue;
            }
            self.flags[i] |= FLAG_HIDE;
            marked += 1;
            stack.extend_from_slice(&children[start[i] as usize..start[i + 1] as usize]);
        }
        marked
    }

    /// 名字池里超过三成是垃圾时，值得整体重建一次。
    pub fn should_compact(&self) -> bool {
        let live_pool = (self.pool.len() as u64).saturating_sub(self.wasted);
        self.wasted > 8 << 20 && self.wasted * 10 > live_pool * 3
    }

    /// 沿父链拼出完整路径。父链最多回溯 512 层，防止脏数据成环。
    /// 遇到墓碑父节点立即停止——目录已被删掉，再把它的名字拼进去只会误导用户。
    pub fn path_of(&self, id: u32) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(12);
        let mut cur = id;
        let vol = self.vol[id as usize] as usize;
        for _ in 0..512 {
            if !self.is_live(cur) {
                break;
            }
            parts.push(self.name(cur));
            let p = self.parent[cur as usize];
            if p == ROOT_PARENT || p == cur {
                break;
            }
            cur = p;
        }
        let mut s = String::with_capacity(64);
        if let Some(v) = self.volumes.get(vol) {
            s.push_str(v);
        }
        for part in parts.iter().rev() {
            s.push('\\');
            s.push_str(part);
        }
        s
    }

    /// 名字池总字节数 / 记录数，用来评估每条记录的平均内存开销。
    pub fn bytes_per_record(&self) -> f64 {
        if self.len() == 0 {
            return 0.0;
        }
        let fixed = self.len() * (4 + 4 + 2 + 2 + 1 + 8);
        (fixed + self.pool.len()) as f64 / self.len() as f64
    }

    pub fn memory_bytes(&self) -> usize {
        self.memory_breakdown().iter().map(|(_, b)| *b).sum()
    }

    /// 逐列占用（字节）。顺序与物理布局一致，性能面板直接照着画占比条。
    pub fn memory_breakdown(&self) -> Vec<(&'static str, usize)> {
        vec![
            ("父指针", self.parent.len() * 4),
            ("名字偏移", self.name_off.len() * 4),
            ("名字长度", self.name_len.len() * 2),
            ("卷号", self.vol.len() * 2),
            ("标志位", self.flags.len()),
            ("文件号", self.frn.len() * 8),
            ("名字池", self.pool.len()),
        ]
    }

    // ---------- 序列化 ----------

    /// 确保卷标签存在，返回它的列号。同步新增记录时用。
    pub fn ensure_volume(&mut self, label: &str) -> u16 {
        if let Some(i) = self.volumes.iter().position(|v| v == label) {
            return i as u16;
        }
        self.volumes.push(label.to_string());
        (self.volumes.len() - 1) as u16
    }

    pub fn save(&self, path: &Path) -> io::Result<u64> {
        let f = File::create(path)?;
        let mut w = BufWriter::with_capacity(1 << 20, f);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&(self.volumes.len() as u32).to_le_bytes())?;
        for v in &self.volumes {
            let b = v.as_bytes();
            w.write_all(&(b.len() as u16).to_le_bytes())?;
            w.write_all(b)?;
        }
        let n = self.len() as u64;
        w.write_all(&n.to_le_bytes())?;
        w.write_all(&(self.pool.len() as u64).to_le_bytes())?;
        w.write_all(&self.wasted.to_le_bytes())?;
        write_u32s(&mut w, &self.parent)?;
        write_u32s(&mut w, &self.name_off)?;
        write_u16s(&mut w, &self.name_len)?;
        write_u16s(&mut w, &self.vol)?;
        w.write_all(&self.flags)?;
        write_u64s(&mut w, &self.frn)?;
        w.write_all(&self.pool)?;
        w.flush()?;
        Ok(std::fs::metadata(path)?.len())
    }

    /// 从磁盘读索引。分列流式读取：不再把整个库文件读进内存缓冲再逐列拷贝，
    /// 加载峰值从「库文件 + 列」降到「列 + 一个 64 KB 分块」。
    pub fn load(path: &Path) -> io::Result<Index> {
        let f = File::open(path)?;
        let mut r = io::BufReader::with_capacity(1 << 20, f);
        Index::from_reader(&mut r)
    }

    pub fn from_bytes(buf: &[u8]) -> io::Result<Index> {
        Index::from_reader(&mut &buf[..])
    }

    pub fn from_reader<R: Read>(r: &mut R) -> io::Result<Index> {
        let mut head = [0u8; 16];
        r.read_exact(&mut head)?;
        if &head[0..8] != MAGIC {
            return Err(bad("魔数不匹配，不是本内核的索引库"));
        }
        let version = u32::from_le_bytes(head[8..12].try_into().unwrap());
        if version != VERSION {
            return Err(bad("索引库版本不支持"));
        }
        let vcount = u32::from_le_bytes(head[12..16].try_into().unwrap()) as usize;
        let mut volumes = Vec::with_capacity(vcount);
        let mut lenbuf = [0u8; 2];
        for _ in 0..vcount {
            r.read_exact(&mut lenbuf)?;
            let l = u16::from_le_bytes(lenbuf) as usize;
            if l > 4096 {
                return Err(bad("卷标长度异常"));
            }
            let mut b = vec![0u8; l];
            r.read_exact(&mut b)?;
            volumes.push(String::from_utf8_lossy(&b).into_owned());
        }
        let mut nbuf = [0u8; 8];
        r.read_exact(&mut nbuf)?;
        let n = u64::from_le_bytes(nbuf) as usize;
        r.read_exact(&mut nbuf)?;
        let pool_len = u64::from_le_bytes(nbuf) as usize;
        r.read_exact(&mut nbuf)?;
        let wasted = u64::from_le_bytes(nbuf);

        let parent = read_u32s(r, n)?;
        let name_off = read_u32s(r, n)?;
        let name_len = read_u16s(r, n)?;
        let vol = read_u16s(r, n)?;
        let mut flags = vec![0u8; n];
        r.read_exact(&mut flags)?;
        let frn = read_u64s(r, n)?;
        let mut pool = vec![0u8; pool_len];
        r.read_exact(&mut pool)?;
        Ok(Index { volumes, parent, name_off, name_len, vol, flags, pool, frn, wasted })
    }

    /// 一致性自检：父指针越界、名字段越界都会被抓出来。
    pub fn validate(&self) -> Result<(), String> {
        let n = self.len();
        if self.frn.len() != n {
            return Err(format!("FRN 列长度 {} 与记录数 {n} 不一致", self.frn.len()));
        }
        for i in 0..n {
            let p = self.parent[i];
            if p != ROOT_PARENT && p as usize >= n {
                return Err(format!("记录 {i} 的父指针 {p} 越界"));
            }
            if !self.is_live(i as u32) {
                continue;
            }
            let off = self.name_off[i] as usize;
            let len = self.name_len[i] as usize;
            if off + len > self.pool.len() {
                return Err(format!("记录 {i} 的名字段 {off}+{len} 越界"));
            }
        }
        if !self.name_off.is_empty() && self.name_len.is_empty() {
            return Err("列长度不一致".into());
        }
        Ok(())
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

// 落盘也走定长小缓冲：整列先拼成 Vec 会在保存时凭空多占一整列内存。
const WBUF: usize = 64 * 1024;

fn write_u32s<W: Write>(w: &mut W, v: &[u32]) -> io::Result<()> {
    let mut tmp = [0u8; WBUF];
    for chunk in v.chunks(WBUF / 4) {
        for (i, x) in chunk.iter().enumerate() {
            tmp[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        }
        w.write_all(&tmp[..chunk.len() * 4])?;
    }
    Ok(())
}

fn write_u16s<W: Write>(w: &mut W, v: &[u16]) -> io::Result<()> {
    let mut tmp = [0u8; WBUF];
    for chunk in v.chunks(WBUF / 2) {
        for (i, x) in chunk.iter().enumerate() {
            tmp[i * 2..i * 2 + 2].copy_from_slice(&x.to_le_bytes());
        }
        w.write_all(&tmp[..chunk.len() * 2])?;
    }
    Ok(())
}

fn write_u64s<W: Write>(w: &mut W, v: &[u64]) -> io::Result<()> {
    let mut tmp = [0u8; WBUF];
    for chunk in v.chunks(WBUF / 8) {
        for (i, x) in chunk.iter().enumerate() {
            tmp[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
        }
        w.write_all(&tmp[..chunk.len() * 8])?;
    }
    Ok(())
}

/// 按 64 KB 分块把定宽列读进 Vec：容量一次到位，逐块解码，
/// 不额外持有整列字节缓冲。
fn read_fixed<R: Read, const W: usize, T>(
    r: &mut R,
    n: usize,
    conv: impl Fn([u8; W]) -> T,
) -> io::Result<Vec<T>> {
    const CHUNK: usize = 64 * 1024;
    let total = n.saturating_mul(W);
    let mut out: Vec<T> = Vec::with_capacity(n);
    let mut buf = [0u8; CHUNK];
    let mut done = 0usize;
    while done < total {
        let want = (total - done).min(CHUNK);
        r.read_exact(&mut buf[..want])?;
        out.extend(buf[..want].chunks_exact(W).map(|c| conv(c.try_into().unwrap())));
        done += want;
    }
    Ok(out)
}

fn read_u64s<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u64>> {
    read_fixed::<R, 8, u64>(r, n, u64::from_le_bytes)
}

fn read_u32s<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u32>> {
    read_fixed::<R, 4, u32>(r, n, u32::from_le_bytes)
}

fn read_u16s<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u16>> {
    read_fixed::<R, 2, u16>(r, n, u16::from_le_bytes)
}

/// 建库阶段的累加器：先按任意顺序 push，再回填父指针。
#[derive(Default)]
pub struct IndexBuilder {
    volumes: Vec<String>,
    parent: Vec<u32>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    vol: Vec<u16>,
    flags: Vec<u8>,
    pool: Vec<u8>,
    frn: Vec<u64>,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize, pool: usize) -> Self {
        IndexBuilder {
            volumes: Vec::new(),
            parent: Vec::with_capacity(n),
            name_off: Vec::with_capacity(n),
            name_len: Vec::with_capacity(n),
            vol: Vec::with_capacity(n),
            flags: Vec::with_capacity(n),
            pool: Vec::with_capacity(pool),
            frn: Vec::with_capacity(n),
        }
    }

    pub fn add_volume(&mut self, label: &str) -> u16 {
        if let Some(i) = self.volumes.iter().position(|v| v == label) {
            return i as u16;
        }
        self.volumes.push(label.to_string());
        (self.volumes.len() - 1) as u16
    }

    /// 追加一条记录，父指针先置空，稍后用 `set_parent` 回填。
    pub fn push(&mut self, vol: u16, name: &[u8], is_dir: bool) -> u32 {
        self.push_frn(vol, 0, name, is_dir)
    }

    /// 带 FRN 的追加。MFT 建库时用这个，实时同步才有映射依据。
    pub fn push_frn(&mut self, vol: u16, frn: u64, name: &[u8], is_dir: bool) -> u32 {
        let id = self.parent.len() as u32;
        self.parent.push(ROOT_PARENT);
        self.name_off.push(self.pool.len() as u32);
        // 超过 65535 字节的文件名在 Windows 上不可能出现，截断只为防御。
        let len = name.len().min(u16::MAX as usize);
        self.name_len.push(len as u16);
        self.vol.push(vol);
        self.flags.push(if is_dir { FLAG_DIR } else { 0 });
        self.frn.push(frn);
        self.pool.extend_from_slice(&name[..len]);
        id
    }

    pub fn push_str(&mut self, vol: u16, name: &str, is_dir: bool) -> u32 {
        self.push(vol, name.as_bytes(), is_dir)
    }

    #[inline]
    pub fn set_parent(&mut self, id: u32, parent: u32) {
        self.parent[id as usize] = parent;
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    pub fn finish(self) -> Index {
        Index {
            volumes: self.volumes,
            parent: self.parent,
            name_off: self.name_off,
            name_len: self.name_len,
            vol: self.vol,
            flags: self.flags,
            pool: self.pool,
            frn: self.frn,
            wasted: 0,
        }
    }
}
