//! 真机验证：能否找到并重定父 QEMU 的 SDL 窗口。
//!
//! 这是 embed 模块最核心的假设——`SetParent` 对 SDL 创建的窗口有效。
//! 不实测就写完整实现，可能到最后才发现 SDL 用某种方式建了不可重定父的窗口。
//!
//! 用 `cargo run --example embed_probe` 运行。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
fn main() {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, IsWindowVisible, SetParent, GetWindowRect, GetWindowThreadProcessId,
        MoveWindow, GWL_STYLE, SetWindowLongPtrW, WS_CHILD,
    };

    struct Search {
        pid: u32,
        found: Option<(HWND, i32)>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: isize) -> i32 {
        let state = &mut *(lparam as *mut Search);
        let mut owner: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner != state.pid || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return 1;
        }
        let area = (rect.right - rect.left).max(0) * (rect.bottom - rect.top).max(0);
        if state.found.map(|(_, best)| area > best).unwrap_or(true) {
            state.found = Some((hwnd, area));
        }
        1
    }

    let qemu = r"C:\Program Files\qemu\qemu-system-x86_64.exe";
    println!("启动 QEMU (sdl 后端)…");
    let mut child = Command::new(qemu)
        .args(["-M", "q35", "-m", "256", "-display", "sdl", "-S"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动失败");
    let pid = child.id();
    println!("  pid = {}", pid);

    // 等窗口出现
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut window = None;
    while Instant::now() < deadline {
        let mut state = Search { pid, found: None };
        unsafe {
            EnumWindows(Some(callback), &mut state as *mut Search as isize);
        }
        if let Some((hwnd, area)) = state.found {
            window = Some((hwnd, area));
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    match window {
        Some((hwnd, area)) => {
            println!("  找到窗口 hwnd={:?} 面积={}", hwnd, area);
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetWindowRect(hwnd, &mut rect) };
            println!(
                "  窗口尺寸 = {}x{}",
                rect.right - rect.left,
                rect.bottom - rect.top
            );

            // 关键验证：SetParent 到一个测试父窗口（用桌面窗口代替宿主）
            let desktop = unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow()
            };
            println!("\n尝试 SetParent 到桌面窗口…");
            unsafe {
                let style =
                    windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWL_STYLE);
                SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !0x8000_0000isize) | (WS_CHILD as isize));
            }
            let result = unsafe { SetParent(hwnd, desktop) };
            println!(
                "  SetParent 返回 {:?} ⇒ {}",
                result,
                if result.is_null() { "失败" } else { "成功" }
            );

            unsafe {
                MoveWindow(hwnd, 40, 40, 800, 600, 1);
            }
            std::thread::sleep(Duration::from_millis(600));
            unsafe { GetWindowRect(hwnd, &mut rect) };
            println!(
                "  重定位后尺寸 = {}x{}（应为 800x600 附近）",
                rect.right - rect.left,
                rect.bottom - rect.top
            );
        }
        None => println!("  12 秒内没找到窗口 —— sdl 后端可能没建窗口"),
    }

    let _ = child.kill();
    let _ = child.wait();
    println!("\n已清理 QEMU 进程");
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("仅 Windows 需要验证");
}
