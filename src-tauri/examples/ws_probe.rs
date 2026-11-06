//! 探针：核对「任务管理器内存」到底对应哪个计数。
//!
//! 同时打印
//! 1. `PROCESS_MEMORY_COUNTERS_EX2.WorkingSetSize`（工作集，含共享页）
//! 2. `PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`（专用工作集）
//! 3. 自己遍历 `QueryWorkingSetEx` 数出来的专用页（独立交叉验证）
//!
//! 打印 PID 后挂起 8 秒，好让外部用 WMI 的 `WorkingSetPrivate` 对一遍。

use std::ffi::c_void;

#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(process: *mut c_void, counters: *mut Ex2, cb: u32) -> i32;
    fn QueryWorkingSetEx(process: *mut c_void, pv: *mut c_void, cb: u32) -> i32;
}

extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn GetCurrentProcessId() -> u32;
    fn GetSystemInfo(info: *mut SystemInfo);
}

#[repr(C)]
#[derive(Default)]
struct Ex2 {
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
    private_usage: usize,
    private_working_set_size: usize,
    shared_commit_usage: u64,
}

#[repr(C)]
#[derive(Default)]
struct SystemInfo {
    arch: u16,
    reserved: u16,
    page_size: u32,
    min_app: *mut c_void,
    max_app: *mut c_void,
    mask: usize,
    num_processors: u32,
    processor_type: u32,
    alloc_granularity: u32,
    processor_level: u16,
    processor_revision: u16,
}

#[repr(C)]
struct WsExInfo {
    virtual_address: *mut c_void,
    flags: usize,
}

fn mb(v: usize) -> f64 {
    v as f64 / 1048576.0
}

fn main() {
    let pid = unsafe { GetCurrentProcessId() };

    let mut ex2 = Ex2 {
        cb: std::mem::size_of::<Ex2>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut ex2, ex2.cb) };
    assert_ne!(ok, 0, "GetProcessMemoryInfo(EX2) 失败");

    // 自己数专用页：把整段已提交的虚拟地址喂给 QueryWorkingSetEx
    let mut si = SystemInfo::default();
    unsafe { GetSystemInfo(&mut si) };
    let page = si.page_size.max(4096) as usize;

    let mut private_pages = 0usize;
    let mut shared_pages = 0usize;
    let mut present_pages = 0usize;
    // 只扫前 4 GB 的地址空间就足够说明问题（本进程都在低地址）
    let span = 4usize * 1024 * 1024 * 1024;
    let n = span / page;
    let mut buf: Vec<WsExInfo> = (0..n)
        .map(|i| WsExInfo { virtual_address: (i * page) as *mut c_void, flags: 0 })
        .collect();
    let ok = unsafe {
        QueryWorkingSetEx(
            GetCurrentProcess(),
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * std::mem::size_of::<WsExInfo>()) as u32,
        )
    };
    if ok != 0 {
        for e in buf.iter() {
            let valid = e.flags & 1;
            if valid == 0 {
                continue;
            }
            present_pages += 1;
            // x64: bit15 = Shared
            if (e.flags >> 15) & 1 == 1 {
                shared_pages += 1;
            } else {
                private_pages += 1;
            }
        }
    }

    println!("pid                 = {}", pid);
    println!("工作集 WorkingSetSize = {:.1} MB", mb(ex2.working_set_size));
    println!("专用工作集 EX2        = {:.1} MB", mb(ex2.private_working_set_size));
    println!("提交 PrivateUsage    = {:.1} MB", mb(ex2.private_usage));
    println!("共享提交 SharedCommit = {:.1} MB", mb(ex2.shared_commit_usage as usize));
    if ok != 0 {
        println!(
            "QueryWorkingSetEx    : 常驻 {} 页（专用 {:.1} MB / 共享 {:.1} MB）",
            present_pages,
            mb(private_pages * page),
            mb(shared_pages * page)
        );
    } else {
        println!("QueryWorkingSetEx    失败，跳过交叉验证");
    }
    println!("保持 25 秒，供 WMI 核对…");
    unsafe {
        let mut again = Ex2 { cb: std::mem::size_of::<Ex2>() as u32, ..Default::default() };
        GetProcessMemoryInfo(GetCurrentProcess(), &mut again, again.cb);
        println!(
            "睡眠前复读：工作集 {:.1} MB / 专用工作集 {:.1} MB",
            mb(again.working_set_size),
            mb(again.private_working_set_size)
        );
    }
    std::thread::sleep(std::time::Duration::from_secs(25));
}
