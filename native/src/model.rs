//! 虚拟机领域模型：规格（持久化）与运行时状态（不持久化）分离。

use serde::{Deserialize, Serialize};

/// 客户机类型。决定默认设备组合与提示文案，不改变底层引擎。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuestOs {
    Windows,
    Linux,
    Macos,
    Other,
}

impl Default for GuestOs {
    fn default() -> Self {
        GuestOs::Linux
    }
}

/// 网络模式。桥接需要管理员权限且依赖宿主网络拓扑，v1 只做 NAT。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkMode {
    Nat,
    Bridged,
}

impl Default for NetworkMode {
    fn default() -> Self {
        NetworkMode::Nat
    }
}

/// 宿主目录共享。
///
/// `mount_tag` 是客户机侧挂载名：Linux 下
/// `mount -t virtiofs <tag> /mnt/<tag>`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDir {
    pub host_path: String,
    pub mount_tag: String,
    #[serde(default)]
    pub read_only: bool,
}

impl SharedDir {
    /// 由宿主路径推导一个合法的 virtiofs 标签（客户机侧要能当目录名用）。
    pub fn derive_tag(host_path: &str, index: usize) -> String {
        let base = std::path::Path::new(host_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("share");
        let cleaned: String = base
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
            .collect();
        let trimmed = cleaned.trim_matches('-');
        if trimmed.is_empty() {
            format!("share{}", index + 1)
        } else {
            format!("{}-{}", trimmed, index + 1)
        }
    }
}

/// 一台虚拟机的完整规格。存到 `~/.otools/vm/vms/<id>/vm.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmSpec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub os: GuestOs,
    /// `arm64` 或 `x86_64`。Apple 芯片上 x86_64 客户机只能走 QEMU 模拟，速度很慢。
    #[serde(default = "default_arch")]
    pub arch: String,
    pub cpus: u32,
    pub memory_mb: u64,
    /// 系统盘（RAW 稀疏文件）绝对路径。
    pub disk_path: String,
    pub disk_size_gb: u64,
    /// 安装介质（ISO / IPSW）绝对路径，可空。
    #[serde(default)]
    pub installer_path: Option<String>,
    #[serde(default)]
    pub network: NetworkMode,
    #[serde(default)]
    pub shared_dirs: Vec<SharedDir>,
    #[serde(default = "default_true")]
    pub clipboard: bool,
    #[serde(default = "default_true")]
    pub auto_resize: bool,
    #[serde(default = "default_true")]
    pub audio: bool,
    /// 在 Linux 客户机里暴露 Rosetta，用 arm64 宿主直接跑 x86_64 二进制。
    /// 只在 Apple 芯片 + Linux 客户机下有意义，其它组合会被引擎忽略。
    #[serde(default)]
    pub rosetta: bool,
    #[serde(default = "default_width")]
    pub display_width: u32,
    #[serde(default = "default_height")]
    pub display_height: u32,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub created_at: i64,
}

fn default_arch() -> String {
    if cfg!(target_arch = "aarch64") {
        "arm64".to_string()
    } else {
        "x86_64".to_string()
    }
}
fn default_true() -> bool {
    true
}
fn default_width() -> u32 {
    1280
}
fn default_height() -> u32 {
    800
}

impl VmSpec {
    /// 配置校验。返回用户可读的错误，前端直接展示。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("虚拟机名称不能为空".to_string());
        }
        if self.cpus == 0 {
            return Err("CPU 核心数至少为 1".to_string());
        }
        if self.memory_mb < 512 {
            return Err("内存至少 512 MB".to_string());
        }
        if self.disk_size_gb == 0 {
            return Err("磁盘容量至少 1 GB".to_string());
        }
        if self.disk_path.trim().is_empty() {
            return Err("磁盘路径不能为空".to_string());
        }
        if self.display_width < 640 || self.display_height < 480 {
            return Err("初始分辨率不得小于 640×480".to_string());
        }
        if self.rosetta && self.arch != "arm64" {
            return Err("Rosetta 只能在 arm64 客户机上启用".to_string());
        }
        if self.rosetta && self.os == GuestOs::Windows {
            return Err("Rosetta 只适用于 Linux 客户机".to_string());
        }
        for dir in &self.shared_dirs {
            if !std::path::Path::new(&dir.host_path).is_dir() {
                return Err(format!("共享目录不存在或不是目录：{}", dir.host_path));
            }
            if dir.mount_tag.trim().is_empty() {
                return Err("共享目录挂载标签不能为空".to_string());
            }
        }
        Ok(())
    }
}

