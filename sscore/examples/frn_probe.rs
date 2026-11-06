//! 诊断：按 FRN 在索引里查记录，看是否撞上别的卷的同号记录。
//! 用法：cargo run --example frn_probe -- <索引路径> <十进制 FRN>

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("缺少索引路径");
    let want: u64 = args.next().expect("缺少 FRN").parse().expect("FRN 不是数字");

    let idx = sscore::Index::load(std::path::Path::new(&db)).expect("索引加载失败");
    let mut hits = 0;
    for (i, frn) in idx.frn.iter().enumerate() {
        if *frn == want {
            hits += 1;
            let id = i as u32;
            println!(
                "id={id} live={} vol={:?} path={}",
                idx.is_live(id),
                idx.volumes.get(idx.vol[id as usize] as usize),
                idx.path_of(id)
            );
        }
    }
    println!("全库合并表里 FRN {want} 命中 {hits} 条");

    // 修复后每个卷各一张表：只有本卷的记录能被解析出来
    for (v, label) in idx.volumes.iter().enumerate() {
        let fi = sscore::sync::FrnIndex::build(&idx, v as u16);
        match fi.get(&idx, want) {
            Some(id) => println!("  按卷建表 {label} → id={id} path={}", idx.path_of(id)),
            None => println!("  按卷建表 {label} → 无此号（本卷新建同号文件不会被顶掉）"),
        }
    }
}
