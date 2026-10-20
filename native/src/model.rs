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

/// 网络模式。
///
/// - `nat`：QEMU 用户态网络（SLIRP）。零配置、免管理员权限；客户机经虚拟网关
///   `10.0.2.2` 访问宿主，宿主访问客户机要靠端口转发。
/// - `bridged`：客户机接入宿主所在局域网，拿到同网段的真实地址，局域网内其它
///   设备可直接访问客户机。需要宿主提供一张二层网卡供其桥接。
/// - `hostOnly`：只在客户机与宿主之间通信，不出局域网。macOS 走 `vmnet-host`；
///   Windows / Linux 依附一张不参与桥接的私有网卡。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkMode {
    Nat,
    Bridged,
    HostOnly,
}

impl NetworkMode {
    /// 该模式是否需要在宿主上指定一张网卡。
    ///
    /// macOS 的 vmnet 由 vmnet.framework 自己造网段，桥接要指定物理网卡、
    /// 仅主机不需要；Windows / Linux 两种模式都依附在具体的网卡上。
    pub fn needs_interface(self) -> bool {
        match self {
            NetworkMode::Nat => false,
            NetworkMode::Bridged => true,
            NetworkMode::HostOnly => !cfg!(target_os = "macos"),
        }
    }

    /// 该模式是否走 QEMU 用户态网络（只有用户态网络吃 hostfwd 端口转发）。
    pub fn is_user_net(self) -> bool {
        matches!(self, NetworkMode::Nat)
    }
}

impl Default for NetworkMode {
    fn default() -> Self {
        NetworkMode::Nat
    }
}

/// 共享协议：Plan 9 virtfs（QEMU `-virtfs`），Linux 客户机开箱可用，
/// 客户机不需要装任何东西。
pub const SHARED_PROTOCOL_9P: &str = "9p";
/// 共享协议：virtio-fs，本地文件系统语义、性能最好；
/// 需要宿主持有 `virtiofsd`（仅 Linux 宿主），
/// Windows 客户机还需 virtio-win 的 viofs 驱动。
pub const SHARED_PROTOCOL_VIRTIOFS: &str = "virtiofs";

/// 共享协议：宿主原生 SMB 服务端（Windows 的文件共享 / Linux 的 Samba /
/// macOS 的文件共享）。客户机按网络路径 `\\<宿主>\<共享名>` 访问，
/// **不需要客户机装任何驱动**——这是 Windows 客户机上唯一开箱即用的共享方式。
pub const SHARED_PROTOCOL_SMB: &str = "smb";

/// 全部共享协议，native 校验与前端下拉共用。
pub const SHARED_PROTOCOLS: [&str; 3] = [
    SHARED_PROTOCOL_9P,
    SHARED_PROTOCOL_VIRTIOFS,
    SHARED_PROTOCOL_SMB,
];

/// 启动设备：系统盘。
pub const BOOT_DISK: &str = "disk";
/// 启动设备：安装 / 驱动光盘。
pub const BOOT_CDROM: &str = "cdrom";
/// 启动设备：U 盘介质（RAW 磁盘镜像）。
pub const BOOT_USB: &str = "usb";
/// 启动设备：网络 PXE 启动。
pub const BOOT_NET: &str = "net";

/// 全部启动设备候选，前端下拉与 native 校验共用。
pub const BOOT_DEVICES: [&str; 4] = [BOOT_DISK, BOOT_CDROM, BOOT_USB, BOOT_NET];

