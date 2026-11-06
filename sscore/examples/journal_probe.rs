//! USN 日志「位置过期」自愈验证。
//!
//! 不碰系统日志，只把读位置人为拨到 `lowest_valid_usn` 之前，制造和
//! 日志回绕完全一样的 1181，然后看 recover 能不能把同步拉回来。
//!
//! 用法：`cargo run --release --example journal_probe -- E`

use sscore::sync::Journal;

fn main() {
    let letter = std::env::args().nth(1).and_then(|s| s.chars().next()).unwrap_or('E');
    let mut j = match Journal::open(letter, 1 << 20) {
        Ok(j) => j,
        Err(e) => {
            println!("打开 {letter}: 失败：{e}");
            return;
        }
    };
    println!(
        "日志 id={} next_usn={} lowest_valid_usn={}",
        j.journal_id, j.next_usn, j.lowest_valid_usn
    );

    // 拨到一个确定落在有效区间之前的位置
    let stale = (j.lowest_valid_usn / 2).max(1);
    if stale >= j.lowest_valid_usn {
        println!("该卷日志还没有被裁剪过（lowest={}），构造不出过期位置", j.lowest_valid_usn);
        return;
    }
    j.seek(stale);
    println!("把读位置拨到 {stale}（合法起点是 {}）", j.lowest_valid_usn);

    match j.read(0, 0) {
        Ok((ev, adv)) => println!("意外读到了 {} 条事件（advanced={adv}），无法复现 1181", ev.len()),
        Err(e) => println!(
            "读失败：{e}（raw={:?}），is_journal_lost={}",
            e.raw_os_error(),
            Journal::is_journal_lost(&e)
        ),
    }

    match j.recover() {
        Ok(r) => println!(
            "recover：rebuilt={} 跳过 USN 跨度 {}（从 {} 挪到 {}）",
            r.rebuilt, r.skipped_usn, r.from, r.resume_at
        ),
        Err(e) => {
            println!("recover 也失败：{e}");
            return;
        }
    }

    // 位置已刷新，再读必须能正常工作（有事件就返回事件，没有就返回 0 条）
    match j.read(0, 0) {
        Ok((ev, _)) => println!("恢复后读取正常：本轮 {} 条事件", ev.len()),
        Err(e) => println!("恢复后仍然失败：{e}"),
    }
}
