//! 诊断：模拟应用启动时挂 USN 同步这一步，打印真实错误。
//! 用法：cargo run --example sync_probe -- <索引路径> <盘符>

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("缺少索引路径");
    let letter = args
        .next()
        .unwrap_or_else(|| "E".into())
        .chars()
        .next()
        .unwrap();

    let mut idx = sscore::Index::load(std::path::Path::new(&db)).expect("索引加载失败");
    println!("记录数 {}，卷 {:?}，has_frn={}", idx.len(), idx.volumes, idx.has_frn());

    match sscore::sync::Syncer::attach(&mut idx, letter) {
        Ok(mut s) => {
            s.skip_to_end();
            println!("attach OK journal_id={} next_usn={}", s.journal.journal_id, s.journal.next_usn);
            for _ in 0..3 {
                match s.pump(&mut idx, 0, 0) {
                    Ok(n) => println!("pump ok events={n}"),
                    Err(e) => println!("pump err {e}"),
                }
            }
        }
        Err(e) => println!("attach FAILED: {e}"),
    }
}
