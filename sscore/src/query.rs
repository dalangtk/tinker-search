//! 查询管线：分词 → 并行扫描名字池 → 评分 → 取前 N。
//!
//! 设计要点：
//! - 只碰内存里的名字池，不打开任何文件；
//! - 多条件（多词）先在最便宜的字段上筛，最后才算分；
//! - 拿不到候选就返回空，绝不阻塞写入方。
//!
//! 目前还没做「文件名排序索引数组」，所以前缀查询也是全量扫，
//! 后续可以在这层加二分收窄。

use crate::store::{Index, ROOT_PARENT};
use rayon::prelude::*;
use std::cell::RefCell;

/// 分类（后缀）/ 磁盘（卷）/ 目录（祖先）三类筛选条件，彼此是「且」的关系，
/// 同一类内部是「或」。这是左侧分类栏多选后落到内核的形态。
#[derive(Default, Clone, Debug)]
pub struct Filter {
    /// 允许的后缀（小写、不含点）。空 = 不限。
    pub exts: Vec<Vec<u8>>,
    /// 只保留目录（侧栏「文件夹」分类）。
    pub dirs_only: bool,
    /// 允许的卷列号。空 = 不限。
    pub vols: Vec<u16>,
    /// 祖先记录 id，命中必须位于其中任一目录之下。空 = 不限。
    pub under: Vec<u32>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty() && !self.dirs_only && self.vols.is_empty() && self.under.is_empty()
    }
}

pub struct QueryOpts {
    pub limit: usize,
    /// 只看目录 / 只看文件 / 不限。
    pub dirs: Option<bool>,
    /// 把匹配范围从「文件名」扩到「完整路径」。
    pub search_path: bool,
    /// 分类栏带来的附加条件。
    pub filter: Filter,
}

impl Default for QueryOpts {
    fn default() -> Self {
        QueryOpts { limit: 50, dirs: None, search_path: false, filter: Filter::default() }
    }
}

#[derive(Clone, Copy)]
pub struct Hit {
    pub id: u32,
    pub score: i32,
}

const SEP: u8 = b'\\';

#[inline]
fn is_sep(b: u8) -> bool {
    matches!(b, b'\\' | b'/' | b'.' | b'-' | b'_' | b' ' | b'(' | b'[' | b'@')
}

#[inline]
fn lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

/// 在 `hay` 里找 `needle`（needle 已经是小写）。返回首个命中位置。
///
/// 用 `memchr2` 先跳到首字符的两种大小写，再做短比较，
/// 比逐字节 `to_lowercase` 快一个量级。
#[inline]
pub fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let nl = needle.len();
    if nl == 0 {
        return Some(0);
    }
    if hay.len() < nl {
        return None;
    }
    let b0 = needle[0];
    let last = hay.len() - nl;
    let mut start = 0usize;
    while start <= last {
        let win = &hay[start..];
        let rel = if b0.is_ascii_lowercase() {
            memchr::memchr2(b0, b0.to_ascii_uppercase(), win)?
        } else {
            memchr::memchr(b0, win)?
        };
        let at = start + rel;
        if at > last {
            return None;
        }
        if hay[at..at + nl].iter().zip(needle).all(|(a, b)| lower(*a) == *b) {
            return Some(at);
        }
        start = at + 1;
    }
    None
}

#[inline]
fn eq_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() == needle.len() && hay.iter().zip(needle).all(|(a, b)| lower(*a) == *b)
}

#[inline]
fn starts_with_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].iter().zip(needle).all(|(a, b)| lower(*a) == *b)
}

/// 查询分词：按空白切开，各自转小写。空查询返回空表。
pub fn tokenize(q: &str) -> Vec<Vec<u8>> {
    q.split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.as_bytes().iter().map(|b| lower(*b)).collect())
        .collect()
}

/// 取文件名的后缀（小写、不含点）。
#[inline]
pub fn ext_of_bytes(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|b| *b == b'.') {
        Some(i) if i + 1 < name.len() => &name[i + 1..],
        _ => &[],
    }
}

/// 判断记录是否满足分类栏条件。
///
/// 先判最便宜的列（vol 是 u16，一次载入），再判目录位与后缀，
/// 最后才是需要沿父链走的目录范围。
#[inline]
pub fn matches_filter(idx: &Index, id: u32, f: &Filter) -> bool {
    if !f.vols.is_empty() && !f.vols.contains(&idx.vol[id as usize]) {
        return false;
    }
    let is_dir = idx.is_dir(id);
    if f.dirs_only && is_dir {
        // 「文件夹」分类命中，其它后缀条件不再需要
    } else if !f.exts.is_empty() {
        if is_dir {
            return false;
        }
        let ext = ext_of_bytes(idx.name_bytes(id));
        if !f.exts.iter().any(|e| e.as_slice() == ext) {
            return false;
        }
    } else if f.dirs_only {
        // 只选了「文件夹」而这不是目录
        return false;
    }
    if !f.under.is_empty() && !under_any(idx, id, &f.under) {
        return false;
    }
    true
}

