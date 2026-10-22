//! 插件自己的系统文件 / 目录选择面板（Windows）。
//!
//! ## 为什么不能只依赖宿主
//!
//! 宿主的 `otools.dialog.open()` 内部先试 `plugin:dialog|open`，再回落
//! `tools_webview_pick_folder`，**两条路都用 `try/catch` 把异常吞成 `null`**
//! （见 `src/platform/runtime/otools/embedded-runtime.js` 的 `dialogApi`）。
//! 一旦底层桥不可用（例如 webview 里没有可用的 `__TAURI_INTERNALS__`），
//! 用户看到的就是「点了没反应」——前端拿到的 `null` 与「用户点了取消」
//! 完全无法区分。这种失败模式极难排查，也不该让用户承担。
//!
//! 插件 native 是**自己的进程内代码**，不需要任何宿主桥就能直接调
//! `comdlg32!GetOpenFileNameW` / `shell32!SHBrowseForFolderW`。这条路
//! 只依赖 Win32，宿主怎么变都不影响。所以 native 这一层必须能真的弹出面板，
//! 而不是像早期那样在非 macOS 上直接返回「不可用」。
//!
//! ## 线程与消息泵
//!
//! 两个 API 都是**模态**的：内部自建消息循环，直到用户确认/取消才返回。
//! 宿主通过 `tauri::async_runtime::spawn_blocking` 调插件 FFI（见主仓
//! `platform/native_plugins/transport.rs`），所以阻塞在这里不会卡住 UI 线程。
//!
//! 但它们**必须**在拥有窗口消息队列的线程上调用；插件 FFI 恰好跑在宿主
//! 的 blocking 线程池里，那是普通线程、没有消息队列，直接调会表现为面板
//! 一闪而过或干脆不显示。因此这里固定用 `std::thread::spawn` 起一个
//! **专属线程**去弹，再 `join` 等结果——线程退出时消息队列随之销毁，干净。

#![cfg(target_os = "windows")]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
};
use windows_sys::Win32::UI::Shell::{
    SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
};

/// 选择单个文件。取消返回 `Ok(None)`，与「面板打不开」区分开。
pub fn pick_file(title: &str, filter_name: &str, extensions: &[&str]) -> Result<Option<String>, String> {
    let title = title.to_string();
    let filter_name = filter_name.to_string();
    let extensions: Vec<String> = extensions.iter().map(|value| value.to_string()).collect();
    run_on_dialog_thread(move || native_open_file(&title, &filter_name, &extensions))
}

/// 选择目录（只允许选目录、且必须是文件系统路径）。
pub fn pick_directory(title: &str) -> Result<Option<String>, String> {
    let title = title.to_string();
    run_on_dialog_thread(move || native_browse_folder(&title))
}

/// 在专属线程上执行阻塞式面板调用，并把 `Option<String>` 结果带回来。
fn run_on_dialog_thread<F>(job: F) -> Result<Option<String>, String>
where
    F: FnOnce() -> Result<Option<String>, String> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("otools-vm-pick".to_string())
        .spawn(job)
        .map_err(|error| format!("无法创建选择面板线程: {}", error))?;
    handle
        .join()
        .map_err(|_| "选择面板线程异常退出".to_string())?
}

/// `GetOpenFileNameW`：系统「打开」对话框。
fn native_open_file(
    title: &str,
    filter_name: &str,
    extensions: &[String],
) -> Result<Option<String>, String> {
    // 过滤器格式是 `说明\0*.ext;*.ext\0\0`，双 NUL 结尾。
    let mut filter: Vec<u16> = Vec::new();
    let label = if filter_name.trim().is_empty() {
        "所有文件"
    } else {
        filter_name.trim()
    };
    filter.extend(label.encode_utf16());
    filter.push(0);
    for extension in extensions {
        let trimmed = extension.trim().trim_start_matches('.');
        if trimmed.is_empty() {
            continue;
        }
        filter.extend(format!("*.{};", trimmed).encode_utf16());
    }
    // 去掉末尾多余的 `;`
    if filter.last() == Some(&u16::from(b';')) {
        filter.pop();
    }
    filter.push(0);
    filter.push(0);

    let mut buffer = vec![0u16; 32 * 1024];
    let title_wide = to_wide(title);
    let label_wide = to_wide("所有文件 (*.*)");

    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: buffer.as_mut_ptr(),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: title_wide.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // `Default` 给的是全零；过滤器为空时给个兜底，避免用户看不到文件。
    if filter.len() <= 2 {
        options.lpstrFilter = label_wide.as_ptr();
    }

    let ok = unsafe { GetOpenFileNameW(&mut options) };
    if ok == 0 {
        // 取消：CommDlgExtendedError 为 0 表示用户主动取消，不是失败。
        return Ok(None);
    }

    let end = buffer.iter().position(|value| *value == 0).unwrap_or(buffer.len());
    let picked = OsString::from_wide(&buffer[..end]).to_string_lossy().to_string();
    let trimmed = picked.trim().to_string();
    Ok(if trimmed.is_empty() { None } else { Some(trimmed) })
}

/// `SHBrowseForFolderW`：老式「浏览文件夹」对话框。
///
/// 不用 `IFileOpenDialog`（Vista+ 的新式目录选择器）是因为它需要 COM 接口
/// 绑定，`windows-sys` 不提供 `IFileOpenDialog` 的定义，自己写 vtable 得不偿失。
/// `SHBrowseForFolderW` 配 `BIF_NEWDIALOGSTYLE` 已经是可调整大小、带「新建文件夹」
/// 的够用版本。
fn native_browse_folder(title: &str) -> Result<Option<String>, String> {
    let title_wide = to_wide(title);
    let mut display_name = vec![0u16; 512];

    let mut info = BROWSEINFOW {
        hwndOwner: std::ptr::null_mut(),
        pidlRoot: std::ptr::null_mut(),
        pszDisplayName: display_name.as_mut_ptr(),
        lpszTitle: title_wide.as_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..Default::default()
    };

    let pidl = unsafe { SHBrowseForFolderW(&mut info) };
    if pidl.is_null() {
        return Ok(None);
    }

    let mut path_buffer = vec![0u16; 32 * 1024];
    let ok = unsafe { SHGetPathFromIDListW(pidl, path_buffer.as_mut_ptr()) };
    // pidl 由 shell 分配，必须归还，否则每选一次泄漏一块。
    unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(pidl as *const core::ffi::c_void) };

    if ok == 0 {
        return Err("无法把所选目录转换成路径".to_string());
    }

    let end = path_buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(path_buffer.len());
    let picked = PathBuf::from(OsString::from_wide(&path_buffer[..end]));
    let text = picked.to_string_lossy().trim().to_string();
    Ok(if text.is_empty() { None } else { Some(text) })
}

/// Rust `str` → NUL 结尾的 UTF-16，供 Win32 `W` 系列 API 使用。
fn to_wide(value: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_string_is_nul_terminated() {
        let wide = to_wide("vms");
        assert_eq!(wide, vec![0x76, 0x6d, 0x73, 0]);
    }

    #[test]
    fn empty_title_still_valid_wide() {
        assert_eq!(to_wide(""), vec![0]);
    }
}
