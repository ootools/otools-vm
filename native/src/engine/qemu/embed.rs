//! 把 QEMU 的显示窗口嵌进宿主面板（Windows）。
//!
//! 与 macOS 侧 `vz/view.rs` 是同一套思路，但机制完全不同：
//!
//! - macOS 有 `VZVirtualMachineView`，是可以直接 `addSubview` 的**原生视图**；
//! - Windows 上 QEMU 只会开一个**自己的顶层窗口**，我们拿不到任何视图对象。
//!
//! 所以这里走「窗口重定父」：`SetParent` 把 QEMU 的顶层窗口挂到宿主窗口下，
//! 再用 `MoveWindow` 让它精确覆盖前端报上来的那块矩形。这样画面由 SDL 直接
//! 画到 QEMU 窗口里，**不经过我们转发像素**——这是选它而不是 VNC 的全部理由：
//! 没有一次额外的拷贝与编码。
//!
//! ## 为什么必须能找到「哪个窗口是 QEMU 的」
//!
//! `SetParent` 需要目标 HWND。QEMU 不告诉我们它的窗口句柄，只能反查：
//! 用**子进程 PID** 匹配顶层窗口（`GetWindowThreadProcessId`）。这条路径比
//! 「按标题匹配」可靠——标题是我们自己传的 `-name`，用户改个名字就失配。
//!
//! ## 已知限制（都是有意的取舍）
//!
//! - 需要 QEMU 用能开窗的显示后端（`sdl`），不能用 `egl-headless`：
//!   后者根本不建窗口，没有 HWND 可挂。
//! - 因此 GL 与「内嵌」在 Windows 上**暂不能同时满足**。
//!   想要 GL 就得用 `egl-headless` + 自己取帧（VNC/SPICE），那是另一条路。
//!   这个取舍写在这里，避免下一个人误以为漏做了。

#![cfg(target_os = "windows")]

use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, MoveWindow,
    SetParent, SetWindowLongPtrW, ShowWindow, GWL_STYLE, SW_HIDE, SW_SHOW, WS_CAPTION, WS_CHILD,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
};

use super::super::ViewRect;

/// 一个已挂上的嵌入式子窗口。
struct Embedded {
    child: HWND,
    /// 被挂接的 QEMU 进程 pid，用于在重挂时判断是否是同一台机器。
    pid: u32,
}

// HWND 是个裸指针包装，跨线程传递由我们自己保证：所有操作都在持有
// `EMBEDDED` 锁的前提下进行，且只在需要时短暂进入。
unsafe impl Send for Embedded {}

static EMBEDDED: Mutex<Option<Embedded>> = Mutex::new(None);

/// 宿主窗口 HWND。
///
/// 插件跑在宿主进程里但拿不到 Tauri 的 Window 句柄，所以反查：
/// 枚举顶层窗口，找**可见、有标题、类名以 `Tauri` 开头**的那个。
/// Wry/WebView2 的窗口类在 tauri 里是 `Tauri Window`，但不同版本可能变，
/// 因此用前缀匹配并优先取前台/最大那个。
fn host_hwnd() -> Option<HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, IsWindowVisible,
    };

    struct Search {
        best: Option<(HWND, i32)>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: isize) -> i32 {
        let state = &mut *(lparam as *mut Search);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        // 没有标题的多半是内部辅助窗口（IME、托盘等），跳过。
        if GetWindowTextLengthW(hwnd) == 0 {
            return 1;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return 1;
        }
        let area = (rect.right - rect.left).max(0) * (rect.bottom - rect.top).max(0);
        // 取面积最大的有标题窗口：宿主主窗口必然是最大的那个。
        // 比「类名前缀匹配」稳，不依赖 Tauri 版本的内部命名。
        if state.best.map(|(_, best_area)| area > best_area).unwrap_or(true) {
            state.best = Some((hwnd, area));
        }
        1
    }

    let mut state = Search { best: None };
    unsafe {
        EnumWindows(Some(callback), &mut state as *mut Search as isize);
    }
    state.best.map(|(hwnd, _)| hwnd)
}

