//! 诊断：探测函数内部到底卡在哪一步。
//!
//! 用 `cargo run --example probe_gl_diag` 运行。不进发布包。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    println!("=== 1. PATH 里能否直接调用 qemu ===");
    match Command::new("qemu-system-x86_64").arg("--version").output() {
        Ok(out) => println!(
            "  可调用，success={}，首行={:?}",
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("")
        ),
        Err(err) => println!("  不可调用: {}", err),
    }

    println!("\n=== 2. 直接用绝对路径起 egl-headless，看多久退出 ===");
    let path = r"C:\Program Files\qemu\qemu-system-x86_64.exe";
    let start = Instant::now();
    let mut child = match Command::new(path)
        .args([
            "-M", "q35", "-m", "128",
            "-device", "virtio-vga-gl",
            "-display", "egl-headless,gl=on",
            "-S",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            println!("  启动失败: {}", err);
            return;
        }
    };

    // 给它时间初始化 GL：失败会立刻退出，成功则会一直挂着（-S 暂停态）。
    std::thread::sleep(Duration::from_millis(1200));
    match child.try_wait() {
        Ok(Some(status)) => {
            println!("  已退出（{:?}），说明初始化失败", status);
            let out = child.wait_with_output().expect("收尾失败");
            println!("  stdout: {}", String::from_utf8_lossy(&out.stdout).trim());
            println!("  stderr: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(None) => {
            println!("  仍在运行（耗时 {:?}）⇒ GL 初始化成功", start.elapsed());
            let _ = child.kill();
        }
        Err(err) => println!("  try_wait 失败: {}", err),
    }

    println!("\n=== 3. 对照：-display none 配 virtio-vga-gl 应当立刻报错 ===");
    let mut child = Command::new(path)
        .args(["-M", "q35", "-m", "128", "-device", "virtio-vga-gl", "-display", "none", "-S"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动失败");
    std::thread::sleep(Duration::from_millis(1200));
    match child.try_wait() {
        Ok(Some(status)) => {
            println!("  已退出（{:?}）——符合预期", status);
            let out = child.wait_with_output().expect("收尾失败");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            println!("  输出: {}", text.trim().lines().next().unwrap_or(""));
        }
        Ok(None) => {
            println!("  仍在运行 —— 不符合预期，说明判定方式不可靠");
            let _ = child.kill();
        }
        Err(err) => println!("  try_wait 失败: {}", err),
    }

    println!("\n=== 4. 反面验证：设备名写错时，探测必须返回 false ===");
    // 这是防「探测恒为 true」的护栏：如果判据只看「起了进程」，写错设备名
    // 也会因为进程存活被判成功，前端就会给一个实际不可用的开关。
    let mut child = Command::new(path)
        .args([
            "-M", "q35", "-m", "128",
            "-device", "virtio-gpu-gl-nonexistent",
            "-display", "egl-headless,gl=on",
            "-S",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动失败");
    std::thread::sleep(Duration::from_millis(1200));
    let exited = matches!(child.try_wait(), Ok(Some(_)));
    println!("  进程是否已退出（应为 true）: {}", exited);
    let _ = child.kill();
}