/// `id` 是否位于 `ancestors` 中任一目录之下（含自身）。父链最多走 512 层。
pub fn under_any(idx: &Index, id: u32, ancestors: &[u32]) -> bool {
    let mut cur = id;
    for _ in 0..512 {
        if ancestors.contains(&cur) {
            return true;
        }
        let p = idx.parent[cur as usize];
        if p == ROOT_PARENT || p == cur {
            return false;
        }
        cur = p;
    }
    false
}

/// 把 `C:\Users\me\Desktop` 这样的路径解析成索引里的记录 id。
///
/// 逐段从卷根往下找，每段一次全量扫描（几毫秒）。调用方应当缓存结果，
/// 因为一次搜索只在分类栏变化时才需要重新解析。
pub fn resolve_path(idx: &Index, path: &str) -> Option<u32> {
    let p = path.trim().trim_end_matches(['\\', '/']).replace('/', "\\");
    if p.is_empty() {
        return None;
    }
    // 卷标签取最长匹配（walk 建的库标签可能是 `D:\Data` 这种）
    let mut best: Option<(u16, &str)> = None;
    for (i, v) in idx.volumes.iter().enumerate() {
        let v = v.trim_end_matches('\\');
        let Some(head) = p.get(..v.len()) else { continue };
        if !head.eq_ignore_ascii_case(v) {
            continue;
        }
        let rest = &p[v.len()..];
        if rest.is_empty() || rest.starts_with('\\') {
            if best.map_or(true, |(_, b)| v.len() > b.len()) {
                best = Some((i as u16, rest));
            }
        }
    }
    let (vol, rest) = best?;
    let mut cur = ROOT_PARENT;
    for comp in rest.split('\\').filter(|s| !s.is_empty()) {
        cur = find_child(idx, cur, vol, comp)?;
    }
    if cur == ROOT_PARENT {
        None
    } else {
        Some(cur)
    }
}

/// 在 `parent` 的子项里找名字等于 `name` 的记录（忽略大小写）。
fn find_child(idx: &Index, parent: u32, vol: u16, name: &str) -> Option<u32> {
    let want: Vec<u8> = name.as_bytes().iter().map(|b| lower(*b)).collect();
    let n = idx.len() as u32;
    const CHUNK: u32 = 32 * 1024;
    (0..n.div_ceil(CHUNK))
        .into_par_iter()
        .find_map_first(|c| {
            let start = c * CHUNK;
            let end = ((c + 1) * CHUNK).min(n);
            for id in start..end {
                if idx.parent[id as usize] != parent || idx.vol[id as usize] != vol {
                    continue;
                }
                if !idx.is_live(id) {
                    continue;
                }
                if eq_ci(idx.name_bytes(id), &want) {
                    return Some(id);
                }
            }
            None
        })
}

thread_local! {
    static PATH_BUF: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(512));
    static PART_STACK: RefCell<Vec<(u32, u32)>> = RefCell::new(Vec::with_capacity(16));
}

/// 把某条记录的完整路径写进线程本地缓冲，返回切片。
/// 复用缓冲，避免每条记录一次分配。
fn with_path<R>(idx: &Index, id: u32, f: impl FnOnce(&[u8]) -> R) -> R {
    PATH_BUF.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        PART_STACK.with(|st| {
            let mut st = st.borrow_mut();
            st.clear();
            let mut cur = id;
            for _ in 0..512 {
                if !idx.is_live(cur) {
                    break;
                }
                st.push((
                    idx.name_off[cur as usize],
                    idx.name_len[cur as usize] as u32,
                ));
                let p = idx.parent[cur as usize];
                if p == crate::store::ROOT_PARENT || p == cur {
                    break;
                }
                cur = p;
            }
            if let Some(v) = idx.volumes.get(idx.vol[id as usize] as usize) {
                buf.extend_from_slice(v.as_bytes());
            }
            for &(off, len) in st.iter().rev() {
                buf.push(SEP);
                buf.extend_from_slice(&idx.pool[off as usize..(off + len) as usize]);
            }
        });
        f(&buf)
    })
}