/// 找出属于指定进程的顶层窗口。
///
/// QEMU 起窗有延迟：进程启动后要等它初始化显示后端才创建窗口，所以这里带重试。
/// 不重试会稳定地拿不到句柄——而报错「找不到窗口」会让用户以为是环境问题。
///
/// **不过滤可见性**：`hide_until_attached` 会抢先把窗口藏起来（避免启动时
/// 闪一下），若这里要求 `IsWindowVisible`，隐藏之后 attach 就永远找不到它。
/// 「可见」是我们要改变的状态，不能拿来当筛选条件。
fn find_window_of_process(pid: u32, timeout_ms: u64) -> Option<HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows;

    struct Search {
        pid: u32,
        found: Option<(HWND, i32)>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: isize) -> i32 {
        let state = &mut *(lparam as *mut Search);
        let mut owner: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner != state.pid {
            return 1;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return 1;
        }
        let area = (rect.right - rect.left).max(0) * (rect.bottom - rect.top).max(0);
        // 隐藏窗口的 GetWindowRect 仍能返回原尺寸，只是面积可能为 0。
        // 用「有尺寸」而不是「可见」来排除 SDL 的辅助空窗口。
        if area <= 0 {
            return 1;
        }
        // SDL 可能建不止一个窗口，取面积最大的那个。
        if state.found.map(|(_, best)| area > best).unwrap_or(true) {
            state.found = Some((hwnd, area));
        }
        1
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let mut state = Search {
            pid,
            found: None,
        };
        unsafe {
            EnumWindows(Some(callback), &mut state as *mut Search as isize);
        }
        if let Some((hwnd, _)) = state.found {
            return Some(hwnd);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
}

/// 把 QEMU 窗口嵌进宿主面板的指定矩形。
///
/// 收 pid 而不是 `&Child`：进程的唯一持有者是调用方的 `CHILDREN` 表，
/// 这里不该再持有一份引用，否则生命周期会被两处同时管。
pub fn attach_by_pid(pid: u32, rect: ViewRect) -> Result<(), String> {
    attach_with_timeout(pid, rect, 8000)
}

/// 实际实现。`timeout_ms` 单独提出来的唯一目的是让测试能快速失败——
/// 单测里等 8 秒的失败路径会让整个测试套变得难以忍受。
fn attach_with_timeout(pid: u32, rect: ViewRect, timeout_ms: u64) -> Result<(), String> {
    if !rect.is_valid() {
        return Err("控制台区域尺寸无效，请先切到控制台再启动".to_string());
    }
    // 同一台机器重复 attach 时先摘掉旧的，避免留下孤儿子窗口。
    if let Some(existing) = embedded_pid() {
        if existing != pid {
            detach();
        }
    }
    let qemu = find_window_of_process(pid, timeout_ms).ok_or_else(|| {
        "找不到虚拟机的显示窗口。当前显示后端可能不创建窗口（如 egl-headless），\
         请改用能开窗的后端，或等待更久后重试"
            .to_string()
    })?;
    let parent = host_hwnd().ok_or_else(|| "找不到宿主窗口".to_string())?;

    unsafe {
        // 去掉顶层窗口的样式，加上子窗口样式。**顺序很关键**：
        // 先设 WS_CHILD 再 SetParent，否则窗口会保持顶层属性，
        // 表现为「挂上了但还是浮在屏幕中间」。
        // 先把窗口改成子窗口样式，再 SetParent。
        //
        // 顺序不能反：直接 SetParent 之后，被挂的窗口仍带着
        // `WS_POPUP | WS_CAPTION`，表现是「嵌进来了但还顶着一圈标题栏和边框」。
        // 必须把这些顶层样式剥掉，否则客户区里会出现一个带阴影的浮层。
        let style = GetWindowLongPtrW(qemu, GWL_STYLE) as u32;
        let cleaned = style
            & !(WS_POPUP
                | WS_CAPTION
                | WS_THICKFRAME
                | WS_SYSMENU
                | WS_MINIMIZEBOX
                | WS_MAXIMIZEBOX);
        SetWindowLongPtrW(qemu, GWL_STYLE, (cleaned as isize) | (WS_CHILD as isize) | (WS_VISIBLE as isize));

        if SetParent(qemu, parent).is_null() {
            return Err(format!(
                "把显示窗口挂到宿主失败（错误码 {}）",
                std::io::Error::last_os_error()
            ));
        }
        apply_rect(qemu, rect);
        ShowWindow(qemu, SW_SHOW);
    }

    if let Ok(mut guard) = EMBEDDED.lock() {
        *guard = Some(Embedded { child: qemu, pid });
    }
    Ok(())
}

/// 按前端上报的矩形摆放子窗口。
///
/// 坐标系换算与 macOS 侧相反：Win32 子窗口坐标相对**父窗口客户区左上角**，
/// 而前端给的 y 是相对 web 视口顶部，本来就都是「从上往下」，所以直接平移。
/// 前提是先找出 web 视口在宿主客户区里的偏移——顶部通常有标题栏。
fn apply_rect(child: HWND, rect: ViewRect) {
    let parent = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetParent(child) };
    if parent.is_null() {
        return;
    }
    // 客户区宽高：前端拿到的是 web 视口尺寸，可能因 DPI 缩放与客户区不等。
    // 用 viewport 尺寸做比例换算，比直接相信物理像素稳。
    let mut client: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetClientRect(parent, &mut client);
    }
    let client_h = (client.bottom - client.top).max(1) as f64;
    let scale = if rect.viewport_height > 0.0 {
        client_h / rect.viewport_height
    } else {
        1.0
    };

    let x = (rect.x * scale).round() as i32;
    let y = (rect.y * scale).round() as i32;
    let w = ((rect.width * scale).round() as i32).max(1);
    let h = ((rect.height * scale).round() as i32).max(1);
    unsafe {
        MoveWindow(child, x, y, w, h, 1);
    }
}

