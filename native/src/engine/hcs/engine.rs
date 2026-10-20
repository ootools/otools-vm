//! HCS 后端：Windows 上的原生虚拟化（走 Host Compute Service）。
//!
//! 与 QEMU 后端的取舍（都是硬约束，不是暂时没做）：
//! - **能跑**：UEFI 引导、SCSI 磁盘与光驱、CPU/内存热调、暂停/继续、
//!   优雅关机与强制关闭。CPU 由 Hyper-V 直接执行，性能接近原生。
//! - **不能**：**拿不到客户机画面**。HCS 不提供 framebuffer / 截屏 API，
//!   商业实现（如 App Sandbox）都要自己写签名内核显示驱动。因此
//!   `native_view` 能力恒为 false，前端不该指望内嵌视图。
//! - **不能**：共享目录（HCS 的 Plan9 共享只服务容器场景）、USB 直通、
//!   快照存档（HCS 有 `HcsSaveComputeSystem`，但恢复需配套运行时状态文件，
//!   v1 不做）。
//! - **磁盘只吃 VHDX**，见 `disk` 模块。
//! - **网络走 HCN**，与 QEMU 的 `-netdev` 模型完全不同，v1 只做「不配网络」，
//!   即客户机无网卡；接 HCN 需要额外的命名空间与 NAT 编排。
//!
//! 权限是硬门槛：HCS 只允许管理员或 Hyper-V Administrators 组成员。探测阶段
//! 就会拦下并给出可操作提示，不做「先失败再猜」。

use std::collections::HashMap;
use std::sync::Mutex;

use super::super::{Engine, ViewRect};
use super::{config, disk, ffi};
use crate::model::{EngineCaps, EngineInfo, VmSpec, VmState};

/// 本插件创建的虚拟机 id 前缀。
///
/// 用作「是不是我们自己的机器」的判据：HCS 是整机共享的服务，用户可能还有
/// 别的程序（WSL2、Docker、Windows Sandbox）在同一台机上跑，绝不能误杀。
const ID_PREFIX: &str = "otools-vm-";

/// 一台由本进程托管的 HCS 虚拟机。
struct ManagedVm {
    system: ffi::System<'static>,
    /// 本次创建的 VHDX 是否为临时生成（停止后不删，用户可能要继续用；
    /// 保留字段以便将来做清理策略）。
    #[allow(dead_code)]
    prepared_disk: Option<std::path::PathBuf>,
    last_error: String,
    paused: bool,
}

// `ffi::System` 内部是裸句柄 + &'static Api；句柄本身可跨线程使用
// （HCS API 明确支持多线程调用），故显式声明。
unsafe impl Send for ManagedVm {}
unsafe impl Sync for ManagedVm {}

static CHILDREN: Mutex<Option<HashMap<String, ManagedVm>>> = Mutex::new(None);

fn with_machines<R>(work: impl FnOnce(&mut HashMap<String, ManagedVm>) -> R) -> R {
    let mut guard = CHILDREN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    work(map)
}

pub struct HcsEngine {
    pub availability: super::Availability,
}

impl HcsEngine {
    /// 探测并构造。永远返回一个引擎：不可用时 `info()` 会如实报告原因，
    /// 而不是让上层拿到 `None` 后再去编一段话。
    pub fn create() -> Self {
        Self {
            availability: super::probe(),
        }
    }

    /// 该平台是否可能用 HCS（编译期判定）。
    pub fn is_supported_platform() -> bool {
        cfg!(target_os = "windows")
    }

}

/// 把 HCS 的状态文档翻成内部状态。
///
/// 文档形如 `{"State":"Running"}`；只匹配 ASCII 关键字，避免受系统语言影响。
/// 空串（查不到）按 `Stopped` 处理不够准确，但调用方在查不到时会先兜一层
/// 「句柄还在就认为 Running」，所以这里只负责能认出关键字的情况。
pub fn map_state(raw: &str) -> VmState {
    let lowered = raw.to_ascii_lowercase();
    if lowered.contains("\"running\"") || lowered.contains("\"runningfrozen\"") {
        VmState::Running
    } else if lowered.contains("\"paused\"") {
        VmState::Paused
    } else if lowered.contains("\"starting\"") {
        VmState::Starting
    } else if lowered.contains("\"stopping\"") || lowered.contains("\"stopped\"") {
        VmState::Stopping
    } else {
        VmState::Stopped
    }
}

