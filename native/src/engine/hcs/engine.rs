//! HCS 后端：Windows 上的原生虚拟化（走 Host Compute Service）。
//!
//! 与 QEMU 后端的取舍：
//! - **能跑**：UEFI 引导、SCSI 磁盘与光驱、CPU/内存、暂停/继续、
//!   优雅关机与强制关闭、NAT 网络。CPU 由 Hyper-V 直接执行，性能接近原生。
//! - **能接管**：进程重启后可以用 `HcsOpenComputeSystem` 重新拿到上次遗留
//!   虚拟机的句柄，不必把它们留成界面上看不见的孤儿。
//! - **不能**：拿不到客户机画面。HCS 不提供 framebuffer 接口——schema 2.5 里
//!   确实有 `ConsoleImageQuery`，但它只出现在 `ProcessModifyRequest` 的
//!   `ConsoleSize` 语境下，`System_PropertyType` 里没有对应变体，也就是说
//!   **没有公开的取图入口**。商业实现（如 App Sandbox）都要自带签名内核
//!   显示驱动。因此 `native_view` / `display_output` 恒为 false。
//! - **不能**：共享目录（HCS 的 Plan9 共享只服务容器场景）、USB 直通、
//!   快照存档（`HcsSaveComputeSystem` 存在，但恢复需要配套运行时状态文件，
//!   v1 不做）。
//! - **磁盘只吃 VHDX**，见 `disk` 模块；扩容走 `diskpart expand vdisk`。
//! - **网络只有 NAT**：桥接 / 仅主机都要宿主预配置 HCN 网络，见 `network` 模块。
//!
//! 权限是硬门槛：HCS 只允许管理员或 Hyper-V Administrators 组成员。探测阶段
//! 就会拦下并给出可操作提示，不做「先失败再猜」。

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use super::super::{Engine, ViewRect};
use super::{config, disk, ffi, network};
use crate::model::{EngineCaps, EngineInfo, NetworkMode, VmSpec, VmState};

/// 本插件创建的虚拟机 id 前缀。
///
/// 用作「是不是我们自己的机器」的判据：HCS 是整机共享的服务，用户可能还有
/// 别的程序（WSL2、Docker、Windows Sandbox）在同一台机上跑，绝不能误杀。
const ID_PREFIX: &str = "otools-vm-";

