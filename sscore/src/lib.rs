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

    b.finish()
}