impl Engine for HcsEngine {
    fn info(&self) -> EngineInfo {
        let available = self.availability.available;
        EngineInfo {
            kind: "hcs".to_string(),
            available,
            label: "HCS".to_string(),
            reason: self.availability.reason.clone(),
            version: self.availability.version.clone(),
            // 这个字段在 VZ 的语义里是「进程有没有虚拟化权限」。
            // HCS 的对应概念就是「有没有提权」，直接映射过去，前端不必区分引擎。
            host_entitled: available,
            accelerator: if available {
                Some("hyperv".to_string())
            } else {
                None
            },
            caps: EngineCaps {
                // HCS 没有 framebuffer API，画面拿不到——这是本后端最大的能力缺口。
                native_view: false,
                shared_dirs: false,
                shared_dirs_guests: Vec::new(),
                clipboard: false,
                auto_resize: false,
                audio: false,
                snapshots: false,
                clone: false,
                resize_disk: false,
                rosetta: false,
                bridged_network: false,
                // HCS 没有 framebuffer 接口，画面拿不到——本后端最大的能力缺口。
                display_output: false,
            },
        }
    }

    fn supports(&self, spec: &VmSpec) -> bool {
        // HCS 跑的是本机架构的客户机；跨架构（如 x86 宿主跑 arm64）它做不到。
        let host_arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x86_64"
        };
        spec.arch == host_arch
    }

    fn start(&self, spec: &VmSpec) -> Result<(), String> {
        if !self.availability.available {
            return Err(if self.availability.reason.is_empty() {
                "HCS 不可用".to_string()
            } else {
                self.availability.reason.clone()
            });
        }

        let api = match ffi::api() {
            Ok(api) => api,
            Err(reason) => return Err(reason.clone()),
        };

        if with_machines(|machines| machines.contains_key(&spec.id)) {
            return Err("该虚拟机已在运行".to_string());
        }

        // 磁盘必须是 VHDX；RAW 会在这里被转好或给出可执行指令。
        let disk_path = disk::ensure_vhdx(&spec.disk_path, spec.disk_size_gb)?;
        let disk_text = disk_path.to_string_lossy().to_string();
        if !config::disk_is_compatible(&disk_text) {
            return Err("HCS 只支持 VHDX 磁盘".to_string());
        }

        // 安装介质：仅在文件确实存在时挂上，避免因一处路径笔误让整机起不来。
        let installer = spec
            .installer_path
            .as_ref()
            .filter(|path| std::path::Path::new(path).exists());
        let document = config::machine_document_text(spec, &disk_text, installer.map(|s| s.as_str()));

        let id = format!("{}{}", ID_PREFIX, crate::paths::sanitize_id(&spec.id));
        let system = ffi::System::create(api, &id, &document)?;
        system.start()?;

        with_machines(|machines| {
            machines.insert(
                spec.id.clone(),
                ManagedVm {
                    system,
                    prepared_disk: Some(disk_path),
                    last_error: String::new(),
                    paused: false,
                },
            );
        });
        Ok(())
    }

    fn stop(&self, spec: &VmSpec) -> Result<(), String> {
        let managed = with_machines(|machines| machines.remove(&spec.id));
        let Some(managed) = managed else {
            return Ok(());
        };
        // 先试优雅关机，失败再强杀——强杀会丢客户机未落盘的数据。
        if let Err(graceful) = managed.system.shutdown() {
            managed
                .system
                .terminate()
                .map_err(|force| format!("{}；强制关闭也失败：{}", graceful, force))?;
        }
        Ok(())
    }

    fn pause(&self, spec: &VmSpec) -> Result<(), String> {
        with_machines(|machines| {
            let Some(managed) = machines.get_mut(&spec.id) else {
                return Err("该虚拟机未在运行".to_string());
            };
            managed.system.pause()?;
            managed.paused = true;
            Ok(())
        })
    }

    fn resume(&self, spec: &VmSpec) -> Result<(), String> {
        with_machines(|machines| {
            let Some(managed) = machines.get_mut(&spec.id) else {
                return Err("该虚拟机未在运行".to_string());
            };
            managed.system.resume()?;
            managed.paused = false;
            Ok(())
        })
    }

    fn state(&self, spec: &VmSpec) -> VmState {
        if !self.availability.available {
            return VmState::Unavailable;
        }
        with_machines(|machines| {
            let Some(managed) = machines.get(&spec.id) else {
                return VmState::Stopped;
            };
            if managed.paused {
                return VmState::Paused;
            }
            let properties = managed.system.properties(config::properties_query());
            if properties.is_empty() {
                // 查不到属性不代表机器没了：句柄还在说明它仍归我们托管。
                return VmState::Running;
            }
            map_state(&properties)
        })
    }

    fn last_error(&self, spec: &VmSpec) -> String {
        with_machines(|machines| {
            machines
                .get(&spec.id)
                .map(|managed| managed.last_error.clone())
                .unwrap_or_default()
        })
    }

    fn attach_view(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Err("HCS 后端不提供客户机画面：它没有 framebuffer 接口".to_string())
    }

    fn update_view_rect(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Ok(())
    }

    fn detach_view(&self, _spec: &VmSpec) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&self) {
        let drained = with_machines(|machines| {
            machines
                .drain()
                .map(|(_, managed)| managed)
                .collect::<Vec<_>>()
        });
        for managed in drained {
            let _ = managed.system.shutdown();
            let _ = managed.system.terminate();
        }
    }
}