/// 运行态。由引擎实时查询，不进磁盘。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VmState {
    Stopped,
    Starting,
    Running,
    Pausing,
    Paused,
    Resuming,
    Stopping,
    Error,
    /// 宿主没有可用引擎，或该虚拟机规格当前引擎不支持。
    Unavailable,
}

impl VmState {
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            VmState::Starting | VmState::Pausing | VmState::Resuming | VmState::Stopping
        )
    }
}

/// 引擎能力矩阵。前端据此灰掉不支持的开关，而不是让用户点了才报错。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCaps {
    /// 原生视图内嵌（VZ 提供 VZVirtualMachineView）。
    pub native_view: bool,
    pub shared_dirs: bool,
    /// 共享目录在哪些客户机开箱可用，前端用于展示限制说明。
    pub shared_dirs_guests: Vec<String>,
    pub clipboard: bool,
    pub auto_resize: bool,
    pub audio: bool,
    pub snapshots: bool,
    /// 能否复制一台虚拟机（APFS 上靠写时复制，秒级完成）。
    pub clone: bool,
    /// 能否在线扩磁盘（只增不减：稀疏文件扩容是安全的，缩小会丢数据）。
    pub resize_disk: bool,
    /// Rosetta 目录共享是否受支持（宿主装没装由运行时再查一次）。
    pub rosetta: bool,
    pub bridged_network: bool,
}

impl EngineCaps {
    pub fn unsupported() -> Self {
        Self {
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    /// `vz` | `qemu` | `none`
    pub kind: String,
    pub available: bool,
    pub label: String,
    /// 不可用原因（前端直接展示）。
    pub reason: String,
    pub version: String,
    /// 宿主进程是否具备 `com.apple.security.virtualization` 权限。
    ///
    /// 该权限属于进程、插件无法自带，宿主版本过旧时会缺失。
    /// 前端据此提前提示「请升级 OTools」，而不是等用户点启动才失败。
    pub host_entitled: bool,
    pub caps: EngineCaps,
}

/// 前端列表项：规格 + 运行态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmSummary {
    #[serde(flatten)]
    pub spec: VmSpec,
    pub state: VmState,
    /// 该虚拟机当前实际使用的引擎（可能因架构回落到 qemu）。
    pub engine: String,
    pub last_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmWorkspace {
    pub engine: EngineInfo,
    pub vms: Vec<VmSummary>,
    pub host: HostInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
    /// 建议的最大 vCPU（宿主核数，保留 2 个给宿主）。
    pub recommended_cpus: u32,
    /// 建议的最大内存 MB（宿主内存的一半）。
    pub recommended_memory_mb: u64,
    pub total_memory_mb: u64,
}

/// 创建/更新入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmCreateInput {
    pub name: String,
    #[serde(default)]
    pub os: GuestOs,
    #[serde(default)]
    pub arch: String,
    pub cpus: u32,
    pub memory_mb: u64,
    pub disk_size_gb: u64,
    #[serde(default)]
    pub installer_path: Option<String>,
    #[serde(default)]
    pub shared_dirs: Vec<SharedDir>,
    #[serde(default)]
    pub clipboard: Option<bool>,
    #[serde(default)]
    pub auto_resize: Option<bool>,
    #[serde(default)]
    pub audio: Option<bool>,
    #[serde(default)]
    pub rosetta: Option<bool>,
    #[serde(default)]
    pub display_width: Option<u32>,
    #[serde(default)]
    pub display_height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmUpdateInput {
    pub id: String,
    pub name: String,
    pub cpus: u32,
    pub memory_mb: u64,
    #[serde(default)]
    pub shared_dirs: Vec<SharedDir>,
    #[serde(default)]
    pub clipboard: Option<bool>,
    #[serde(default)]
    pub auto_resize: Option<bool>,
    #[serde(default)]
    pub audio: Option<bool>,
    #[serde(default)]
    pub rosetta: Option<bool>,
    #[serde(default)]
    pub installer_path: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmCloneInput {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmResizeDiskInput {
    pub id: String,
    pub disk_size_gb: u64,
}
