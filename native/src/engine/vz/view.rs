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

/// 把 CSS 矩形换算成父视图坐标系里的 frame。
fn to_view_frame(parent: &NSView, rect: ViewRect) -> NSRect {
    let parent_height = parent.bounds().size.height;
    let y = parent_height - rect.y - rect.height;
    NSRect::new(
        NSPoint::new(rect.x, y),
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
    let view = create_view(mtm, machine, rect);
    let frame = to_view_frame(&parent, rect);
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

pub fn resize(view: &VZVirtualMachineView, parent: &NSView, rect: ViewRect) {
    let frame = to_view_frame(parent, rect);
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
