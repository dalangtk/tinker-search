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
        self.pool.len()
            + self.parent.len() * 4
            + self.name_off.len() * 4
            + self.name_len.len() * 2
            + self.vol.len() * 2
            + self.flags.len()
            + self.frn.len() * 8
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

    pub fn load(path: &Path) -> io::Result<Index> {
        let mut buf = Vec::new();
        File::open(path)?.read_to_end(&mut buf)?;
        Index::from_bytes(&buf)
    }

    pub fn from_bytes(buf: &[u8]) -> io::Result<Index> {
        if buf.len() < 16 || &buf[0..8] != MAGIC {
            return Err(bad("魔数不匹配，不是本内核的索引库"));
        }
        let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        if version != VERSION {
            return Err(bad("索引库版本不支持"));
        }
        let vcount = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
        let mut p = 16usize;
        let mut volumes = Vec::with_capacity(vcount);
        for _ in 0..vcount {
            let l = u16::from_le_bytes(need(buf, &mut p, 2)?.try_into().unwrap()) as usize;
            let s = need(buf, &mut p, l)?;
            volumes.push(String::from_utf8_lossy(s).into_owned());
        }
        let n = u64::from_le_bytes(need(buf, &mut p, 8)?.try_into().unwrap()) as usize;
        let pool_len = u64::from_le_bytes(need(buf, &mut p, 8)?.try_into().unwrap()) as usize;
        let wasted = u64::from_le_bytes(need(buf, &mut p, 8)?.try_into().unwrap());

        let parent = read_u32s(buf, &mut p, n)?;
        let name_off = read_u32s(buf, &mut p, n)?;
        let name_len = read_u16s(buf, &mut p, n)?;
        let vol = read_u16s(buf, &mut p, n)?;
        let flags = need(buf, &mut p, n)?.to_vec();
        let frn = read_u64s(buf, &mut p, n)?;
        let pool = need(buf, &mut p, pool_len)?.to_vec();
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

fn need<'a>(buf: &'a [u8], p: &mut usize, n: usize) -> io::Result<&'a [u8]> {
    if *p + n > buf.len() {
        return Err(bad("索引库被截断"));
    }
    let s = &buf[*p..*p + n];
    *p += n;
    Ok(s)
}

fn write_u32s<W: Write>(w: &mut W, v: &[u32]) -> io::Result<()> {
    let mut tmp = Vec::with_capacity(v.len() * 4);
    for x in v {
        tmp.extend_from_slice(&x.to_le_bytes());
    }
    w.write_all(&tmp)
}

fn write_u16s<W: Write>(w: &mut W, v: &[u16]) -> io::Result<()> {
    let mut tmp = Vec::with_capacity(v.len() * 2);
    for x in v {
        tmp.extend_from_slice(&x.to_le_bytes());
    }
    w.write_all(&tmp)
}

fn write_u64s<W: Write>(w: &mut W, v: &[u64]) -> io::Result<()> {
    let mut tmp = Vec::with_capacity(v.len() * 8);
    for x in v {
        tmp.extend_from_slice(&x.to_le_bytes());
    }
    w.write_all(&tmp)
}

fn read_u64s(buf: &[u8], p: &mut usize, n: usize) -> io::Result<Vec<u64>> {
    let s = need(buf, p, n * 8)?;
    Ok(s.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect())
}

fn read_u32s(buf: &[u8], p: &mut usize, n: usize) -> io::Result<Vec<u32>> {
    let s = need(buf, p, n * 4)?;
    Ok(s.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
}

fn read_u16s(buf: &[u8], p: &mut usize, n: usize) -> io::Result<Vec<u16>> {
    let s = need(buf, p, n * 2)?;
    Ok(s.chunks_exact(2).map(|c| u16::from_le_bytes(c.try_into().unwrap())).collect())
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
