//! 把 `VmSpec` 翻译成 HCS 的 JSON 虚拟机文档。
//!
//! HCS 用一份 JSON 描述整台机器（schema 2.x），不像 QEMU 那样靠命令行参数拼装。
//! 这份文档一旦写错，报错只有一句 `HCS_E_INVALID_JSON`，非常难查——所以构造
//! 逻辑集中在这里、单独可测，并**逐字段对照官方 JSON Schema Reference**。
//!
//! 已核对过的几个容易写错的点（都有测试盯着）：
//! - `Attachment.Type` 必须是 `VirtualDisk` / `Iso` / `PassThru` 之一。ISO 用
//!   `VirtualDisk` 会被当成磁盘打开，客户机里根本看不到光驱内容。
//! - `ComputeTopology.Memory` 里**没有** `Backing` 字段（那是早期猜测）；
//!   有效的是 `SizeInMB` 与 `AllowOvercommit`。
//! - `Devices.NetworkAdapters` 是 `Map<string, NetworkAdapter>`，端点字段名是
//!   `EndpointId`（Guid）。写成 `Devices.Network` 会被静默忽略——机器起得来，
//!   但客户机里没有网卡，最难查。
//! - `Uefi.BootThis` 是**单个** `UefiBootEntry`，没有「启动顺序数组」这种写法。
//!   想改启动顺序只能改 `DiskNumber` 指向哪块盘。

use crate::model::{NetworkMode, VmSpec};
use serde_json::{json, Value};

/// HCS 文档 schema 版本。2.1 是 Win10 1809 起稳定的那一版。
const SCHEMA_MAJOR: u32 = 2;
const SCHEMA_MINOR: u32 = 1;

/// 系统盘在 SCSI 控制器上的名字，同时用于 UEFI 引导目标。
const PRIMARY_DISK: &str = "Primary disk";
/// 安装介质光驱的名字。
const INSTALLER_CDROM: &str = "Installer cdrom";
/// 网卡在 `Devices.NetworkAdapters` 里的键名。
const NETWORK_ADAPTER: &str = "Network adapter";

/// 生成虚拟机配置文档。
///
/// `disk_path` 必须是 `.vhdx`；`installer_path` 会作为第二块 SCSI 设备挂上。
/// `endpoint_id` 非空时挂一块接到该 HCN 端点的网卡。
pub fn machine_document(
    spec: &VmSpec,
    disk_path: &str,
    installer_path: Option<&str>,
    endpoint_id: Option<&str>,
) -> Value {
    let mut scsi = serde_json::Map::new();
    scsi.insert(
        PRIMARY_DISK.to_string(),
        json!({
            "Attachments": {
                "0": { "Type": "VirtualDisk", "Path": disk_path }
            }
        }),
    );
    if let Some(iso) = installer_path {
        // ISO 必须标成 `Iso`：用 `VirtualDisk` 会被当磁盘打开，客户机里没有光驱。
        // `ReadOnly` 显式写上——客户机不该能改安装介质。
        scsi.insert(
            INSTALLER_CDROM.to_string(),
            json!({
                "Attachments": {
                    "0": { "Type": "Iso", "Path": iso, "ReadOnly": true }
                }
            }),
        );
    }

    let mut devices = serde_json::Map::new();
    devices.insert("Scsi".to_string(), Value::Object(scsi));
    if let Some(endpoint) = endpoint_id {
        devices.insert(
            "NetworkAdapters".to_string(),
            json!({
                NETWORK_ADAPTER: {
                    // 端点由 HCN 创建；HCS 只按这个 Guid 挂上。
                    "EndpointId": endpoint
                }
            }),
        );
    }

    json!({
        "SchemaVersion": { "Major": SCHEMA_MAJOR, "Minor": SCHEMA_MINOR },
        "Owner": "OTools",
        // 进程崩掉时句柄会随之关闭，机器应当一并终止——否则会留下孤儿虚拟机
        // 占着磁盘和内存，用户从界面上还看不到。
        "ShouldTerminateOnLastHandleClosed": true,
        "VirtualMachine": {
            "Chipset": {
                "Uefi": {
                    "BootThis": boot_entry(spec)
                }
            },
            "ComputeTopology": {
                "Memory": {
                    // 没有 `Backing` 字段；`AllowOvercommit` 让内存按页文件后备，
                    // 与 Task Manager 里看到的「已提交」一致，也是 Hyper-V 的默认行为。
                    "SizeInMB": spec.memory_mb,
                    "AllowOvercommit": true,
                },
                "Processor": {
                    "Count": spec.cpus,
                    // 嵌套虚拟化：Linux 客户机里跑 Docker/WSL 需要它。
                    // 只在规格里明确要了才开——开了会略降宿主侧安全性。
                    "ExposeVirtualizationExtensions": false,
                },
            },
            "Devices": Value::Object(devices),
        }
    })
}

