//! 磁盘准备：把 QEMU 时代的 RAW 镜像变成 HCS 能吃的 VHDX。
//!
//! HCS 的 `VirtualDisk` 附件只接受 VHD / VHDX，**不吃 RAW / qcow2**。而本插件
//! 一直以来创建的是 RAW 稀疏文件（QEMU 后端的默认格式）。两者不能直接互换。
//!
//! 处理策略按「代价从低到高」分三档，任一成立就用：
//! 1. 已经是 `.vhdx` —— 直接用。
//! 2. 原盘还是「刚创建、没装过系统」（体积小于阈值）—— 用系统自带 `diskpart`
//!    在同目录建一个等大的可扩展 VHDX，原来的 RAW 留着不删（用户可能还要
//!    切回 QEMU）。
//! 3. 原盘已有数据 —— 借助 `qemu-img` 转换一次（装了 QEMU 才有），转换结果
//!    缓存在同目录，下次直接命中。
//!
//! 三档都不成立时明确报错并给出可执行指令，而不是抛一句「配置无效」。
//! 之所以不自己写 VHDX 头：VHDX 的元数据区、BAT 表与校验和容错度很低，
//! 手写风险远高于调用系统组件。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 小于这个体积就认为盘里没有系统——空 RAW 稀疏文件通常只有几 MB。
const EMPTY_DISK_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;

/// 为 HCS 准备一块可用的 VHDX，返回其路径。
pub fn ensure_vhdx(disk_path: &str, size_gb: u64) -> Result<PathBuf, String> {
    let source = PathBuf::from(disk_path);
    if has_extension(&source, "vhdx") {
        if !source.exists() {
            create_blank_vhdx(&source, size_gb)?;
        }
        return Ok(source);
    }

    let target = source.with_extension("vhdx");
    if target.exists() {
        return Ok(target);
    }

    let source_size = std::fs::metadata(&source).map(|meta| meta.len()).unwrap_or(0);
    if source_size <= EMPTY_DISK_THRESHOLD_BYTES {
        // 空盘：直接建一块新的 VHDX，不需要搬运任何数据。
        create_blank_vhdx(&target, size_gb)?;
        return Ok(target);
    }

    // 有数据：只能靠 qemu-img 转。
    match convert_with_qemu_img(&source, &target, "vhdx") {
        Ok(()) => Ok(target),
        Err(reason) => Err(format!(
            "HCS 需要 VHDX 磁盘，但当前盘是 RAW 且无法自动转换（{}）。\n\
             两种办法：① 安装 QEMU 后重试（会用 qemu-img 自动转）；\
             ② 手动转换：qemu-img convert -O vhdx \"{}\" \"{}\"，\
             然后把虚拟机设置里的磁盘路径改成生成的 {}。",
            reason,
            source.display(),
            target.display(),
            target
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        )),
    }
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .map(|ext| ext.to_string_lossy().eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

/// 该路径是否是需要走 VHDX 专用扩容的虚拟磁盘。
///
/// 判断依据是扩展名而不是「HCS 在用什么」：调用方（`store::resize_disk`）
/// 拿到的可能是原始 `spec.disk_path`，而 HCS 实际挂载的是同名 `.vhdx`。
/// 只要看到虚拟磁盘扩展名，就**绝不能**用 `set_len`——那会把文件撑大但
/// 不更新 VHDX 元数据，客户机里看到的容量不变，宿主上却白占几十 GB。
pub fn is_virtual_disk(path: &Path) -> bool {
    has_extension(path, "vhdx") || has_extension(path, "vhd")
}

/// 扩容 VHDX/VHD 的虚拟容量。
///
/// 用 `diskpart` 的 `expand vdisk`：它会同时更新 VHDX 的元数据与 BAT 表，
/// 这是 `set_len` 做不到的。`diskpart` 是系统自带，不引入额外依赖。
///
/// ⚠️ 宿主侧扩完只是「块设备变大了」，客户机里还要自己扩分区/文件系统。
pub fn expand_vhdx(path: &Path, size_gb: u64) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("虚拟磁盘不存在：{}", path.display()));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("取当前目录失败: {}", error))?
            .join(path)
    };
    let size_mb = size_gb.max(1) * 1024;
    let script = format!(
        "select vdisk file=\"{}\"\nexpand vdisk maximum={}\n",
        absolute.display(),
        size_mb
    );

    let mut child = Command::new("diskpart")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 diskpart: {}", error))?;

    use std::io::Write;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|error| format!("写入 diskpart 脚本失败: {}", error))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("diskpart 执行失败: {}", error))?;

    // diskpart 退出码不可靠，所以还要在输出里找失败字样。
    // 「只能扩大」由调用方保证，这里只负责把 diskpart 的真实报错带出来。
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lower = stdout.to_ascii_lowercase();
    if output.status.success() && !lower.contains("error") && !lower.contains("失败") {
        return Ok(());
    }
    Err(format!(
        "扩容虚拟磁盘失败（{}）：{}",
        absolute.display(),
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .last()
            .unwrap_or("")
            .trim()
    ))
}

