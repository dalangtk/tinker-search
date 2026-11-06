//! sscore 命令行：建库 / 查询 / 基准 / 实时同步。
//!
//! 用法：
//!   sscore mft C --db sscore.idx          读 $MFT 全量建库（需管理员）
//!   sscore walk C:\Users --db sscore.idx  遍历建库（零权限）
//!   sscore info --db sscore.idx
//!   sscore find 报告 --db sscore.idx
//!   sscore bench --db sscore.idx --n 200
//!   sscore watch C --db sscore.idx        挂着实时同步（需管理员）

use sscore::sync::{reap_orphans, Journal, Syncer};
use sscore::sql;
use sscore::{assemble, search_timed, Filter, Index, QueryOpts};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn default_db() -> PathBuf {
    PathBuf::from("sscore.idx")
}

fn arg_val(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned()
}

fn has_flag(args: &[String], key: &str) -> bool {
    args.iter().any(|a| a == key)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: sscore <mft|walk|find|info|bench> ...");
        std::process::exit(2);
    }
    let db = arg_val(&args, "--db").map(PathBuf::from).unwrap_or_else(default_db);

    let rc = match args[0].as_str() {
        "mft" => cmd_mft(&args, &db),
        "walk" => cmd_walk(&args, &db),
        "find" => cmd_find(&args, &db),
        "sql" => cmd_sql(&args, &db),
        "info" => cmd_info(&db),
        "check" => cmd_check(&args, &db),
        "bench" => cmd_bench(&args, &db),
        "synth" => cmd_synth(&args, &db),
        "watch" => cmd_watch(&args, &db),
        other => {
            eprintln!("未知命令: {other}");
            2
        }
    };
    std::process::exit(rc);
}

fn report_build(idx: &Index, label: &str, enum_ms: f64, asm_ms: f64, save_ms: f64, bytes: u64) {
    let n = idx.len();
    let mem = idx.memory_bytes();
    println!("卷标        : {label}");
    println!("记录数      : {n}");
    println!("名字池      : {:.1} MB", idx.pool.len() as f64 / 1048576.0);
    println!(
        "内存占用    : {:.1} MB ({:.1} 字节/条)",
        mem as f64 / 1048576.0,
        idx.bytes_per_record()
    );
    println!("落盘大小    : {:.1} MB", bytes as f64 / 1048576.0);
    println!("枚举耗时    : {enum_ms:.0} ms");
    println!("装配耗时    : {asm_ms:.0} ms");
    println!("存盘耗时    : {save_ms:.0} ms");
    println!("合计        : {:.0} ms", enum_ms + asm_ms + save_ms);
    if enum_ms > 0.0 {
        println!("枚举吞吐    : {:.0} 条/秒", n as f64 / (enum_ms / 1000.0));
    }
}

fn finish(idx: Index, label: &str, db: &PathBuf, enum_ms: f64, asm_ms: f64) -> i32 {
    if let Err(e) = idx.validate() {
        eprintln!("索引自检失败: {e}");
        return 1;
    }
    let t = Instant::now();
    match idx.save(db) {
        Ok(bytes) => {
            let save_ms = t.elapsed().as_secs_f64() * 1000.0;
            report_build(&idx, label, enum_ms, asm_ms, save_ms, bytes);
            println!("索引库      : {}", db.display());
            0
        }
        Err(e) => {
            eprintln!("存盘失败: {e}");
            1
        }
    }
}

fn cmd_mft(args: &[String], db: &PathBuf) -> i32 {
    let letter = match args.get(1) {
        Some(v) => v.clone(),
        None => {
            eprintln!("用法: sscore mft <盘符>");
            return 2;
        }
    };
    #[cfg(windows)]
    {
        let c = match sscore::mft::normalize_letter(&letter) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        };
        println!("[1/3] 枚举 {c}: 的 $MFT ...");
        let (recs, enum_ms) = match sscore::mft::enum_volume(c) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("枚举失败: {e}");
                return 1;
            }
        };
        println!("      拿到 {} 条原始记录，{enum_ms:.0} ms", recs.len());
        let t = Instant::now();
        let idx = assemble(&format!("{c}:"), &recs);
        let asm_ms = t.elapsed().as_secs_f64() * 1000.0;
        drop(recs);
        println!("[2/3] 装配列式索引 ... {asm_ms:.0} ms");
        println!("[3/3] 存盘 ...");
        finish(idx, &format!("{c}:"), db, enum_ms, asm_ms)
    }
    #[cfg(not(windows))]
    {
        let _ = (letter, db);
        eprintln!("mft 模式仅支持 Windows");
        2
    }
}

