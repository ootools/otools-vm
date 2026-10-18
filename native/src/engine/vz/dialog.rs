//! 原生文件面板。
//!
//! 共享目录与安装镜像都需要「选一个真实路径」。插件跑在宿主进程里，
//! 用 `NSOpenPanel` 直接选，比走 webview 的 `input[type=file]` 靠谱：
//! 后者在 macOS 上拿不到真实文件系统路径（沙箱只给文件名 + 临时拷贝）。

use objc2::MainThreadMarker;
use objc2_app_kit::NSOpenPanel;
use objc2_foundation::NSString;

use super::on_main;

/// 模态返回码（NSModalResponseOK）。
const MODAL_RESPONSE_OK: isize = 1;

pub fn pick_directory(title: &str) -> Result<Option<String>, String> {
    let title = title.to_string();
    on_main(move || {
        let mtm = MainThreadMarker::new().ok_or_else(|| "必须在主线程打开文件面板".to_string())?;
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&NSString::from_str(&title)));
        if panel.runModal() != MODAL_RESPONSE_OK {
            return Ok(None);
        }
        Ok(first_path(&panel))
    })
}

pub fn pick_file(title: &str) -> Result<Option<String>, String> {
    let title = title.to_string();
    on_main(move || {
        let mtm = MainThreadMarker::new().ok_or_else(|| "必须在主线程打开文件面板".to_string())?;
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(false);
        panel.setCanChooseFiles(true);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&NSString::from_str(&title)));
        if panel.runModal() != MODAL_RESPONSE_OK {
            return Ok(None);
        }
        Ok(first_path(&panel))
    })
}

fn first_path(panel: &NSOpenPanel) -> Option<String> {
    let urls = panel.URLs();
    let first = urls.firstObject()?;
    Some(first.path()?.to_string())
}
