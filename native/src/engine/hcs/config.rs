//! 把 `VmSpec` 翻译成 HCS 的 JSON 虚拟机文档。
//!
//! HCS 用一份 JSON 描述整台机器（schema 2.x），不像 QEMU 那样靠命令行参数拼装。
//! 这份文档一旦写错，报错只有一句 `HCS_E_INVALID_JSON`，非常难查——所以构造
//! 逻辑集中在这里、单独可测，并保持「结构贴近官方 Quick Start 示例」的样子。
//!
//! 与 QEMU 后端的关键差异（都会影响用户预期，必须如实反映到能力表里）：
//! - **磁盘只吃 VHDX**，不吃 RAW/qcow2；调用方需先转换。
//! - **安装介质（ISO）只能作为 SCSI 光驱**挂给客户机。
//! - **没有 framebuffer**：HCS 不提供读画面的 API，客户机画面拿不到。
//! - 网络走 HCN（Host Compute Network），不是 QEMU 的 `-netdev`。

use crate::model::VmSpec;
use serde_json::{json, Value};

/// HCS 文档 schema 版本。2.1 是 Win10 1809 起稳定的那一版。
const SCHEMA_MAJOR: u32 = 2;
const SCHEMA_MINOR: u32 = 1;

/// 系统盘在 SCSI 控制器上的名字，同时用于 UEFI 引导目标。
const PRIMARY_DISK: &str = "Primary disk";
/// 安装介质光驱的名字。
const INSTALLER_CDROM: &str = "Installer cdrom";

/// 生成虚拟机配置文档。
///
/// `disk_path` 必须是 `.vhdx`；`installer_path` 会作为第二块 SCSI 设备挂上。
pub fn machine_document(spec: &VmSpec, disk_path: &str, installer_path: Option<&str>) -> Value {
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
        scsi.insert(
            INSTALLER_CDROM.to_string(),
            json!({
                "Attachments": {
                    "0": { "Type": "VirtualDisk", "Path": iso }
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
                    "BootThis": {
                        "DevicePath": PRIMARY_DISK,
                        "DiskNumber": 0,
                        "DeviceType": "ScsiDrive"
                    }
                }
            },
            "ComputeTopology": {
                "Memory": {
                    "Backing": "Virtual",
                    "SizeInMB": spec.memory_mb,
                },
                "Processor": { "Count": spec.cpus },
            },
            "Devices": { "Scsi": Value::Object(scsi) },
        }
    })
}

/// 序列化成 HCS 要的宽字符串前的紧凑 JSON。
pub fn machine_document_text(spec: &VmSpec, disk_path: &str, installer_path: Option<&str>) -> String {
    machine_document(spec, disk_path, installer_path).to_string()
}

/// 磁盘是否满足 HCS 要求。
///
/// HCS 只认 VHDX（`VirtualDisk` 附件类型）。RAW 是 QEMU 后端的默认格式，
/// 两者不能混用——这个检查放在启动前，比让 HCS 抛一句「配置无效」有用地多。
pub fn disk_is_compatible(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".vhdx")
}

/// 查询属性用的请求文档：只问状态与内存，字段越少越不容易随版本失效。
pub fn properties_query() -> &'static str {
    r#"{"PropertyTypes":["State","Memory"]}"#
}