fn cmd_walk(args: &[String], db: &PathBuf) -> i32 {
    let root = match args.get(1) {
        Some(v) => v.clone(),
        None => {
            eprintln!("用法: sscore walk <目录> [--depth N]");
            return 2;
        }
    };
    let depth: usize = arg_val(args, "--depth").and_then(|v| v.parse().ok()).unwrap_or(64);
    let p = PathBuf::from(&root);
    if !p.is_dir() {
        eprintln!("不是目录: {root}");
        return 2;
    }
    // 卷标统一成反斜杠、去掉尾部分隔符，拼路径时才不会出现 C:/a\b。
    let label = root.replace('/', "\\");
    let label = label.trim_end_matches('\\').to_string();
    println!("[1/3] 遍历 {root} (深度上限 {depth}) ...");
    let (recs, walk_ms) = match sscore::walk::walk(&p, &label, depth) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("遍历失败: {e}");
            return 1;
        }
    };
    println!("      拿到 {} 条原始记录，{walk_ms:.0} ms", recs.len());
    let t = Instant::now();
    let idx = assemble(&label, &recs);
    let asm_ms = t.elapsed().as_secs_f64() * 1000.0;
    drop(recs);
    println!("[2/3] 装配列式索引 ... {asm_ms:.0} ms");
    println!("[3/3] 存盘 ...");
    finish(idx, &label, db, walk_ms, asm_ms)
}

/// 抽样把索引里的路径还原出来，逐个去文件系统上核对是否存在。
/// 这是「不存全路径、靠父链拼装」这个设计最容易出错的地方，必须能自证。
fn cmd_check(args: &[String], db: &PathBuf) -> i32 {
    let idx = match Index::load(db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return 1;
        }
    };
    let n = idx.len();
    if n == 0 {
        eprintln!("索引为空");
        return 1;
    }
    let sample: usize = arg_val(args, "--n").and_then(|v| v.parse().ok()).unwrap_or(500);
    let step = (n / sample).max(1);
    let mut checked = 0usize;
    let mut ok = 0usize;
    let mut bad: Vec<String> = Vec::new();
    let mut id = 0usize;
    while id < n && checked < sample {
        let p = idx.path_of(id as u32);
        checked += 1;
        if std::path::Path::new(&p).exists() {
            ok += 1;
        } else if bad.len() < 10 {
            bad.push(p);
        }
        id += step;
    }
    println!("抽样 {} 条，路径存在 {} 条（{:.2}%）", checked, ok, ok as f64 * 100.0 / checked as f64);
    if !bad.is_empty() {
        println!("对不上的样本：");
        for b in &bad {
            println!("  {b}");
        }
    }
    if ok == checked {
        0
    } else {
        1
    }
}

fn cmd_info(db: &PathBuf) -> i32 {
    let idx = match Index::load(db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return 1;
        }
    };
    if let Err(e) = idx.validate() {
        eprintln!("索引自检失败: {e}");
        return 1;
    }
    let n = idx.len();
    let dirs = (0..n as u32).filter(|&i| idx.is_dir(i)).count();
    println!("记录数      : {n}（目录 {dirs}，文件 {}）", n - dirs);
    println!("名字池      : {:.1} MB", idx.pool.len() as f64 / 1048576.0);
    println!(
        "内存占用    : {:.1} MB ({:.1} 字节/条)",
        idx.memory_bytes() as f64 / 1048576.0,
        idx.bytes_per_record()
    );
    println!("卷标        : {}", idx.volumes.join(", "));
    println!(
        "已隐藏      : {}（回收站/系统卷信息，不参与搜索）",
        idx.hidden_count()
    );
    println!("自检        : 通过");
    0
}