/// 该从哪里引导。
///
/// HCS 只有「一个」引导目标（`BootThis`），没有 QEMU 那种有序启动列表。
/// 我们的映射：规格里把 `cdrom` 排在 `disk` 前面时，从光驱引导（首次装系统），
/// 否则从系统盘引导。
///
/// 没装系统时空盘引导会落到 UEFI Shell——所以「有安装介质且光驱优先」这条
/// 才是有意义的分支，其余一律引导系统盘，避免装完系统还反复进安装界面。
fn boot_entry(spec: &VmSpec) -> Value {
    let cdrom_first = match (spec.boot_order.iter().position(|item| item == "disk"),
        spec.boot_order.iter().position(|item| item == "cdrom")) {
        (Some(disk), Some(cdrom)) => cdrom < disk,
        (None, Some(_)) => true,
        _ => false,
    };
    // 没有安装介质时不可能从光驱引导，别把客户机卡在「找不到引导设备」。
    if cdrom_first && spec.installer_path.is_some() {
        return json!({
            "DeviceType": "ScsiDrive",
            "DevicePath": INSTALLER_CDROM,
            "DiskNumber": 0,
        });
    }
    json!({
        "DeviceType": "ScsiDrive",
        "DevicePath": PRIMARY_DISK,
        "DiskNumber": 0,
    })
}

/// 序列化成 HCS 要的宽字符串前的紧凑 JSON。
pub fn machine_document_text(
    spec: &VmSpec,
    disk_path: &str,
    installer_path: Option<&str>,
    endpoint_id: Option<&str>,
) -> String {
    machine_document(spec, disk_path, installer_path, endpoint_id).to_string()
}

/// 磁盘是否满足 HCS 要求。
///
/// HCS 只认 VHDX（`VirtualDisk` 附件类型）。RAW 是 QEMU 后端的默认格式，
/// 两者不能混用——这个检查放在启动前，比让 HCS 抛一句「配置无效」有用地多。
pub fn disk_is_compatible(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".vhdx")
}

/// 属性查询文档。
///
/// **`State` 不在 `PropertyTypes` 里**：`Properties` 响应恒带 `Id` / `SystemType` /
/// `State`，`PropertyTypes` 只用于请求那些「要额外算」的属性。早前写成
/// `{"PropertyTypes":["State","Memory"]}` 是非法值——`State` 不是
/// `System_PropertyType` 的变体，整份查询会被拒，于是永远查不到状态。
pub fn properties_query() -> &'static str {
    r#"{"PropertyTypes":["Memory"]}"#
}

/// 该网络模式在 HCS 上是否可实现。
///
/// HCS 的网络全走 HCN（Host Compute Network），没有 QEMU 那种用户态网络。
/// 三种模式里只有 NAT 能让插件自己建（建一个 ICS/NAT 网络 + 一个端点），
/// 桥接需要宿主预先存在对应的 HCN 网络，仅主机同理——v1 只做 NAT。
pub fn network_supported(mode: NetworkMode) -> bool {
    matches!(mode, NetworkMode::Nat)
}
