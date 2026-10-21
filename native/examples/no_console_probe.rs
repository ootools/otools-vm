//! 验证子进程到底有没有被分配控制台。
//!
//! 上一种做法（枚举可见窗口做前后对比）是**无效的**：控制台窗口从创建到
//! 关闭只有几十毫秒，靠「spawn 后 sleep 再数窗口」根本抓不住，对照组和实验组
//! 都会读到 0 —— 那种验证等于没验证。
//!
//! 这里换成正向判据：直接问内核「这个进程有没有控制台」
//! （`AttachConsole` 能否挂上去），以及它的控制台窗口是否可见。
//!
//! 用 `cargo run --example no_console_probe` 运行。

#[cfg(target_os = "windows")]
mod win {
    // 手写 extern 声明，避免为「只在这个验证示例里用一次」的 API 去改
    // Cargo.toml 的 feature 列表。
    #[link(name = "kernel32")]
    extern "system" {
        pub fn AttachConsole(process_id: u32) -> i32;
        pub fn FreeConsole() -> i32;
        pub fn GetConsoleWindow() -> *mut core::ffi::c_void;
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn IsWindowVisible(hwnd: *mut core::ffi::c_void) -> i32;
    }
}

#[cfg(target_os = "windows")]
fn probe_console(program: &str, use_helper: bool) -> (bool, bool) {
    use std::process::Stdio;

    // 先把自己从任何控制台里摘出来，否则 AttachConsole 的行为会被自身干扰。
    unsafe {
        win::FreeConsole();
    }

    let mut command = if use_helper {
        otools_vm_native::process::command(program)
    } else {
        std::process::Command::new(program)
    };
    command
        .args(["/C", "ping", "-n", "4", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = command.spawn().expect("起进程失败");
    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(400));

    // 挂得上说明它有一个控制台。
    let attached = unsafe { win::AttachConsole(pid) };
    let has_console = attached != 0;

    // 挂上之后看它的控制台窗口是否可见 —— 这正是用户「看到黑框」的判据。
    let window_visible = if has_console {
        let hwnd = unsafe { win::GetConsoleWindow() };
        unsafe {
            win::FreeConsole();
        }
        !hwnd.is_null() && unsafe { win::IsWindowVisible(hwnd) } != 0
    } else {
        false
    };

    let _ = child.kill();
    let _ = child.wait();
    (has_console, window_visible)
}

fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("判据：子进程是否被分配控制台 / 该控制台窗口是否可见\n");

        let (raw_console, raw_visible) = probe_console("cmd", false);
        println!("裸 std::process::Command：");
        println!("  有控制台 = {}", raw_console);
        println!("  控制台窗口可见 = {}  <- 用户看到的就是这个", raw_visible);

        let (fixed_console, fixed_visible) = probe_console("cmd", true);
        println!("\ncrate::process::command（CREATE_NO_WINDOW）：");
        println!("  有控制台 = {}", fixed_console);
        println!("  控制台窗口可见 = {}", fixed_visible);

        println!("\n=== 结论 ===");
        // 关键判据是**窗口是否可见**，不是「有没有控制台」。
        // `CREATE_NO_WINDOW` 的语义是「创建但不显示窗口」，子进程仍可能被
        // 分配一个控制台对象（某些工具需要它才能正常工作），只是用户看不见。
        // 我把这点搞错过一次：断言「不该有控制台」会误报失败。
        if raw_console && raw_visible && !fixed_visible {
            println!("✓ 修复生效：裸调用会弹出可见控制台，修复后不可见");
        } else if !raw_visible {
            println!("⚠ 对照组也没观察到可见控制台，本机环境无法区分（不影响修复本身）");
        } else {
            println!("✗ 修复未生效：修复后仍有可见控制台窗口");
        }
        assert!(!fixed_visible, "CREATE_NO_WINDOW 下不该有可见控制台窗口");
    }

    #[cfg(not(target_os = "windows"))]
    println!("仅 Windows 有控制台弹窗问题");
}