fn parse_dirs(args: &[String]) -> Option<bool> {
    match arg_val(args, "--dirs").as_deref() {
        Some("dir") | Some("dirs") => Some(true),
        Some("file") | Some("files") => Some(false),
        _ => None,
    }
}

fn cmd_sql(args: &[String], db: &PathBuf) -> i32 {
    let q: Vec<String> = args
        .iter()
        .skip(1)
        .take_while(|a| !a.starts_with("--"))
        .cloned()
        .collect();
    if q.is_empty() {
        eprintln!("用法: sscore sql \"SELECT Path FROM alltable WHERE Size > '100M'\" [--limit N]");
        return 2;
    }
    let text = q.join(" ");
    let idx = match Index::load(db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return 1;
        }
    };
    let limit = arg_val(args, "--limit").and_then(|v| v.parse().ok()).unwrap_or(20);
    let t0 = Instant::now();
    match sql::run(&idx, &text, &Filter::default(), limit) {
        Ok(t) => {
            println!("SQL 命中 {} 行，用时 {:.2} ms", t.total, t0.elapsed().as_secs_f64() * 1000.0);
            println!("{}", t.fields.join(" | "));
            if t.aggregate {
                for g in &t.groups {
                    println!("{} | {} | {} | {} | {}", g.key_text, g.count, g.sum, g.min, g.max);
                }
            } else {
                for r in &t.rows {
                    let cells: Vec<String> = r
                        .iter()
                        .map(|v| match v {
                            sql::Value::Text(s) => s.clone(),
                            sql::Value::Num(n) => format!("{n:.0}"),
                            sql::Value::Null => "-".into(),
                        })
                        .collect();
                    println!("{}", cells.join(" | "));
                }
            }
            0
        }
        Err(e) => {
            eprintln!("SQL 错误: {e}");
            1
        }
    }
}

fn cmd_find(args: &[String], db: &PathBuf) -> i32 {
    let q: Vec<String> = args
        .iter()
        .skip(1)
        .take_while(|a| !a.starts_with("--"))
        .cloned()
        .collect();
    if q.is_empty() {
        eprintln!("用法: sscore find <关键词> [--limit N] [--dirs all|dir|file] [--path]");
        return 2;
    }
    let q = q.join(" ");
    let idx = match Index::load(db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return 1;
        }
    };
    let opts = QueryOpts {
        limit: arg_val(args, "--limit").and_then(|v| v.parse().ok()).unwrap_or(20),
        dirs: parse_dirs(args),
        search_path: has_flag(args, "--path"),
        filter: Filter::default(),
    };
    let (hits, ms) = search_timed(&idx, &q, &opts);
    println!("查询 \"{q}\" 命中 {} 条，{ms:.2} ms（库 {:.1} MB / {} 条）", hits.len(), idx.memory_bytes() as f64 / 1048576.0, idx.len());
    for h in &hits {
        println!("  {:6}  {}", h.score, idx.path_of(h.id));
    }
    0
}

