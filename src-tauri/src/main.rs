// 无控制台子系统：这个属性必须写在**二进制 crate 的根**（main.rs）里才会生效。
// 之前只写在 lib.rs，那是库 crate，对最终 exe 的 /SUBSYSTEM 没有影响，
// 于是发布版一启动就多出一个黑色控制台窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tinker_search_lib::run();
}