/// 单条记录的匹配 + 评分。`q_lower` 是整串小写，用于打分。
fn score_record(
    idx: &Index,
    id: u32,
    tokens: &[Vec<u8>],
    q_lower: &[u8],
    opts: &QueryOpts,
) -> Option<i32> {
    // 墓碑（已删除）记录直接跳过。名字已清空，不判的话空串会匹配任何查询。
    if !idx.is_live(id) {
        return None;
    }
    if !matches_filter(idx, id, &opts.filter) {
        return None;
    }
    let name = idx.name_bytes(id);
    let is_dir = idx.is_dir(id);
    match opts.dirs {
        Some(true) if !is_dir => return None,
        Some(false) if is_dir => return None,
        _ => {}
    }

    let name_ok = tokens.iter().all(|t| find_ci(name, t).is_some());
    if !name_ok {
        if !opts.search_path {
            return None;
        }
        return with_path(idx, id, |p| {
            if tokens.iter().all(|t| find_ci(p, t).is_some()) {
                Some(20)
            } else {
                None
            }
        });
    }

    let mut s = 100i32;
    if eq_ci(name, q_lower) {
        s += 900;
    } else if starts_with_ci(name, q_lower) {
        s += 600;
    } else if let Some(pos) = find_ci(name, q_lower) {
        if pos > 0 && is_sep(name[pos - 1]) {
            s += 300;
        }
    }
    // 名字越短越靠前（用户要找的东西通常名字干脆）
    s -= (name.len().min(64)) as i32;
    if is_dir {
        s -= 60;
    }
    Some(s)
}

fn cmp_hit(idx: &Index, a: &Hit, b: &Hit) -> std::cmp::Ordering {
    b.score
        .cmp(&a.score)
        .then_with(|| idx.name_len[a.id as usize].cmp(&idx.name_len[b.id as usize]))
        .then_with(|| a.id.cmp(&b.id))
}

/// 并行全量扫描。返回 (命中, 扫描耗时毫秒)。
pub fn search_timed(idx: &Index, q: &str, opts: &QueryOpts) -> (Vec<Hit>, f64) {
    let t0 = std::time::Instant::now();
    let tokens = tokenize(q);
    if tokens.is_empty() || idx.is_empty() {
        return (Vec::new(), 0.0);
    }
    let q_lower: Vec<u8> = q.trim().as_bytes().iter().map(|b| lower(*b)).collect();

    let n = idx.len() as u32;
    const CHUNK: u32 = 32 * 1024;
    // 局部最多留这么多，防止全命中时把内存吃满。
    const LOCAL_KEEP: usize = 1024;

    let chunks = n.div_ceil(CHUNK);
    let mut hits: Vec<Hit> = (0..chunks)
        .into_par_iter()
        .fold(Vec::new, |mut acc, c| {
            let start = c * CHUNK;
            let end = ((c + 1) * CHUNK).min(n);
            for id in start..end {
                if let Some(sc) = score_record(idx, id, &tokens, &q_lower, opts) {
                    acc.push(Hit { id, score: sc });
                }
            }
            if acc.len() > LOCAL_KEEP * 4 {
                acc.sort_unstable_by(|a, b| cmp_hit(idx, a, b));
                acc.truncate(LOCAL_KEEP);
            }
            acc
        })
        .reduce(Vec::new, |mut a, mut b| {
            a.append(&mut b);
            a
        });

    hits.sort_unstable_by(|a, b| cmp_hit(idx, a, b));
    hits.truncate(opts.limit.max(1));
    (hits, t0.elapsed().as_secs_f64() * 1000.0)
}

pub fn search(idx: &Index, q: &str, opts: &QueryOpts) -> Vec<Hit> {
    search_timed(idx, q, opts).0
}

/// 空关键词、只有分类条件时的浏览：按索引顺序取前 N 条，目录排在前面。
///
/// 这里不做排序——没有关键词就没有相关性可排，按 MFT 顺序（大致等于按目录
/// 建立顺序）反而更接近资源管理器的观感，而且不需要额外的名字比较开销。
pub fn browse(idx: &Index, filter: &Filter, limit: usize) -> Vec<Hit> {
    let n = idx.len() as u32;
    if n == 0 {
        return Vec::new();
    }
    let keep = limit.max(1);
    const CHUNK: u32 = 32 * 1024;
    let chunks = n.div_ceil(CHUNK);
    // 每个分块只留一小份，合并后总量仍接近 keep，不至于把内存撑大
    let per_chunk = (keep / chunks.max(1) as usize).max(64) + 16;
    let mut hits: Vec<Hit> = (0..chunks)
        .into_par_iter()
        .fold(Vec::new, |mut acc, c| {
            let start = c * CHUNK;
            let end = ((c + 1) * CHUNK).min(n);
            for id in start..end {
                if !idx.is_live(id) {
                    continue;
                }
                if matches_filter(idx, id, filter) {
                    acc.push(Hit { id, score: 0 });
                    if acc.len() >= per_chunk {
                        break;
                    }
                }
            }
            acc
        })
        .reduce(Vec::new, |mut a, mut b| {
            a.append(&mut b);
            a
        });

    // 目录优先，其余保持索引顺序
    hits.sort_unstable_by_key(|h| !idx.is_dir(h.id));
    hits.truncate(keep);
    hits
}
