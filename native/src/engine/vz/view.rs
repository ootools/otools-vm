//! 把 `VZVirtualMachineView` 挂进宿主窗口。
//!
//! 客户机画面是原生 NSView，**不在 webview 里**：webview 只负责画外围 UI，
//! 中间留一块空白矩形，由前端把该矩形的 CSS 坐标报给 native，native 把
//! NSView 精确覆盖上去。这样键盘、鼠标、触控板、GPU 全走原生通路，
//! 延迟和 Parallels / UTM 同一量级。
//!
//! 坐标换算：CSS 像素 ≈ point（webview 缩放为 1），NSView 原点在左下，
//! 所以要按父视图高度做一次纵向翻转。

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_virtualization::{VZVirtualMachine, VZVirtualMachineView};

use super::super::ViewRect;

/// 宿主主窗口的内容视图。
///
/// 插件跑在宿主进程里，拿不到 Tauri 的 Window 句柄，只能从 NSApplication
/// 反查：key window → main window → 第一个可见窗口。
fn host_content_view() -> Option<Retained<NSView>> {
    let mtm = MainThreadMarker::new()?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);

    if let Some(window) = app.keyWindow() {
        if let Some(view) = window.contentView() {
            return Some(view);
        }
    }
    if let Some(window) = app.mainWindow() {
        if let Some(view) = window.contentView() {
            return Some(view);
        }
    }
    for window in app.windows().iter() {
        if window.isVisible() {
            if let Some(view) = window.contentView() {
                return Some(view);
            }
        }
    }
    None
}

/// 递归找窗口里的 WKWebView（宿主真正画 UI、挖出舞台孔洞的视图）。
///
/// 之所以不能直接贴 contentView 定位：宿主窗口顶部通常有原生 chrome /
/// webview 在 contentView 里有 inset，DOM 的 `getBoundingClientRect` 是以
/// webview 视口左上为原点的，只有锚到 webview 自身才能对齐（否则原生视图
/// 会被多翻一段、顶上去盖住上方按钮和文案）。找不到时返回 None，调用方回退。
fn find_webview(node: &NSView, depth: u32) -> Option<Retained<NSView>> {
    if depth > 12 {
        return None;
    }
    if is_webview_class(node) {
        // 节点归其父视图所有（attach 期间一定活着），这里 retain 一份独立引用带出递归。
        return unsafe { Retained::retain(node as *const NSView as *mut NSView) };
    }
    for child in node.subviews().iter() {
        if let Some(found) = find_webview(&child, depth + 1) {
            return Some(found);
        }
    }
    None
}

fn is_webview_class(view: &NSView) -> bool {
    view.class()
        .name()
        .to_string_lossy()
        .contains("WebView")
}

/// 把前端报来的 CSS 矩形换算成宿主窗口里 VZ 视图的 frame。
///
/// `rect` 是 **webview 视口坐标**（orig top-left，CSS px ≈ point）。
/// 当能找到承载插件 UI 的 webview 时，先用 `convertRect` 取它在
/// `parent`（contentView）里的真实位置，再做纵向翻转，自动消化宿主顶部
/// chrome / inset；找不到 webview 时退化为「webview == 整个 contentView」的旧算法。
fn frame_in_parent(parent: &NSView, webview: Option<&NSView>, rect: ViewRect) -> NSRect {
    let (origin_x, webview_top_from_bottom) = match webview {
        Some(w) => unsafe {
            let in_parent = parent.convertRect_fromView(w.frame(), w.superview().as_deref());
            (
                in_parent.origin.x,
                in_parent.origin.y + in_parent.size.height,
            )
        },
        None => (0.0, parent.bounds().size.height),
    };
    // 舞台顶边距 parent 底部 = webview 顶 - rect.y；再往下减 rect.height 得到原点的 y。
    let top = webview_top_from_bottom - rect.y;
    let y = top - rect.height;
    NSRect::new(
        NSPoint::new(origin_x + rect.x, y),
        NSSize::new(rect.width.max(1.0), rect.height.max(1.0)),
    )
}

pub fn create_view(
    mtm: MainThreadMarker,
    machine: &VZVirtualMachine,
    rect: ViewRect,
) -> Retained<VZVirtualMachineView> {
    let frame = NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.width.max(1.0), rect.height.max(1.0)),
    );
    let view = unsafe {
        VZVirtualMachineView::initWithFrame(VZVirtualMachineView::alloc(mtm), frame)
    };
    // 窗口拖动改变大小时自动向客户机下发新的显示模式 —— 这就是
    // 「拖动窗口自动改变分辨率」的全部实现，客户机侧无需装任何驱动
    // （Linux 需要内核 virtio-gpu 支持，Windows ARM 需要 virtio-gpu 驱动）。
    unsafe {
        view.setAutomaticallyReconfiguresDisplay(true);
        view.setVirtualMachine(Some(machine));
    }
    view
}

/// 内嵌到宿主窗口。返回被挂上去的 view。
pub fn attach_embedded(
    machine: &VZVirtualMachine,
    rect: ViewRect,
) -> Result<Retained<VZVirtualMachineView>, String> {
    if !rect.is_valid() {
        return Err("控制台区域尺寸无效，请先切到控制台再启动".to_string());
    }
    let mtm = MainThreadMarker::new().ok_or_else(|| "必须在主线程挂载视图".to_string())?;
    let parent = host_content_view().ok_or_else(|| "找不到宿主窗口".to_string())?;
    let webview = find_webview(&parent, 0);
    let view = create_view(mtm, machine, rect);
    let frame = frame_in_parent(&parent, webview.as_deref(), rect);
    view.setFrame(frame);
    view.setAutoresizingMask(NSAutoresizingMaskOptions::empty());
    parent.addSubview(&view);
    Ok(view)
}

/// 独立窗口模式：宿主窗口拿不到时（或用户主动分离）退化成自己的窗口。
pub fn attach_detached(
    machine: &VZVirtualMachine,
    rect: ViewRect,
) -> Result<(Retained<VZVirtualMachineView>, Retained<NSWindow>), String> {
    let mtm = MainThreadMarker::new().ok_or_else(|| "必须在主线程创建窗口".to_string())?;

    let frame = NSRect::new(
        NSPoint::new(80.0, 80.0),
        NSSize::new(rect.width.max(960.0), rect.height.max(640.0)),
    );
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    let view = create_view(mtm, machine, rect);
    window.setTitle(&objc2_foundation::NSString::from_str("OTools VM"));
    window.setContentView(Some(&view));
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&view));
    Ok((view, window))
}

pub fn resize(view: &VZVirtualMachineView, rect: ViewRect) {
    let Some(parent) = (unsafe { view.superview() }) else {
        return;
    };
    let webview = find_webview(&parent, 0);
    let frame = frame_in_parent(&parent, webview.as_deref(), rect);
    view.setFrame(frame);
}

/// 让客户机拿到键盘焦点。
pub fn focus(view: &VZVirtualMachineView) {
    let mtm = match MainThreadMarker::new() {
        Some(mtm) => mtm,
        None => return,
    };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    if let Some(window) = app.keyWindow() {
        window.makeFirstResponder(Some(view));
    }
}

pub fn detach(view: &VZVirtualMachineView) {
    unsafe { view.setVirtualMachine(None) };
    view.removeFromSuperview();
}
