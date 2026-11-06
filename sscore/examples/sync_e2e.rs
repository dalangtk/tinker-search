//! 端到端诊断：加载真实索引 → 按卷挂 USN → 重放日志 → 看指定名字是否进了索引。
//!
//! 只读：不回写索引文件。
//! 用法（需要管理员）：sync_e2e <索引> <输出文件> <要查的名字1> [名字2...]

use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("缺少索引路径");
    let out_path = args.next().expect("缺少输出文件");
    let names: Vec<String> = args.collect();

    let mut log = String::new();
    let mut idx = sscore::Index::load(std::path::Path::new(&db)).expect("索引加载失败");
    log.push_str(&format!(
        "索引 {} 条，卷 {:?}\n",
        idx.live_count(),
        idx.volumes
    ));

    let letters: Vec<char> = idx.volumes.iter().filter_map(|v| v.chars().next()).collect();
    let t0 = std::time::Instant::now();
    let mut total = 0usize;
    for letter in letters {
        match sscore::sync::Syncer::attach(&mut idx, letter) {
            Ok(mut s) => {
                // 和 start_sync 里「没有断点」的分支一致：从日志最早一条重放
                let first = s.journal.lowest_valid_usn;
                s.journal.seek(first);
                let mut n = 0usize;
                for _ in 0..4096 {
                    match s.pump(&mut idx, 0, 0) {
                        Ok(0) => break,
                        Ok(k) => {
                            n += k;
                            if n > 2_000_000 {
                                break;
                            }
                        }
                        Err(e) => {
                            log.push_str(&format!("{letter}: pump 出错 {e}\n"));
                            break;
                        }
                    }
                }
                total += n;
                log.push_str(&format!(
                    "{letter}: 重放 {n} 条，进度 journal={} usn={}\n",
                    s.journal.journal_id, s.journal.next_usn
                ));
            }
            Err(e) => log.push_str(&format!("{letter}: attach 失败 {e}\n")),
        }
    }
    log.push_str(&format!(
        "合计 {total} 条，耗时 {} ms\n",
        t0.elapsed().as_millis()
    ));

    for want in &names {
        let hits: Vec<String> = (0..idx.len() as u32)
            .filter(|i| idx.is_live(*i) && idx.name(*i) == want.as_str())
            .map(|i| idx.path_of(i))
            .collect();
        log.push_str(&format!(
            "查「{want}」→ {} 条\n",
            hits.len()
        ));
        for h in hits {
            log.push_str(&format!("    {h}\n"));
        }
    }

    let mut f = std::fs::File::create(&out_path).expect("写不出结果");
    let _ = f.write_all(log.as_bytes());
    println!("{log}");
}
