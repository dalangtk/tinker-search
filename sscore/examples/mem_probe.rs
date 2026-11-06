//! 内存占用探针：加载真实索引，报告进程工作集 / 峰值与索引各列开销。
//!
//! ```text
//! cargo run --release --example mem_probe -- <索引文件>
//! ```

use sscore::Index;
use std::ffi::c_void;

#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[link(name = "psapi")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn GetProcessMemoryInfo(
        process: *mut c_void,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

fn mem() -> (f64, f64, f64) {
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok == 0 {
        return (0.0, 0.0, 0.0);
    }
    const MB: f64 = 1048576.0;
    (c.working_set_size as f64 / MB, c.peak_working_set_size as f64 / MB, c.pagefile_usage as f64 / MB)
}

fn line(tag: &str) {
    let (ws, peak, own) = mem();
    println!("{tag:<14} 工作集 {ws:7.1} MB   峰值 {peak:7.1} MB   私有 {own:7.1} MB");
}

fn main() {
    let path = std::env::args().nth(1).expect("用法：mem_probe <索引文件>");
    line("启动");

    let t = std::time::Instant::now();
    let idx = Index::load(std::path::Path::new(&path)).expect("加载失败");
    let ms = t.elapsed().as_millis();
    println!(
        "加载 {ms} ms，{} 条，库文件 {:.1} MB",
        idx.len(),
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1048576.0
    );
    line("加载后");

    let m = idx.memory_breakdown();
    println!("\n索引内存明细：");
    let total: usize = m.iter().map(|(_, b)| b).sum();
    for (name, bytes) in m.iter() {
        println!(
            "  {name:<10} {:>9.1} MB   {:>5.1}%",
            *bytes as f64 / 1048576.0,
            *bytes as f64 * 100.0 / total as f64
        );
    }
    println!(
        "  {:<10} {:>9.1} MB   合计，平均 {:.1} 字节/条",
        "合计",
        total as f64 / 1048576.0,
        total as f64 / idx.len().max(1) as f64
    );

    // 实时同步用的 FRN 映射表（每个卷一张，按记录下标存 4 字节/条）
    let mut map_total = 0usize;
    for v in 0..idx.volumes.len() {
        let fi = sscore::sync::FrnIndex::build(&idx, v as u16);
        let b = fi.memory_bytes();
        map_total += b;
        println!("  同步映射 {}: {:.1} MB（{} 条）", idx.volumes[v], b as f64 / 1048576.0, fi.len());
    }
    println!("  同步映射合计 {:.1} MB", map_total as f64 / 1048576.0);

    // 释放：量一下空闲后能回吐多少
    drop(idx);
    line("drop 后");
}