/// 抢在 attach 之前把 QEMU 的显示窗口藏起来。
///
/// QEMU 启动即建窗并弹到屏幕中央；而内嵌要等前端上报矩形。这段空窗期里
/// 用户会看到「窗口闪一下然后被吸进面板」。这里在后台等窗口出现就立刻藏掉。
///
/// **只藏不挂**：挂接要等真实的矩形，藏起来不会有副作用（attach 时会 SHOW）。
/// 超时也无需报错——真要拿不到窗口，attach 自己会给明确原因。
pub fn hide_until_attached(pid: u32) {
    if let Some(hwnd) = find_window_of_process(pid, 10_000) {
        // 已经挂上了就别再藏（attach 可能抢先跑完）。
        if embedded_pid() == Some(pid) {
            return;
        }
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// 按新矩形更新位置（拖动窗口、切换布局时调用）。
pub fn update_rect(rect: ViewRect) {
    let Ok(guard) = EMBEDDED.lock() else {
        return;
    };
    let Some(embedded) = guard.as_ref() else {
        return;
    };
    apply_rect(embedded.child, rect);
    // 显式忽略未使用警告式的取值：pid 在这里仅用于身份校验，见 detach。
    let _ = embedded.pid;
}

/// 摘下嵌入式子窗口，并把它还原成独立顶层窗口。
///
/// **不销毁窗口**：那会让 QEMU 失去显示目标，客户机画面直接断掉。
/// 还原成顶层窗口是最小的副作用——用户至少还能继续看到机器在跑。
pub fn detach() {
    let Ok(mut guard) = EMBEDDED.lock() else {
        return;
    };
    let Some(embedded) = guard.take() else {
        return;
    };
    unsafe {
        SetParent(embedded.child, std::ptr::null_mut());
        let style = GetWindowLongPtrW(embedded.child, GWL_STYLE) as u32;
        // 恢复成「能拖动、能关闭」的普通顶层窗口：只去掉 WS_CHILD，把
        // attach 时剥掉的标题栏/边框加回来。少了这步，还原出来的窗口
        // 没有边框也没有标题，用户既拖不动也关不掉。
        let restored = (style & !WS_CHILD)
            | WS_POPUP
            | WS_CAPTION
            | WS_THICKFRAME
            | WS_SYSMENU
            | WS_MINIMIZEBOX
            | WS_MAXIMIZEBOX
            | WS_VISIBLE;
        SetWindowLongPtrW(embedded.child, GWL_STYLE, restored as isize);
        // 兜底位置：屏幕左上角偏内一点，避免被任务栏盖住或跑到屏幕外。
        MoveWindow(embedded.child, 120, 90, 1024, 700, 1);
        ShowWindow(embedded.child, SW_SHOW);
    }
}

/// 判断某个 pid 当前是否挂着嵌入视图（换机器时用来跳过无效的 detach）。
pub fn embedded_pid() -> Option<u32> {
    EMBEDDED.lock().ok().and_then(|guard| guard.as_ref().map(|item| item.pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：矩形必须先通过 `is_valid` 才允许 attach。
    ///
    /// 前端在控制台尚未布局时会上报 0 尺寸（切 TAB 的瞬间就会发生）。
    /// 不挡这一层就会把一个 1×1 的子窗口挂上去，之后客户机一直显示 1 像素，
    /// 而用户以为机器坏了。
    #[test]
    fn invalid_rect_is_rejected() {
        let empty = ViewRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            scale: 1.0,
            viewport_width: 1200.0,
            viewport_height: 800.0,
        };
        let result = attach_with_timeout(999_999, empty, 0);
        assert!(result.is_err(), "零尺寸矩形必须被拒绝");
        assert!(
            result.unwrap_err().contains("尺寸无效"),
            "报错要说明是尺寸问题，而不是「找不到窗口」"
        );
    }

    /// 回归：找不到进程时给出可执行的原因，而不是笼统失败。
    ///
    /// 这条路径最常见的真实原因是「显示后端不建窗口」（egl-headless），
    /// 报错文案必须点出来，否则用户只会看到「找不到窗口」而无从下手。
    #[test]
    fn missing_window_error_is_actionable() {
        let rect = ViewRect {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
            scale: 1.0,
            viewport_width: 1200.0,
            viewport_height: 800.0,
        };
        // 用一个几乎不可能存在的 pid，并把重试窗口压到 0（不等）。
        let result = attach_with_timeout(0x7FFF_FFFE, rect, 0);
        match result {
            Ok(()) => panic!("不存在的进程不该 attach 成功"),
            Err(error) => {
                assert!(
                    error.contains("显示窗口") || error.contains("宿主窗口"),
                    "报错要指明缺的是窗口: {}",
                    error
                );
            }
        }
    }
}
