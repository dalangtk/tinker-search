//! 验证「回收站里的文件不再被搜出来」：读真实索引，对比打隐藏位前后的结果。
//!
//! 用法：cargo run --release --example hidden_probe -- <index> [关键词]

use sscore::{search, Index, QueryOpts};

fn count_visible_recycled(idx: &Index) -> usize {
    (0..idx.len() as u32)
        .filter(|id| idx.is_visible(*id))
        .filter(|id| idx.path_of(*id).to_ascii_lowercase().contains("\\$recycle.bin\\"))
        .count()
}

fn hits_of(idx: &Index, q: &str) -> Vec<String> {
    let opts = QueryOpts { limit: 20, ..Default::default() };
    search(idx, q, &opts).iter().map(|h| idx.path_of(h.id)).collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().unwrap_or_else(|| "sscore.idx".into());
    let q = args.next().unwrap_or_else(|| "C20261104".into());

    let t = std::time::Instant::now();
    let mut idx = Index::load(std::path::Path::new(&db)).expect("加载索引失败");
    println!("索引 {} 条，加载 {} ms", idx.len(), t.elapsed().as_millis());

    println!("--- 修复前 ---");
    println!("可见但位于回收站的记录：{}", count_visible_recycled(&idx));
    println!("搜索 {:?}：{} 条", q, hits_of(&idx, &q).len());
    for p in hits_of(&idx, &q).iter().take(5) {
        println!("  {p}");
    }

    let t2 = std::time::Instant::now();
    let marked = sscore::mark_hidden(&mut idx);
    println!("--- 打隐藏位：{marked} 条，耗时 {} ms ---", t2.elapsed().as_millis());
    println!("可见但位于回收站的记录：{}", count_visible_recycled(&idx));
    let after = hits_of(&idx, &q);
    println!("搜索 {:?}：{} 条", q, after.len());
    for p in after.iter().take(5) {
        println!("  {p}");
    }
    println!("隐藏总数：{}", idx.hidden_count());

    // 落盘 → 重新加载：隐藏位必须跟着 flags 列一起持久化。
    let marked_path = std::path::Path::new(&db).with_extension("marked.idx");
    idx.save(&marked_path).expect("存盘失败");
    let back = Index::load(&marked_path).expect("重新加载失败");
    println!(
        "重新加载后：隐藏 {} 条 / 搜索 {:?} {} 条 / 回收站可见 {} 条",
        back.hidden_count(),
        q,
        hits_of(&back, &q).len(),
        count_visible_recycled(&back)
    );
    let _ = std::fs::remove_file(&marked_path);
}
