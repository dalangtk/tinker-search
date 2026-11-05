//! 非管理员回退路径：按目录树递归遍历。
//!
//! 慢（每个目录一次 `FindFirstFile`），但零权限要求，用来在没有管理员的环境下
//! 建小规模索引、做功能验证与基准对照。

use crate::RawRec;
use std::io;
use std::path::Path;

/// 递归遍历 `root`。`label` 是写进索引的卷标（例如 `C:` 或 `D:\Data`）。
pub fn walk(root: &Path, label: &str, max_depth: usize) -> io::Result<(Vec<RawRec>, f64)> {
    let t0 = std::time::Instant::now();
    let mut out: Vec<RawRec> = Vec::with_capacity(1 << 16);
    let mut skipped = 0usize;

    // 第一个记录是根，父指针指向自己（assemble 会把它变成 ROOT_PARENT）。
    out.push(RawRec { key: 0, parent_key: 0, name: label.to_string(), is_dir: true });

    let mut stack: Vec<(std::path::PathBuf, u64, usize)> = vec![(root.to_path_buf(), 0, 0)];
    while let Some((dir, key, depth)) = stack.pop() {
        if depth >= max_depth {
            continue;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for ent in rd {
            let ent = match ent {
                Ok(e) => e,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let ft = match ent.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            // 不跟随符号链接/联接点，否则容易绕圈。
            if ft.is_symlink() {
                continue;
            }
            let name = ent.file_name();
            let name = name.to_string_lossy();
            let child_key = out.len() as u64;
            out.push(RawRec {
                key: child_key,
                parent_key: key,
                name: name.into_owned(),
                is_dir: ft.is_dir(),
            });
            if ft.is_dir() {
                stack.push((ent.path(), child_key, depth + 1));
            }
        }
    }

    let secs = t0.elapsed().as_secs_f64() * 1000.0;
    if skipped > 0 {
        eprintln!("[walk] 跳过 {skipped} 个无权限/不可读目录");
    }
    Ok((out, secs))
}