/// 宿主目录共享。
///
/// `mount_tag` 是客户机侧挂载名：9p 下 Linux 客户机
/// `mount -t 9p -o trans=virtio <tag> /mnt/<tag>`；virtio-fs 下
/// `mount -t virtiofs <tag> /mnt/<tag>`（Linux）或
/// `virtiofs.exe -m Z: -t <tag>`（Windows）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDir {
    pub host_path: String,
    pub mount_tag: String,
    /// 共享协议：`9p`（默认）、`virtiofs` 或 `smb`。
    #[serde(default = "default_protocol")]
    pub protocol: String,
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
        // virtio-fs 的挂载标签上限 36 字符（virtio-win 侧限制），给序号留出余量。
        let trimmed: String = cleaned.trim_matches('-').chars().take(32).collect();
        if trimmed.is_empty() {
            format!("share{}", index + 1)
        } else {
            format!("{}-{}", trimmed, index + 1)
        }
    }

    /// 该共享项是否走宿主原生 SMB。
    pub fn is_smb(&self) -> bool {
        self.protocol == SHARED_PROTOCOL_SMB
    }

    /// 宿主原生 SMB 共享名校验。
    ///
    /// Windows 不允许共享名含 `\ / : * ? " < > |`，长度上限 80；这里进一步
    /// 收紧到字母数字与 `- _ .`，免得跨到 Linux（Samba）时才炸。
    pub fn is_valid_smb_share_name(name: &str) -> bool {
        !name.is_empty()
            && name.chars().count() <= 80
            && name.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.'
            })
    }
}

/// NAT 端口转发：把宿主端口映射进客户机（Parallels「共享网络」里的端口转发规则）。
///
/// 只在 `nat` 网络下有意义——桥接 / 仅主机模式的客户机本来就有独立地址，
/// 不需要宿主代转。
///
/// 典型用途是 Windows 客户机的远程桌面：宿主 3389 → 客户机 3389。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
    /// tcp 或 udp。
    pub protocol: String,
    /// 宿主监听端口。
    pub host_port: u16,
    /// 客户机目标端口。
    pub guest_port: u16,
    /// 宿主绑定地址。默认 127.0.0.1：只监听回环，不会把端口暴露到局域网。
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
}

fn default_bind_host() -> String {
    "127.0.0.1".to_string()
}

/// 一台虚拟机的完整规格。存到 `~/.otools/vm/vms/<id>/vm.json`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// 固件类型：`bios`（默认，SeaBIOS）或 `uefi`（OVMF）。
    /// Windows 11 等现代客户机需要 UEFI；arm64 `virt` 机器本就默认 UEFI。
    #[serde(default = "default_firmware")]
    pub firmware: String,
    /// UEFI 固件文件（OVMF_CODE.fd）绝对路径；留空则按平台常见位置自动探测。
    #[serde(default)]
    pub firmware_path: Option<String>,
    /// VirtIO 驱动 ISO（virtio-win 等）绝对路径，作为附加光盘挂给客户机。
    /// Windows 安装时经「加载驱动」识别 virtio 磁盘/网卡。
    #[serde(default)]
    pub driver_iso: Option<String>,
    /// virtiofsd 可执行文件路径；留空则按常见安装位置自动探测。
    /// 仅在共享目录使用 `virtiofs` 协议时有意义。
    #[serde(default)]
    pub virtiofsd_path: Option<String>,
    #[serde(default)]
    pub network: NetworkMode,
    /// 桥接 / 仅主机模式用的宿主网卡标识：Windows 填「网络连接」名，
    /// Linux 填 `eth0` 一类接口名，macOS 填 `en0` 一类。NAT 模式下忽略。
    #[serde(default)]
    pub network_iface: Option<String>,
    /// 启动顺序：disk / cdrom / usb / net 的有序列表，决定 QEMU 的 bootindex。
    /// 留空表示不干预，走 QEMU 默认顺序。
    #[serde(default)]
    pub boot_order: Vec<String>,
    /// U 盘介质：RAW 磁盘镜像（.img）绝对路径，以 USB 大容量存储设备挂给客户机，
    /// 可在启动顺序里作为启动项。
    #[serde(default)]
    pub usb_media: Option<String>,
    /// NAT 端口转发规则。
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
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
fn default_firmware() -> String {
    "bios".to_string()
}
fn default_protocol() -> String {
    SHARED_PROTOCOL_9P.to_string()
}

