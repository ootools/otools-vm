//! 端到端验证：embed 模块的核心函数在真实 QEMU 上能否工作。
//!
//! 用 `cargo run --example embed_e2e` 运行。
//! 不进发布包。

use std::process::{Command, Stdio};
use std::time::Duration;

fn main() {
    let qemu = r"C:\Program Files\qemu\qemu-system-x86_64.exe";
    println!("=== 启动 QEMU (sdl) ===");
    let mut child = Command::new(qemu)
        .args(["-M", "q35", "-m", "256", "-display", "sdl", "-S"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动失败");
    let pid = child.id();
    println!("  pid = {}", pid);

    // 直接调用生产代码里的函数，而不是在示例里复制一份逻辑——
    // 复制出来的验证等于没验证。
    println!("\n=== hide_until_attached（应在窗口出现后隐藏）===");
    let start = std::time::Instant::now();
    otools_vm_native::engine::qemu::embed::hide_until_attached(pid);
    println!("  耗时 {:?}", start.elapsed());

    println!("\n=== 检查窗口是否仍存在（隐藏≠销毁）===");
    // 用 embed 的 find 逻辑间接确认：attach 应能找到窗口
    let rect = otools_vm_native::engine::ViewRect {
        x: 10.0,
        y: 20.0,
        width: 800.0,
        height: 600.0,
        scale: 1.0,
        viewport_width: 1200.0,
        viewport_height: 800.0,
    };

    println!("\n=== attach_by_pid（找窗口 + SetParent 到宿主）===");
    // 注意：在示例进程里没有「宿主窗口」，host_hwnd 会取面积最大的有标题窗口，
    // 可能是任意应用。这里只验证「能走到 attach 的最后一步」，
    // 不验证嵌入位置——那需要真实宿主环境。
    match otools_vm_native::engine::qemu::embed::attach_by_pid(pid, rect) {
        Ok(()) => {
            println!("  attach 成功");
            println!("  embedded_pid = {:?}", otools_vm_native::engine::qemu::embed::embedded_pid());
            println!("\n=== update_rect（改尺寸）===");
            let mut resized = rect;
            resized.width = 1024.0;
            resized.height = 768.0;
            otools_vm_native::engine::qemu::embed::update_rect(resized);
            println!("  已更新");
            println!("\n=== detach（还原成顶层窗口）===");
            otools_vm_native::engine::qemu::embed::detach();
            println!("  detach 完成，embedded_pid = {:?}",
                otools_vm_native::engine::qemu::embed::embedded_pid());
        }
        Err(error) => println!("  attach 失败: {}", error),
    }

    std::thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();
    println!("\n已清理");
}