/// 把现有库按倍数复制并给名字加序号后缀，用来在不动磁盘的前提下模拟大规模索引，
/// 观察「内存放不下」时扫描吞吐的衰减。仅用于基准，不参与真实业务。
fn cmd_synth(args: &[String], db: &PathBuf) -> i32 {
    let src = match Index::load(db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return 1;
        }
    };
    let scale: usize = arg_val(args, "--scale").and_then(|v| v.parse().ok()).unwrap_or(10);
    let out = arg_val(args, "--out").map(PathBuf::from).unwrap_or_else(|| {
        let mut p = db.clone();
        p.set_extension("synth.idx");
        p
    });
    let n = src.len();
    let label = src.volumes.first().cloned().unwrap_or_else(|| "X:".to_string());
    let mut recs: Vec<sscore::RawRec> = Vec::with_capacity(n * scale);
    for k in 0..scale {
        for i in 0..n {
            let id = i as u32;
            let name = src.name(id);
            let name = if k == 0 {
                name.to_string()
            } else {
                match name.rsplit_once('.') {
                    Some((stem, ext)) if ext.len() <= 5 => format!("{stem}_{k}.{ext}"),
                    _ => format!("{name}_{k}"),
                }
            };
            let base = (k * n) as u64;
            let pk = src.parent[id as usize];
            // 0 与 u32::MAX 都表示「挂在卷根上」，映射到本份拷贝的根记录。
            let parent_key = if pk == u32::MAX || pk == 0 { base } else { base + pk as u64 };
            recs.push(sscore::RawRec {
                key: base + i as u64,
                parent_key,
                name,
                is_dir: src.is_dir(id),
            });
        }
    }
    // 让根记录的父指向自己：按 key 0 复制一份即可。
    recs[0].parent_key = 0;
    let t = Instant::now();
    let idx = assemble(&label, &recs);
    let asm = t.elapsed().as_secs_f64() * 1000.0;
    drop(recs);
    println!("合成 {n} × {scale} = {} 条，装配 {asm:.0} ms", idx.len());
    finish(idx, &label, &out, 0.0, asm)
}

fn cmd_bench(args: &[String], db: &PathBuf) -> i32 {
    let idx = match Index::load(db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return 1;
        }
    };
    if idx.is_empty() {
        eprintln!("索引为空");
        return 1;
    }
    let n: usize = arg_val(args, "--n").and_then(|v| v.parse().ok()).unwrap_or(200);
    let rounds: usize = arg_val(args, "--rounds").and_then(|v| v.parse().ok()).unwrap_or(5);

    // 从索引里随机抽真实文件名做查询词，避免造词失真。
    let total = idx.len() as u32;
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % total as u64) as u32
    };

    // 固定一批查询词，供「换引擎跑同一批词」的对比使用。
    if let Some(path) = arg_val(args, "--write-queries") {
        let mut out = String::new();
        for _ in 0..n {
            let id = next();
            let name = idx.name(id);
            let stem = name.split('.').next().unwrap_or(name);
            let q: String = stem.chars().skip(1).take(4).collect();
            if q.chars().count() >= 2 {
                out.push_str(&q);
                out.push('\n');
            }
        }
        match std::fs::write(&path, out) {
            Ok(_) => println!("已写出查询词: {path}"),
            Err(e) => eprintln!("写查询词失败: {e}"),
        }
        return 0;
    }

    let mut queries: Vec<String> = Vec::with_capacity(n);
    while queries.len() < n {
        let id = next();
        let name = idx.name(id);
        let stem = name.split('.').next().unwrap_or(name);
        // 取 2~6 个字符，贴近真实输入
        let take = stem.chars().count().clamp(2, 6);
        if take < 2 {
            continue;
        }
        let q: String = stem.chars().skip(1).take(take).collect();
        if !q.is_empty() {
            queries.push(q);
        }
    }

    // 也可以从文件读同一批查询词，用于和别的引擎逐词对比。
    if let Some(p) = arg_val(args, "--queries") {
        if let Ok(txt) = std::fs::read_to_string(&p) {
            let list: Vec<String> = txt
                .lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                queries = list;
            }
        }
    }

    let opts = QueryOpts {
        limit: arg_val(args, "--limit").and_then(|v| v.parse().ok()).unwrap_or(50),
        dirs: None,
        search_path: false,
        filter: Filter::default(),
    };
    // 预热：把代码路径和分页都走热
    for q in queries.iter().take(32) {
        let _ = search_timed(&idx, q, &opts);
    }

    let mut all: Vec<f64> = Vec::with_capacity(n * rounds);
    let mut hit_total = 0usize;
    for _ in 0..rounds {
        for q in &queries {
            let (hits, ms) = search_timed(&idx, q, &opts);
            hit_total += hits.len();
            all.push(ms);
        }
    }
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |p: f64| all[((all.len() - 1) as f64 * p) as usize];
    let sum: f64 = all.iter().sum();

    let pool_mb = idx.pool.len() as f64 / 1048576.0;
    println!("库规模      : {} 条，名字池 {:.1} MB，内存 {:.1} MB", idx.len(), pool_mb, idx.memory_bytes() as f64 / 1048576.0);
    println!("查询数      : {} 词 × {rounds} 轮 = {}", queries.len(), all.len());
    println!("平均命中    : {:.1} 条/次", hit_total as f64 / all.len() as f64);
    println!("最小        : {:.2} ms", all[0]);
    println!("中位        : {:.2} ms", pick(0.5));
    println!("p95         : {:.2} ms", pick(0.95));
    println!("最大        : {:.2} ms", all[all.len() - 1]);
    println!("平均        : {:.2} ms", sum / all.len() as f64);
    if let Some(&med) = all.get(all.len() / 2) {
        if med > 0.0 {
            println!("扫描吞吐    : {:.0} MB/s（按名字池字节）", pool_mb / (med / 1000.0));
        }
    }
    0
}

