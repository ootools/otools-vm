//! QEMU 后端：非 macOS 宿主，或需要跑非本机架构客户机时的回落路径。
//!
//! 与 VZ 后端的取舍：
//! - **能跑**：virtio-gpu + virtio-blk + virtio-net + 9p 共享目录，Linux 客户机开箱可用；
//! - **不能**：画面走 QEMU 自己的原生窗口（`-display cocoa/gtk/sdl`），
//!   因此剪贴板与「窗口缩放自动改分辨率」需要 SPICE 客户端，v1 不提供。
//!
//! 这条路径刻意保持「薄」：只做进程生命周期 + 参数拼装，不试图在插件里
//! 重写一个 VMM。Apple 芯片上的主力体验始终是 VZ。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use super::{Engine, ViewRect};
use crate::model::{EngineCaps, EngineInfo, GuestOs, VmSpec, VmState};

static CHILDREN: Mutex<Option<HashMap<String, Child>>> = Mutex::new(None);

fn with_children<R>(work: impl FnOnce(&mut HashMap<String, Child>) -> R) -> R {
    let mut guard = CHILDREN.lock().unwrap_or_else(|poison| poison.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    work(map)
}

pub struct QemuEngine {
    binary: PathBuf,
    version: String,
    arch: String,
}

impl QemuEngine {
    pub fn create_or_noop(_reason: String) -> Self {
        match Self::detect() {
            Some(engine) => engine,
            None => Self {
                binary: PathBuf::new(),
                version: String::new(),
                arch: String::new(),
            },
        }
    }

    fn detect() -> Option<Self> {
        let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
        let candidates = [
            format!("qemu-system-{}", arch),
            "/opt/homebrew/bin/qemu-system-aarch64".to_string(),
            "/usr/local/bin/qemu-system-aarch64".to_string(),
            "/opt/homebrew/bin/qemu-system-x86_64".to_string(),
            "/usr/local/bin/qemu-system-x86_64".to_string(),
            "qemu-system-x86_64".to_string(),
        ];
        for candidate in candidates {
            if let Ok(output) = Command::new(&candidate).arg("--version").output() {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout).to_string();
                    let version = text
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let resolved = resolve_binary(&candidate);
                    return Some(Self {
                        binary: resolved,
                        version,
                        arch: arch.to_string(),
                    });
                }
            }
        }
        None
    }

    fn is_available(&self) -> bool {
        !self.binary.as_os_str().is_empty()
    }

    fn display_arg() -> &'static str {
        if cfg!(target_os = "macos") {
            "cocoa"
        } else if cfg!(target_os = "windows") {
            "sdl"
        } else {
            "gtk"
        }
    }

    fn accelerator() -> &'static str {
        if cfg!(target_os = "macos") {
            "hvf"
        } else if cfg!(target_os = "windows") {
            "whpx"
        } else {
            "kvm"
        }
    }

    fn machine_type() -> &'static str {
        if cfg!(target_arch = "aarch64") {
            "virt"
        } else {
            "q35"
        }
    }

    fn build_args(&self, spec: &VmSpec) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        args.push("-name".into());
        args.push(spec.name.clone());
        args.push("-machine".into());
        args.push(format!("{},accel={}", Self::machine_type(), Self::accelerator()));
        if cfg!(target_arch = "aarch64") {
            args.push("-cpu".into());
            args.push("host".into());
        }
        args.push("-smp".into());
        args.push(spec.cpus.to_string());
        args.push("-m".into());
        args.push(spec.memory_mb.to_string());

        // 系统盘
        args.push("-drive".into());
        args.push(format!("file={},if=virtio,format=raw,cache=writethrough", spec.disk_path));

        // 安装介质
        if let Some(installer) = spec.installer_path.as_deref() {
            if !installer.trim().is_empty() {
                args.push("-drive".into());
                args.push(format!("file={},if=virtio,media=cdrom,readonly=on", installer));
            }
        }

        // 网络（用户态 NAT，免管理员权限）
        args.push("-netdev".into());
        args.push("user,id=net0".into());
        args.push("-device".into());
        args.push("virtio-net-pci,netdev=net0".into());

        // 显示与输入
        args.push("-device".into());
        args.push("virtio-gpu-pci".into());
        args.push("-device".into());
        args.push("usb-kbd".into());
        args.push("-device".into());
        args.push("usb-tablet".into());
        args.push("-device".into());
        args.push("virtio-rng-pci".into());
        if spec.audio {
            args.push("-audiodev".into());
            args.push("coreaudio,id=snd0".into());
            args.push("-device".into());
            args.push("virtio-sound-pci,audiodev=snd0".into());
        }

        // 共享目录（9p）：客户机 mount -t 9p -o trans=virtio <tag> /mnt/<tag>
        for dir in &spec.shared_dirs {
            let mut opts = format!(
                "local,path={},mount_tag={},security_model=none,multidevs=remap",
                dir.host_path, dir.mount_tag
            );
            if dir.read_only {
                opts.push_str(",readonly=on");
            }
            args.push("-virtfs".into());
            args.push(opts);
        }

        args.push("-display".into());
        args.push(Self::display_arg().into());

        args
    }
}