/// 一台由本进程托管的 HCS 虚拟机。
struct ManagedVm {
    system: ffi::System<'static>,
    /// 本次创建的 VHDX 路径。
    ///
    /// 停止后不删：用户可能还要继续用，删盘等于把系统装了两遍。
    #[allow(dead_code)]
    prepared_disk: Option<std::path::PathBuf>,
    /// NAT 网络。**必须活到机器关闭之后**：先删端点再关机器，HCS 会报设备错误。
    /// 字段顺序决定 Drop 顺序，`_network` 声明在 `system` 之后，因此先于它释放
    /// —— 这里靠显式 `Option::take` 在 `stop` 里控制，不依赖声明顺序。
    net: Option<network::NatNetwork>,
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

/// HCS 内部的虚拟机 id（带前缀，避免误伤别人的机器）。
fn machine_id(spec_id: &str) -> String {
    format!("{}{}", ID_PREFIX, crate::paths::sanitize_id(spec_id))
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
/// `State` 是枚举，官方只有六个值：`Created` / `Running` / `Paused` /
/// `Stopped` / `SavedAsTemplate` / `Unknown`。**没有** `Starting` / `Stopping`
/// / `RunningFrozen`——早前按那套词表写的分支永远不会命中，等于状态机里
/// 藏了三段死代码（同时把 `SavedAsTemplate` 误当成关机）。
///
/// 只匹配 ASCII 关键字，不受系统语言影响。
pub fn map_state(raw: &str) -> VmState {
    let lowered = raw.to_ascii_lowercase();
    if lowered.contains("\"running\"") {
        VmState::Running
    } else if lowered.contains("\"paused\"") {
        VmState::Paused
    } else {
        // `Created` / `Stopped` / `SavedAsTemplate` / `Unknown` / 空串：
        // 用户视角都是「没在跑」。
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
            // HCS 的客户机架构跟随宿主。
            arch: if cfg!(target_arch = "aarch64") {
                "arm64".to_string()
            } else {
                "x86_64".to_string()
            },
            accelerator: if available {
                Some("hyperv".to_string())
            } else {
                None
            },
            caps: EngineCaps {
                // HCS 没有公开的取图入口，画面拿不到——本后端最大的能力缺口。
                native_view: false,
                display_output: false,
                // 连画面都没有，更谈不上 GL 加速。
                gl_accel: false,
                shared_dirs: false,
                shared_dirs_guests: Vec::new(),
                clipboard: false,
                auto_resize: false,
                audio: false,
                snapshots: false,
                // 克隆要复制 VHDX（没有写时复制），代价与整盘复制相同；
                // 与其给出一个看起来能用、实际卡几十秒的按钮，不如先如实标 false。
                clone: false,
                // 扩容走 `diskpart expand vdisk`，会同步更新 VHDX 元数据。
                // 机器必须处于关机态（`cmd_resize_disk` 已挡），扩完客户机里
                // 还要自己扩分区/文件系统。
                resize_disk: true,
                rosetta: false,
                // 只有 NAT 可用；桥接 / 仅主机都要宿主预配置 HCN 网络。
                bridged_network: false,
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

        // 网络：只支持 NAT。不支持的组合要**明确拒绝**，不能静默给一台没网的机器
        // ——那会表现为「装完系统上不了网」，用户根本想不到是插件的锅。
        if !config::network_supported(spec.network) {
            return Err(format!(
                "HCS 后端只支持 NAT 网络（当前是「{}」）。桥接与仅主机需要宿主预先配置 \
                 HCS 网络，请切换到 NAT，或在设置里改用 QEMU 引擎。",
                network_label(spec.network)
            ));
        }
        let net = match network::NatNetwork::create(&spec.id) {
            Ok(net) => Some(net),
            Err(reason) => {
                // 网络建不出来就明确失败，不要退回「无网卡」——用户会以为坏了。
                return Err(format!("创建 NAT 网络失败：{}", reason));
            }
        };
        let endpoint = net.as_ref().map(|value| value.endpoint_id().to_string());

        // 安装介质：仅在文件确实存在时挂上，避免因一处路径笔误让整机起不来。
        let installer = spec
            .installer_path
            .as_ref()
            .filter(|path| std::path::Path::new(path).exists());
        let document = config::machine_document_text(
            spec,
            &disk_text,
            installer.map(|value| value.as_str()),
            endpoint.as_deref(),
        );

        let id = machine_id(&spec.id);
        let system = match ffi::System::create(api, &id, &document) {
            Ok(system) => system,
            Err(reason) => {
                // 机器没起来，网络也没用了；`net` 会随作用域结束自动释放。
                return Err(reason);
            }
        };
        if let Err(reason) = system.start() {
            return Err(reason);
        }

        with_machines(|machines| {
            machines.insert(
                spec.id.clone(),
                ManagedVm {
                    system,
                    prepared_disk: Some(disk_path),
                    net,
                    last_error: String::new(),
                    paused: false,
                },
            );
        });
        Ok(())
    }

    fn stop(&self, spec: &VmSpec) -> Result<(), String> {
        let managed = with_machines(|machines| machines.remove(&spec.id));
        let Some(mut managed) = managed else {
            // 不在表里不代表没在跑：可能是上次进程遗留的。尽力关掉它。
            return self.terminate_orphan(&spec.id);
        };
        // 先试优雅关机，失败再强杀——强杀会丢客户机未落盘的数据。
        if let Err(graceful) = managed.system.shutdown() {
            managed
                .system
                .terminate()
                .map_err(|force| format!("{}；强制关闭也失败：{}", graceful, force))?;
        }
        // 网络必须在机器停下来之后再拆（先拆端点会让 HCS 报设备错误）。
        // 显式 take 而不是等 Drop：Drop 顺序依赖字段声明，太脆。
        managed.net.take();
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
        Err("HCS 后端不提供客户机画面：它没有公开的取图接口".to_string())
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
        for mut managed in drained {
            let _ = managed.system.shutdown();
            let _ = managed.system.terminate();
            managed.net.take();
        }
    }
}

impl HcsEngine {
    /// 关掉一台「不在托管表里、但 HCS 里还在」的机器。
    ///
    /// 这条路径专治孤儿：插件崩溃或宿主重启后，上一轮的机器可能还在跑，
    /// 而内存里的表已经空了。以前这种情况只能靠用户去任务管理器里杀
    /// vmwp.exe，或者干脆重启系统。
    fn terminate_orphan(&self, spec_id: &str) -> Result<(), String> {
        let api = match ffi::api() {
            Ok(api) => api,
            Err(_) => return Ok(()),
        };
        let id = machine_id(spec_id);
        // 先确认它真的存在，避免对不存在的 id 调用 open 得到一串无意义错误。
        if !ffi::enumerate(api).iter().any(|item| item == &id) {
            return Ok(());
        }
        let system = ffi::System::open(api, &id)?;
        if let Err(graceful) = system.shutdown() {
            system
                .terminate()
                .map_err(|force| format!("{}；强制关闭也失败：{}", graceful, force))?;
        }
        Ok(())
    }

    /// 列出所有「前缀是我们的、但不在托管表里」的机器。
    ///
    /// **返回的是去掉前缀的 spec id**（不是 HCS 里那串 `otools-vm-<id>`），
    /// 这样前端可以直接和虚拟机列表对照。真要动手还要走 `kill_orphans`，
    /// 因为 `stop()` 要求 spec 文件仍然存在。
    pub fn list_orphans(&self) -> Vec<String> {
        let api = match ffi::api() {
            Ok(api) => api,
            Err(_) => return Vec::new(),
        };
        let managed: Vec<String> =
            with_machines(|machines| machines.keys().cloned().collect());
        ffi::enumerate(api)
            .into_iter()
            .filter(|id| id.starts_with(ID_PREFIX))
            .map(|id| id[ID_PREFIX.len()..].to_string())
            .filter(|id| !managed.contains(id))
            .collect()
    }

    /// 收掉指定的孤儿机器，返回 (成功, 失败)。
    ///
    /// 参数是 `list_orphans()` 给出的 spec id。**逐个处理**：一台失败不该
    /// 连累其余——残留的机器越少越好，能收几台是几台。
    pub fn kill_orphans(&self, spec_ids: &[String]) -> (Vec<String>, Vec<String>) {
        let mut killed = Vec::new();
        let mut failed = Vec::new();
        for spec_id in spec_ids {
            match self.terminate_orphan(spec_id) {
                Ok(()) => killed.push(spec_id.clone()),
                Err(_) => failed.push(spec_id.clone()),
            }
        }
        (killed, failed)
    }

    /// 运行中调整内存上限。
    ///
    /// HCS 的 `ModifySettingRequest` 支持热改 `Memory/SizeInMB`。这里只负责
    /// 「调大」——调小会让客户机里已占用的内存无处安放，HCS 要么拒绝、要么
    /// 让客户机直接崩。上层（`cmd_update`）已把非内存变更挡在外面。
    ///
    /// **CPU 数量不能在运行中改**：`ProcessorCount` 属于 `ComputeTopology` 的
    /// 创建期字段，`ModifySettingRequest` 没有对应路径。改 CPU 仍需关机后走
    /// `vm_update`。
    pub fn set_memory_live(&self, spec_id: &str, memory_mb: u64) -> Result<(), String> {
        with_machines(|machines| {
            let Some(managed) = machines.get(spec_id) else {
                return Err("该虚拟机未在运行，请直接修改配置".to_string());
            };
            managed
                .system
                .modify("VirtualMachine/ComputeTopology/Memory/SizeInMB", json!(memory_mb))
        })
    }
}

fn network_label(mode: NetworkMode) -> &'static str {
    match mode {
        NetworkMode::Nat => "NAT",
        NetworkMode::Bridged => "桥接",
        NetworkMode::HostOnly => "仅主机",
    }
}