/// 用 `diskpart` 建一块可扩展（dynamic）VHDX。
///
/// 选它而不是自己写文件：VHDX 是带校验和的复合格式，`diskpart` 是系统自带、
/// 无需任何额外安装。`create vdisk ... type=expandable` 会立刻建好元数据，
/// 实际占用随写入增长。
fn create_blank_vhdx(path: &Path, size_gb: u64) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建目录失败 {}: {}", parent.display(), error))?;
    }
    // diskpart 不接受相对路径，也不接受未加引号的空格路径。
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("取当前目录失败: {}", error))?
            .join(path)
    };
    let size_mb = size_gb.max(1) * 1024;
    let script = format!(
        "create vdisk file=\"{}\" maximum={} type=expandable\n",
        absolute.display(),
        size_mb
    );

    // diskpart 从 stdin 读脚本；用管道比临时文件干净，也不留痕。
    let mut child = Command::new("diskpart")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 diskpart: {}", error))?;

    use std::io::Write;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|error| format!("写入 diskpart 脚本失败: {}", error))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("diskpart 执行失败: {}", error))?;

    if output.status.success() && path.exists() {
        return Ok(());
    }
    // diskpart 的退出码并不可靠（部分错误仍返 0），所以以「文件是否出现」为准。
    let message = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "创建 VHDX 失败（{}）：{}",
        absolute.display(),
        message.lines().last().unwrap_or("").trim()
    ))
}

/// 调 qemu-img 转换格式。
fn convert_with_qemu_img(source: &Path, target: &Path, format: &str) -> Result<(), String> {
    let binary = find_qemu_img().ok_or_else(|| "未找到 qemu-img".to_string())?;
    let output = Command::new(binary)
        .arg("convert")
        .arg("-O")
        .arg(format)
        .arg(source)
        .arg(target)
        .output()
        .map_err(|error| format!("调用 qemu-img 失败: {}", error))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn find_qemu_img() -> Option<String> {
    for candidate in [
        "qemu-img",
        "C:/Program Files/qemu/qemu-img.exe",
        "/opt/homebrew/bin/qemu-img",
        "/usr/local/bin/qemu-img",
        "/usr/bin/qemu-img",
    ] {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vhdx_is_recognised_case_insensitively() {
        assert!(has_extension(Path::new("C:/vm/disk.VHDX"), "vhdx"));
        assert!(has_extension(Path::new("C:/vm/disk.vhdx"), "vhdx"));
        assert!(!has_extension(Path::new("C:/vm/disk.raw"), "vhdx"));
        assert!(!has_extension(Path::new("C:/vm/disk"), "vhdx"));
    }

    /// 回归：虚拟磁盘绝不能被当成「长度即容量」的普通文件。
    ///
    /// `store::resize_disk` 用这个判定决定走 `diskpart expand` 还是 `set_len`。
    /// 判错就会静默撑大文件、客户机容量却不变。
    #[test]
    fn virtual_disks_are_not_plain_files() {
        assert!(is_virtual_disk(Path::new("C:/vm/disk.vhdx")));
        assert!(is_virtual_disk(Path::new("C:/vm/disk.VHD")));
        assert!(!is_virtual_disk(Path::new("C:/vm/disk.raw")));
        assert!(!is_virtual_disk(Path::new("C:/vm/disk.qcow2")));
        assert!(!is_virtual_disk(Path::new("C:/vm/disk")));
    }
}
