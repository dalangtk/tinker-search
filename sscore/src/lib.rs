//! tinker搜索 —— 原生 Rust 索引内核（原型）。
//!
//! 模块划分：
//! - `mft`    读 `$MFT` 全量枚举（卷内父目录子链索引）
//! - `walk`   非管理员回退遍历
//! - `store`  列式内存存储 + 自定义落盘格式
//! - `query`  并行子串匹配 + 评分

pub mod mft;
pub mod query;
pub mod sql;
pub mod store;
pub mod sync;
pub mod walk;

use std::collections::HashMap;

pub use query::{browse, matches_filter, resolve_path, search, search_timed, Filter, Hit, QueryOpts};
pub use store::{Index, IndexBuilder, ROOT_PARENT};

/// 卷根下的系统目录：回收站里放的是**已经删掉**的文件，系统卷信息是还原点，
/// 两者都不该出现在搜索结果里。
///
/// 只按「卷根下的目录名」匹配，不按路径片段匹配——用户自己的目录里
/// 叫这个名字的概率极低，但真出现了也只是被隐藏，不会误删索引。
pub const HIDDEN_DIRS: &[&str] = &["$RECYCLE.BIN", "System Volume Information"];

/// 给索引打隐藏位（幂等）。建库后、从磁盘加载后各调一次。
pub fn mark_hidden(idx: &mut Index) -> usize {
    idx.mark_hidden_subtrees(HIDDEN_DIRS)
}

/// 枚举结果的中间形态。`key` / `parent_key` 是卷内唯一 id
/// （MFT 里就是 FileReferenceNumber 的低 48 位；walk 里是自增序号）。
pub struct RawRec {
    pub key: u64,
    pub parent_key: u64,
    pub name: String,
    pub is_dir: bool,
}

/// 把扁平记录表装配成列式索引。
///
/// 这一步只做一次映射：`key -> 记录下标`，然后把 `parent_key` 换成父记录下标。
/// 找不到父记录（父目录已被删除、或父就是卷根）时挂到根上。
pub fn assemble(label: &str, recs: &[RawRec]) -> Index {
    let pool_hint: usize = recs.iter().map(|r| r.name.len()).sum();
    let mut b = IndexBuilder::with_capacity(recs.len(), pool_hint);
    let vol = b.add_volume(label);

    let mut map: HashMap<u64, u32> = HashMap::with_capacity(recs.len() * 2);
    let mut ids: Vec<u32> = Vec::with_capacity(recs.len());
    for r in recs {
        // 根记录自身不进索引：它的名字就是卷标，拼路径时直接作为前缀。
        if r.key == r.parent_key {
            map.insert(r.key, ROOT_PARENT);
            ids.push(ROOT_PARENT);
            continue;
        }
        let id = b.push_frn(vol, r.key, r.name.as_bytes(), r.is_dir);
        map.insert(r.key, id);
        ids.push(id);
    }

    for (i, r) in recs.iter().enumerate() {
        let id = ids[i];
        if id == ROOT_PARENT {
            continue;
        }
        let p = map.get(&r.parent_key).copied().unwrap_or(ROOT_PARENT);
        // 父指针不能指向自己，否则拼路径会死循环。
        b.set_parent(id, if p == id { ROOT_PARENT } else { p });
    }

    let mut idx = b.finish();
    // 回收站/系统目录在装配阶段就藏掉，落盘后查询、浏览、SQL 都不会碰到。
    mark_hidden(&mut idx);
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(key: u64, parent: u64, name: &str, is_dir: bool) -> RawRec {
        RawRec { key, parent_key: parent, name: name.to_string(), is_dir }
    }

    /// 真实用户的现场：删掉的文件夹被搬进回收站，里面的文件仍然叫原来的名字。
    fn fixture() -> Index {
        let recs = vec![
            rec(5, 5, "E:", true), // 卷根，assemble 会跳过它
            rec(10, 5, "$RECYCLE.BIN", true),
            rec(11, 10, "S-1-5-21-3578192670-1001", true),
            rec(12, 11, "$RC9NQ3D", true),
            rec(13, 12, "C20261104.pdf", false),
            rec(20, 5, "文档", true),
            rec(21, 20, "C20261104.pdf", false),
        ];
        assemble("E:", &recs)
    }

    #[test]
    fn recycle_bin_subtree_is_hidden() {
        let idx = fixture();
        assert_eq!(idx.hidden_count(), 4, "回收站 + SID 目录 + $R 目录 + 里面的文件");
        assert_eq!(idx.live_count(), 6);
    }

    #[test]
    fn search_skips_recycled_files() {
        let idx = fixture();
        let opts = QueryOpts { limit: 20, ..Default::default() };
        let hits = search(&idx, "C20261104", &opts);
        assert_eq!(hits.len(), 1, "只能剩文档目录里那一个");
        assert!(idx.path_of(hits[0].id).ends_with(r"文档\C20261104.pdf"));
    }

    #[test]
    fn browse_skips_recycled_files() {
        let idx = fixture();
        let hits = browse(&idx, &Filter::default(), 100);
        assert_eq!(hits.len(), 2, "浏览也不该列出回收站里的东西");
        assert!(hits.iter().all(|h| !idx.is_hidden(h.id)));
    }

    #[test]
    fn system_volume_information_is_hidden_too() {
        let recs = vec![
            rec(5, 5, "C:", true),
            rec(10, 5, "System Volume Information", true),
            rec(11, 10, "{abc-123}.vhdx", false),
            rec(20, 5, "Users", true),
        ];
        let idx = assemble("C:", &recs);
        assert_eq!(idx.hidden_count(), 2);
        assert!(search(&idx, "vhdx", &QueryOpts::default()).is_empty());
    }
}