/// 挂着实时同步跑。既当常驻进程用，也是测开销的工具。
///
/// 关键指标是 `同步占用`：处理同步事件花掉的 CPU 时间 / 墙钟时间。
/// 空闲时它应该是 0.0x% 量级——每次轮询只是读一次日志尾部，没有变更就立刻返回。
fn cmd_watch(args: &[String], db: &PathBuf) -> i32 {
    let Some(target) = args.get(1) else {
        eprintln!("用法: sscore watch <盘符> --db xxx.idx [--interval 250] [--seconds 30] [--bench-query 词]");
        return 2;
    };
    let letter = match sscore::mft::normalize_letter(target) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let interval: u64 = arg_val(args, "--interval").and_then(|v| v.parse().ok()).unwrap_or(250);
    let seconds: u64 = arg_val(args, "--seconds").and_then(|v| v.parse().ok()).unwrap_or(0);
    let every: u64 = arg_val(args, "--every").and_then(|v| v.parse().ok()).unwrap_or(8);
    let bench_q = arg_val(args, "--bench-query");
    let probe = arg_val(args, "--probe");
    let save_on_exit = has_flag(args, "--save");
    let verify = has_flag(args, "--verify");

    let t0 = Instant::now();
    let mut idx = match Index::load(db) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("加载索引失败: {e}");
            return 1;
        }
    };
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let live = idx.live_count();
    println!("索引        : {} 条（有效 {live}，墓碑 {}），{:.1} MB，加载 {load_ms:.0} ms",
        idx.len(), idx.tomb_count(), idx.memory_bytes() as f64 / 1048576.0);
    if !idx.has_frn() {
        eprintln!("这个索引没有 FRN 列（多半是 walk 建的），无法做 USN 同步。请先用 mft 模式重建。");
        return 3;
    }

    let ta = Instant::now();
    let mut syncer = match Syncer::attach(&mut idx, letter) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("挂载 {letter}: 的 USN 日志失败: {e}");
            return 4;
        }
    };
    let attach_ms = ta.elapsed().as_secs_f64() * 1000.0;
    // 快照就是当前状态，历史事件不必重放，从日志末尾开始。
    syncer.skip_to_end();
    println!("USN 日志    : 会话 {} 起点 usn={} 挂载耗时 {attach_ms:.0} ms",
        syncer.journal.journal_id, syncer.journal.next_usn);
    println!("轮询间隔    : {interval} ms{}", if seconds > 0 { format!("，跑 {seconds} 秒") } else { String::new() });
    println!();

    let start = Instant::now();
    let mut report_at = 0u64;
    let mut qt: Vec<f64> = Vec::new();
    let mut probe_last: usize = usize::MAX;
    let mut probe_at: u64 = u64::MAX;

    loop {
        match syncer.pump(&mut idx, 0, 0) {
            Ok(_) => {}
            Err(e) if Journal::is_journal_lost(&e) => {
                println!("\n[!] USN 日志被删除或正在重建（{e}），断点失效，需要全量重建索引。");
                return 5;
            }
            Err(e) => {
                eprintln!("\n[!] 读 USN 失败: {e}");
                return 6;
            }
        }

        // 同步期间照常查询：用来证明「写索引」不挡「读索引」。
        if let Some(q) = &bench_q {
            let opts = QueryOpts { limit: 50, dirs: None, search_path: false, filter: Filter::default() };
            let (_, ms) = search_timed(&idx, q, &opts);
            qt.push(ms);
        }

        // 探针：有新事件时查一次，命中数一变就打印，用来肉眼确认同步真的写进了索引。
        // 只在事件数变化时才查，避免探针本身污染开销测量。
        if let Some(q) = &probe {
            if syncer.stats.events != probe_at {
                probe_at = syncer.stats.events;
                let opts = QueryOpts { limit: 1_000_000, dirs: None, search_path: false, filter: Filter::default() };
                let (hits, ms) = search_timed(&idx, q, &opts);
                if hits.len() != probe_last {
                    probe_last = hits.len();
                    let first = hits.first().map(|h| idx.path_of(h.id)).unwrap_or_default();
                    println!("  [探针] \"{q}\" → {} 条（{ms:.2} ms）  首条: {first}", hits.len());
                }
            }
        }

        let elapsed = start.elapsed();
        if every > 0 && syncer.stats.rounds >= report_at + every {
            report_at = syncer.stats.rounds;
            let ev = syncer.stats.events;
            let cpu = syncer.stats.busy_ms / (elapsed.as_secs_f64().max(0.001) * 1000.0) * 100.0;
            println!(
                "轮 {:>6} │ 事件 {:>8} │ 增 {:>7} 删 {:>7} 改 {:>7} │ 有效 {:>9} │ 上轮 {:>8.1}µs │ 同步占用 {cpu:.2}%",
                syncer.stats.rounds,
                ev,
                syncer.stats.added,
                syncer.stats.removed,
                syncer.stats.renamed,
                idx.live_count(),
                syncer.stats.last_round_us
            );
        }

        if seconds > 0 && elapsed.as_secs() >= seconds {
            break;
        }
        std::thread::sleep(Duration::from_millis(interval));
    }

    let wall = start.elapsed().as_secs_f64();
    println!();
    println!("================ 汇总 ================");
    println!("运行时长    : {:.1} s（{} 轮）", wall, syncer.stats.rounds);
    println!("事件总数    : {}（增 {} / 删 {} / 改 {} / FRN 复用复活 {}）",
        syncer.stats.events, syncer.stats.added, syncer.stats.removed, syncer.stats.renamed, syncer.stats.revived);
    println!("同步累计耗时: {:.1} ms", syncer.stats.busy_ms);
    println!("同步占用率  : {:.3}%  ← 同步线程的处理时间 / 墙钟时间", syncer.stats.busy_ms / (wall * 1000.0) * 100.0);
    println!("单轮均值    : {:.1} µs", syncer.stats.busy_ms * 1000.0 / syncer.stats.rounds.max(1) as f64);
    println!("索引变化    : {} 条 → {} 条（有效 {}）", live, idx.len(), idx.live_count());
    if !qt.is_empty() {
        let mut s = qt.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("同步中查询  : {} 次，中位 {:.2} ms，p95 {:.2} ms，最大 {:.2} ms",
            s.len(), s[s.len() / 2], s[(s.len() as f64 * 0.95) as usize], s[s.len() - 1]);
    }
    if verify {
        let killed = reap_orphans(&mut idx);
        println!("孤儿修复    : 清掉 {killed} 条父链断裂的记录");
    }
    if save_on_exit {
        let t = Instant::now();
        match idx.save(db) {
            Ok(bytes) => println!("已存盘      : {:.1} MB，{} ms", bytes as f64 / 1048576.0, t.elapsed().as_millis()),
            Err(e) => eprintln!("存盘失败    : {e}"),
        }
    }
    0
}

