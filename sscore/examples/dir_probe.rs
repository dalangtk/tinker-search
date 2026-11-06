//! 验证「列出某目录的直接子项」在真实索引上的表现。
//!
//! 用法：cargo run --release --example dir_probe -- <index> <目录路径>

use sscore::{resolve_path, Index, ROOT_PARENT};

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().unwrap_or_else(|| "sscore.idx".into());
    let dir = args.next().unwrap_or_else(|| r"E:\测试1".into());

    let idx = Index::load(std::path::Path::new(&db)).expect("加载索引失败");
    let Some(parent) = resolve_path(&idx, &dir) else {
        println!("索引里找不到目录：{dir}");
        return;
    };
    println!("{dir} → id {parent}（目录 {}）", idx.is_dir(parent));

    let mut kids: Vec<u32> = (0..idx.len() as u32)
        .filter(|id| idx.parent[*id as usize] == parent && idx.is_visible(*id))
        .collect();
    kids.sort_by_cached_key(|&id| (!idx.is_dir(id), idx.name(id).to_ascii_lowercase()));

    println!("直接子项 {} 条，前 12 条：", kids.len());
    for id in kids.iter().take(12) {
        println!(
            "  {} {:<40} {}",
            if idx.is_dir(*id) { "[目录]" } else { "[文件]" },
            idx.name(*id),
            idx.path_of(*id)
        );
    }

    // 顺手核对一下：卷根是不是 ROOT_PARENT（list_dir 输出路径前缀靠它）
    let roots = (0..idx.len() as u32)
        .filter(|id| idx.parent[*id as usize] == ROOT_PARENT && idx.is_visible(*id))
        .count();
    println!("卷根直接子项合计 {roots} 条");
}