/// virtio-fs 挂载标签校验：virtio-win 的 `virtiofs.exe` 上限 36 字符，
/// 且标签会出现在客户机盘符/资源管理器里，不能带空格等字符。
fn is_valid_virtiofs_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.chars().count() <= 36
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
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
        if self.firmware != "bios" && self.firmware != "uefi" {
            return Err(format!("未知固件类型：{}（应为 bios 或 uefi）", self.firmware));
        }
        // 桥接 / 仅主机都要挂到一张具体的宿主网卡上，没指定等于配了个空网卡。
        if self.network.needs_interface()
            && self
                .network_iface
                .as_deref()
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        {
            return Err("桥接 / 仅主机网络需要指定宿主网卡".to_string());
        }
        for entry in &self.boot_order {
            if !BOOT_DEVICES.contains(&entry.as_str()) {
                return Err(format!(
                    "未知启动设备：{}（应为 disk / cdrom / usb / net）",
                    entry
                ));
            }
        }
        // 选了 U 盘启动却没给介质，等于配了个空设备——提前报错，别让客户机黑屏。
        if self.boot_order.iter().any(|entry| entry == BOOT_USB) && self.usb_media.is_none() {
            return Err("启动顺序包含 U 盘，但未指定 U 盘介质".to_string());
        }
        if let Some(media) = self.usb_media.as_deref() {
            if !std::path::Path::new(media).is_file() {
                return Err(format!("U 盘介质文件不存在：{}", media));
            }
        }
        // hostfwd 是用户态网络独有的选项；桥接 / 仅主机模式下 QEMU 会直接
        // 拒绝这条参数，所以提前报错，而不是让用户对着启动失败猜。
        if !self.network.is_user_net() && !self.port_forwards.is_empty() {
            return Err(
                "端口转发只在 NAT 网络下可用；桥接 / 仅主机模式的客户机已有独立地址"
                    .to_string(),
            );
        }
        for rule in &self.port_forwards {
            if rule.protocol != "tcp" && rule.protocol != "udp" {
                return Err(format!("端口转发协议只支持 tcp / udp：{}", rule.protocol));
            }
            if rule.host_port == 0 || rule.guest_port == 0 {
                return Err("端口转发端口必须为 1-65535".to_string());
            }
            // hostfwd 用冒号分隔各段，绑定地址只接受 IPv4；IPv6 / 主机名会把参数拼坏。
            let valid_ipv4 = rule.bind_host.split('.').count() == 4
                && rule
                    .bind_host
                    .split('.')
                    .all(|part| !part.is_empty() && part.parse::<u8>().is_ok());
            if !valid_ipv4 {
                return Err(format!(
                    "端口转发绑定地址不是 IPv4 地址：{}",
                    rule.bind_host
                ));
            }
        }
        // 同一协议 + 宿主端口只能有一条，否则 QEMU 监听同一个端口直接起不来。
        let mut seen: Vec<(String, u16)> = Vec::new();
        for rule in &self.port_forwards {
            let key = (rule.protocol.clone(), rule.host_port);
            if seen.contains(&key) {
                return Err(format!(
                    "端口转发规则重复：{}:{} 已存在",
                    rule.protocol, rule.host_port
                ));
            }
            seen.push(key);
        }
        for dir in &self.shared_dirs {
            if !SHARED_PROTOCOLS.contains(&dir.protocol.as_str()) {
                return Err(format!(
                    "未知共享协议：{}（应为 9p / virtiofs / smb）",
                    dir.protocol
                ));
            }
            if !std::path::Path::new(&dir.host_path).is_dir() {
                return Err(format!("共享目录不存在或不是目录：{}", dir.host_path));
            }
            if dir.mount_tag.trim().is_empty() {
                return Err("共享目录挂载标签不能为空".to_string());
            }
            if dir.protocol == SHARED_PROTOCOL_VIRTIOFS && !is_valid_virtiofs_tag(&dir.mount_tag) {
                return Err(format!(
                    "virtio-fs 挂载标签不合法：{}（仅限字母、数字、-、_，最长 36 字符）",
                    dir.mount_tag
                ));
            }
            if dir.is_smb() && !SharedDir::is_valid_smb_share_name(&dir.mount_tag) {
                return Err(format!(
                    "SMB 共享名不合法：{}（仅限字母、数字、-、_、.，最长 80 字符）",
                    dir.mount_tag
                ));
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

/// 存档式快照能力。
///
/// Apple VZ 的 `validateSaveRestoreSupport` 是**按配置**判定的（同一台机器，
/// 换掉显卡/设备组合结果就可能变），所以不能只在引擎级给一个 bool，必须跟着
/// 具体 `VmSpec` 走。前端据此决定「存档/恢复」按钮是否可点、以及为什么不可点。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSupport {
    /// 该配置能否存盘。
    pub supported: bool,
    /// 不能时的原因（直接展示给用户，来自 VZ 的错误文案）。
    pub reason: String,
    /// 是否已有一份可恢复的存档文件。
    pub has_snapshot: bool,
    /// 存档文件大小（字节），无存档时为 0。
    pub snapshot_bytes: u64,
    /// 存档创建时间（Unix 秒），无存档时为 0。
    pub snapshot_at: i64,
    /// 快照任务当前阶段：`idle` / `savingPausing` / `savingWriting` /
    /// `restoring` / `restoringResuming`。
    ///
    /// 存/恢复是异步链（save = pause→落盘；restore = load→resume），命令立即返回，
    /// 前端靠轮询这个字段显示进度。
    #[serde(default)]
    pub phase: String,
    /// 最近一次快照失败原因；成功或未开始时为空。
    #[serde(default)]
    pub error: String,
}

impl SnapshotSupport {
    pub fn unsupported() -> Self {
        Self {
            supported: false,
            reason: "当前引擎不支持存档式快照".to_string(),
            has_snapshot: false,
            snapshot_bytes: 0,
            snapshot_at: 0,
            phase: "idle".to_string(),
            error: String::new(),
        }
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
    /// 该后端能否把客户机画面送出来（内嵌视图或独立窗口都算）。
    ///
    /// `false` 意味着**任何**画面都拿不到，不只是不能内嵌——HCS 就是这种：
    /// 它没有 framebuffer 接口，商业实现都得自带签名内核显示驱动。
    /// 前端据此隐藏「控制台」入口，并在详情里说明，而不是让用户对着黑屏猜。
    pub display_output: bool,
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
            display_output: false,
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
    /// 当前实际生效的加速器（QEMU 运行时经 monitor 探测）：`whpx`/`hvf`/`kvm`/`tcg`。
    /// `None` 表示尚未探测到。前端据此在跑 TCG 软件模拟时提示开启硬件加速。
    #[serde(default)]
    pub accelerator: Option<String>,
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
    pub network: Option<NetworkMode>,
    #[serde(default)]
    pub network_iface: Option<String>,
    #[serde(default)]
    pub installer_path: Option<String>,
    #[serde(default)]
    pub firmware: Option<String>,
    #[serde(default)]
    pub firmware_path: Option<String>,
    #[serde(default)]
    pub driver_iso: Option<String>,
    #[serde(default)]
    pub virtiofsd_path: Option<String>,
    #[serde(default)]
    pub boot_order: Vec<String>,
    #[serde(default)]
    pub usb_media: Option<String>,
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
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
    pub firmware: Option<String>,
    #[serde(default)]
    pub firmware_path: Option<String>,
    #[serde(default)]
    pub driver_iso: Option<String>,
    #[serde(default)]
    pub virtiofsd_path: Option<String>,
    #[serde(default)]
    pub boot_order: Vec<String>,
    #[serde(default)]
    pub usb_media: Option<String>,
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
    #[serde(default)]
    pub network: Option<NetworkMode>,
    #[serde(default)]
    pub network_iface: Option<String>,
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