fn resolve_binary(candidate: &str) -> PathBuf {
    if candidate.contains('/') {
        return PathBuf::from(candidate);
    }
    if let Ok(output) = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", candidate))
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    PathBuf::from(candidate)
}

impl Engine for QemuEngine {
    fn info(&self) -> EngineInfo {
        if !self.is_available() {
            return EngineInfo {
                kind: "qemu".to_string(),
                available: false,
                label: "QEMU".to_string(),
                reason: "未找到 qemu-system-*。安装 QEMU 后重启宿主即可启用跨平台虚拟机。"
                    .to_string(),
                version: String::new(),
                caps: EngineCaps::unsupported(),
            };
        }
        EngineInfo {
            kind: "qemu".to_string(),
            available: true,
            label: "QEMU".to_string(),
            reason: String::new(),
            version: self.version.clone(),
            caps: EngineCaps {
                native_view: false,
                shared_dirs: true,
                shared_dirs_guests: vec!["linux".to_string()],
                clipboard: false,
                auto_resize: false,
                audio: true,
                snapshots: false,
                // 进程式后端没有写时复制，克隆退化为整盘复制（仍然可用）。
                clone: true,
                resize_disk: true,
                rosetta: false,
                bridged_network: true,
            },
        }
    }

    fn supports(&self, spec: &VmSpec) -> bool {
        self.is_available() && spec.arch == self.arch
    }

    fn start(&self, spec: &VmSpec) -> Result<(), String> {
        if !self.is_available() {
            return Err(self.info().reason);
        }
        spec.validate()?;
        if !std::path::Path::new(&spec.disk_path).exists() {
            return Err(format!("系统盘不存在：{}", spec.disk_path));
        }

        let mut running = false;
        with_children(|children| {
            if let Some(child) = children.get_mut(&spec.id) {
                if let Ok(None) = child.try_wait() {
                    running = true;
                }
            }
        });
        if running {
            return Err("虚拟机已在运行".to_string());
        }

        let args = self.build_args(spec);
        let log_path = crate::paths::vm_log_path(&spec.id);
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let log = std::fs::File::create(&log_path).ok();

        let mut command = Command::new(&self.binary);
        command.args(&args).stdin(Stdio::null());
        if let Some(file) = log {
            let cloned = file.try_clone().ok();
            command.stdout(file);
            if let Some(cloned) = cloned {
                command.stderr(cloned);
            }
        } else {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }

        let child = command
            .spawn()
            .map_err(|error| format!("启动 QEMU 失败: {}", error))?;
        with_children(|children| {
            children.insert(spec.id.clone(), child);
        });
        Ok(())
    }

    fn stop(&self, spec: &VmSpec) -> Result<(), String> {
        let mut found = false;
        with_children(|children| {
            if let Some(mut child) = children.remove(&spec.id) {
                found = true;
                let _ = child.kill();
                let _ = child.wait();
            }
        });
        if found {
            Ok(())
        } else {
            Ok(())
        }
    }

    fn pause(&self, _spec: &VmSpec) -> Result<(), String> {
        Err("QEMU 后端暂不支持暂停".to_string())
    }

    fn resume(&self, _spec: &VmSpec) -> Result<(), String> {
        Err("QEMU 后端暂不支持继续".to_string())
    }

    fn state(&self, spec: &VmSpec) -> VmState {
        with_children(|children| match children.get_mut(&spec.id) {
            Some(child) => match child.try_wait() {
                Ok(None) => VmState::Running,
                Ok(Some(_)) => {
                    children.remove(&spec.id);
                    VmState::Stopped
                }
                Err(_) => VmState::Error,
            },
            None => VmState::Stopped,
        })
    }

    fn last_error(&self, spec: &VmSpec) -> String {
        let path = crate::paths::vm_log_path(&spec.id);
        std::fs::read_to_string(path)
            .map(|text| {
                text.lines()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn attach_view(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Err("QEMU 后端的画面在 QEMU 自己的窗口里，不支持内嵌".to_string())
    }

    fn update_view_rect(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Ok(())
    }

    fn detach_view(&self, _spec: &VmSpec) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&self) {
        with_children(|children| {
            for (_, mut child) in children.drain() {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
    }
}

#[allow(dead_code)]
fn guest_is_linux(spec: &VmSpec) -> bool {
    matches!(spec.os, GuestOs::Linux)
}
