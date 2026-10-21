//! 真机探测：确认 `probe_gl_support()` 在本机返回什么，以及耗时。
//!
//! 用 `cargo run --example probe_gl` 运行。这个例子不进发布包。

fn main() {
    let start = std::time::Instant::now();
    let supported = otools_vm_native::engine::qemu::probe_gl_support();
    let first = start.elapsed();

    let start2 = std::time::Instant::now();
    let again = otools_vm_native::engine::qemu::probe_gl_support();
    let second = start2.elapsed();

    println!("gl_support 首次 = {} （耗时 {:?}）", supported, first);
    println!("gl_support 再次 = {} （耗时 {:?}，应为缓存命中）", again, second);
    assert_eq!(supported, again, "缓存结果必须与首次一致");

    // 对照：-display none 配 GL 设备必然失败，用来确认探测确实在分辨后端能力，
    // 而不是「只要起了进程就返回 true」。
    println!("（对照结论见 probe_gl 的注释：-display none 会报 does not have OpenGL support）");
}
