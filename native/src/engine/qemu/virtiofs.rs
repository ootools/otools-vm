//! virtio-fs 共享目录：宿主侧 daemon 生命周期 + 客户机侧挂载参数。
//!
//! ## 为什么只有 Linux 宿主能用
//! virtio-fs 需要宿主跑 `virtiofsd`（vhost-user 后端）。上游 virtiofsd 依赖
//! memfd / seccomp / 命名空间，只支持 Linux；QEMU 官方 Windows 与 macOS 包都不
//! 带它，也没有可用的第三方构建。同时 QEMU 侧的 `memory-backend-memfd` 同样
//! 只有 Linux 有。
//!
//! 因此本模块在非 Linux 宿主上**提前**返回可操作的错误，而不是让 QEMU 报一串
//! 「vhost-user-fs-pci: ...」的设备错误让用户去猜。
//!
//! ## 一个共享目录一个 daemon
//! virtiofsd 一次只能导出一个目录（README: "only supports sharing a single
//! directory"），所以每个 virtio-fs 共享项都要起独立 daemon + 独立 socket。
//!
//! ## 客户机侧
//! - Linux：`mount -t virtiofs <tag> /mnt/<tag>`
//! - Windows：virtio-win 的 viofs 驱动 + WinFsp，`virtiofs.exe -m Z: -t <tag>`，
//!   或装好 VirtIO-FS Service 后自动出现 Z: 盘。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::{SharedDir, VmSpec};
use crate::paths;

/// 协议常量以 `model` 为单一来源（`VmSpec` 的校验要用同一套值），这里转出去供本模块引用。
pub use crate::model::{
    SHARED_PROTOCOL_9P as PROTOCOL_9P, SHARED_PROTOCOL_VIRTIOFS as PROTOCOL_VIRTIOFS,
};

/// 该共享项是否走 virtio-fs。
pub fn is_virtiofs(dir: &SharedDir) -> bool {
    dir.protocol == PROTOCOL_VIRTIOFS
}

/// 规格里是否有任何 virtio-fs 共享项。
pub fn spec_uses_virtiofs(spec: &VmSpec) -> bool {
    spec.shared_dirs.iter().any(is_virtiofs)
}

/// 当前宿主能否跑 virtio-fs（daemon 与 memfd 都只有 Linux 有）。
pub fn host_supports_virtiofs() -> bool {
    cfg!(target_os = "linux")
}

/// virtio-fs 在当前宿主不可用的原因。文案直接展示给用户，要说清「为什么」和「那怎么办」。
pub fn unsupported_reason() -> String {
    if cfg!(target_os = "windows") {
        "virtio-fs 需要宿主运行 virtiofsd，但官方 QEMU Windows 包不包含该程序（virtiofsd 仅支持 Linux），因此 Windows 宿主无法使用 virtio-fs。Linux 客户机请改用 9p 协议。".to_string()
    } else if cfg!(target_os = "macos") {
        "virtio-fs 需要 Linux 宿主的 memfd 共享内存与 virtiofsd，macOS 宿主不支持。Linux 客户机请改用 9p 协议。".to_string()
    } else {
        "当前宿主不支持 virtio-fs，Linux 客户机请改用 9p 协议。".to_string()
    }
}

/// 解析 virtiofsd 可执行文件：用户在设置里显式指定的路径优先，否则按常见安装位置探测。
pub fn resolve_daemon(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(path) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!("virtiofsd 路径不存在：{}", path));
    }
    for candidate in daemon_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(
        "未找到 virtiofsd。请先安装（Fedora：dnf install virtiofsd；Debian/Ubuntu：apt install virtiofsd），或在虚拟机设置里指定 virtiofsd 可执行文件路径。"
            .to_string(),
    )
}

/// 各发行版常见的 virtiofsd 安装位置。
fn daemon_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/libexec/virtiofsd"),
        PathBuf::from("/usr/lib/virtiofsd"),
        PathBuf::from("/usr/lib/qemu/virtiofsd"),
        PathBuf::from("/usr/local/libexec/virtiofsd"),
        PathBuf::from("/usr/local/bin/virtiofsd"),
        PathBuf::from("/usr/bin/virtiofsd"),
    ]
}

/// 某个共享项的 vhost-user socket 路径。
///
/// 放在该虚拟机自己的数据目录下：虚拟机删掉时 socket 跟着消失，不会在系统里留垃圾。
pub fn socket_path(vm_id: &str, mount_tag: &str) -> PathBuf {
    paths::vm_dir(vm_id).join(format!("virtiofs-{}.sock", sanitize_tag(mount_tag)))
}

fn sanitize_tag(tag: &str) -> String {
    tag.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// 一个被本进程托管的 virtiofsd。随虚拟机停止而退出。
pub struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    /// 强杀并清理 socket 文件。virtiofsd 退出时不会自己删 socket。
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// 起一个 virtiofsd，并等它的 socket 就绪后返回（QEMU 会去连这个 socket）。
pub fn start(spec: &VmSpec, dir: &SharedDir, daemon: &PathBuf) -> Result<Daemon, String> {
    let socket = socket_path(&spec.id, &dir.mount_tag);
    if let Some(parent) = socket.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 上次异常退出可能留下 socket 文件，而 virtiofsd 不会覆盖已存在的路径。
    let _ = std::fs::remove_file(&socket);

    let mut command = Command::new(daemon);
    command
        .arg("--socket-path")
        .arg(&socket)
        .arg("--shared-dir")
        .arg(&dir.host_path)
        // 桌面 Linux 通常不以 root 运行，namespace 沙箱会因缺权限失败；
        // `none` 是官方为非特权用户提供的选项（README: Running as non-privileged user）。
        .arg("--sandbox=none")
        .arg("--log-level=info");
    // 只读共享由 daemon 侧强制：QEMU 的 vhost-user-fs-pci 没有只读属性。
    if dir.read_only {
        command.arg("--readonly");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = command
        .spawn()
        .map_err(|error| format!("启动 virtiofsd 失败: {}", error))?;

    // QEMU 起来后会立刻连 socket，等它就位再放行；daemon 提前退出也要尽早报出来。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if socket.exists() {
            return Ok(Daemon { child, socket });
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = std::fs::remove_file(&socket);
                return Err(format!(
                    "virtiofsd 启动后立即退出（{}）。共享目录：{}",
                    status, dir.host_path
                ));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&socket);
            return Err("virtiofsd 未能在 5 秒内创建 socket，请检查 virtiofsd 是否能正常运行"
                .to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
