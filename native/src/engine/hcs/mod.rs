//! HCS 后端：Windows 上的原生虚拟化（Host Compute Service）。
//!
//! 模块划分：
//! - `ffi`：运行期动态绑定 `computecore.dll`，不依赖 Windows SDK；
//! - `config`：把 `VmSpec` 翻成 HCS 的 JSON 机器文档（单独可测）；
//! - `network`：HCN NAT 网络 + 端点，HCS 虚拟机靠它才有网卡；
//! - `disk`：把 QEMU 时代的 RAW 盘处理成 HCS 要求的 VHDX；
//! - `engine`：`Engine` trait 的实现。

pub mod config;
pub mod disk;
pub mod engine;
pub mod ffi;
pub mod network;

pub use engine::HcsEngine;


/// 探测结果。`available` 为 false 时 `reason` 一定有内容。
#[derive(Debug, Clone)]
pub struct Availability {
    pub available: bool,
    /// 是「缺权限」而不是「缺组件」——两者的解决路径不同，前端要分开提示。
    pub needs_elevation: bool,
    pub reason: String,
    pub version: String,
}

/// 完整探测，不做缓存：用户中途开功能或提权后重新探测应当能变。
pub fn probe() -> Availability {
    if let Err(reason) = ffi::api() {
        return Availability {
            available: false,
            needs_elevation: false,
            reason: reason.clone(),
            version: String::new(),
        };
    }

    if !is_elevated() && !in_hyperv_admins() {
        return Availability {
            available: false,
            needs_elevation: true,
            reason: "HCS 要求管理员权限：请以管理员身份运行 OTools，或将当前账户加入 \
                     Hyper-V Administrators 组（net localgroup \"Hyper-V Administrators\" /add）"
                .to_string(),
            version: String::new(),
        };
    }

    Availability {
        available: true,
        needs_elevation: false,
        reason: String::new(),
        version: system_version(),
    }
}

/// 当前进程是否已提权。
///
/// `net session` 能成功即说明有管理员权限：它需要访问 SMB 服务端管理接口，
/// 比解析 `whoami /groups` 稳（后者输出受本地化影响）。
pub fn is_elevated() -> bool {
    crate::process::command("net")
        .args(["session"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 是否属于 Hyper-V Administrators 组。
///
/// 与提权是两个独立门槛：组内用户即使不提权也能操作 HCS。两者都不成立才拦。
///
/// **必须按 SID 查，不能按名字匹配**：该组名 `Hyper-V Administrators` 本身就
/// 含 `admin` 子串，用账户名做 `contains` 会恒为真（本机实测踩过：组是空的，
/// 探测却报「已在组内」，于是误判 HCS 可用）。SID `S-1-5-32-578` 是固定的，
/// 也不受系统语言影响。
pub fn in_hyperv_admins() -> bool {
    const HYPERV_ADMINS_SID: &str = "S-1-5-32-578";
    let Ok(output) = crate::process::command("whoami").args(["/groups"]).output() else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .to_uppercase()
        .contains(HYPERV_ADMINS_SID)
}

/// HCS 没有公开的版本号接口，用系统版本代替——它能反映 HCS schema 的能力。
///
/// 不用 `cmd /c ver`：它的输出是系统 ANSI 代码页（中文版是 GBK），按 UTF-8 解
/// 会得到「[�汾 10.0.26100]」这种乱码（本机实测）。改问 PowerShell 取数字，
/// 全是 ASCII，不受代码页影响。
fn system_version() -> String {
    let output = crate::process::command("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[System.Environment]::OSVersion.Version.ToString()",
        ])
        .output();
    let Ok(output) = output else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // 只保留形如 10.0.26100.8037 的值；其它情况宁可不显示，也不显示乱码。
    if text.chars().next().is_some_and(|first| first.is_ascii_digit()) {
        text
    } else {
        String::new()
    }
}

/// 供 `build_engine` 在非 Windows 平台返回一段明确的不可用说明。
pub fn unsupported_reason() -> &'static str {
    "HCS 只在 Windows 上可用"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EngineInfo, VmState};

    fn sample_spec() -> crate::model::VmSpec {
        crate::model::VmSpec {
            id: "demo".to_string(),
            name: "Demo".to_string(),
            os: Default::default(),
            arch: "x86_64".to_string(),
            cpus: 2,
            memory_mb: 2048,
            disk_path: "C:/vm/disk.raw".to_string(),
            disk_size_gb: 20,
            installer_path: None,
            firmware: "uefi".to_string(),
            firmware_path: None,
            driver_iso: None,
            virtiofsd_path: None,
            network: Default::default(),
            network_iface: None,
            boot_order: Vec::new(),
            usb_media: None,
            port_forwards: Vec::new(),
            shared_dirs: Vec::new(),
            clipboard: true,
            auto_resize: true,
            audio: true,
            rosetta: false,
            display_width: 1280,
            display_height: 800,
            notes: String::new(),
            created_at: 0,
        }
    }

    #[test]
    fn document_carries_resources_and_primary_disk() {
        let spec = sample_spec();
        let text = config::machine_document_text(&spec, "C:/vm/disk.vhdx", None, None);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let machine = &value["VirtualMachine"];

        assert_eq!(machine["ComputeTopology"]["Processor"]["Count"], 2);
        assert_eq!(machine["ComputeTopology"]["Memory"]["SizeInMB"], 2048);
        assert_eq!(
            machine["Devices"]["Scsi"]["Primary disk"]["Attachments"]["0"]["Path"],
            "C:/vm/disk.vhdx"
        );
        // 句柄关掉就该停机，否则会留下孤儿虚拟机。
        assert_eq!(value["ShouldTerminateOnLastHandleClosed"], true);
    }

    #[test]
    fn memory_uses_schema_fields_not_guessed_ones() {
        // 回归：`ComputeTopology.Memory` 里**没有** `Backing` 字段。
        // 写错不会被 HCS 报错，只会被静默忽略——于是「设置 4G 实际拿到默认值」。
        let value: serde_json::Value =
            serde_json::from_str(&config::machine_document_text(&sample_spec(), "C:/d.vhdx", None, None))
                .unwrap();
        let memory = &value["VirtualMachine"]["ComputeTopology"]["Memory"];
        assert!(memory.get("Backing").is_none(), "Backing 不是有效字段: {}", memory);
        assert_eq!(memory["SizeInMB"], 2048);
        assert_eq!(memory["AllowOvercommit"], true);
    }

    #[test]
    fn installer_becomes_iso_not_virtual_disk() {
        // 回归：ISO 必须标 `Iso`。写成 `VirtualDisk` 会被当磁盘打开，
        // 客户机里根本看不到光驱内容——装机时表现为「找不到安装介质」。
        let spec = sample_spec();
        let value: serde_json::Value = serde_json::from_str(&config::machine_document_text(
            &spec,
            "C:/vm/disk.vhdx",
            Some("C:/iso/win11.iso"),
            None,
        ))
        .unwrap();
        let attachment = &value["VirtualMachine"]["Devices"]["Scsi"]["Installer cdrom"]["Attachments"]["0"];
        assert_eq!(attachment["Path"], "C:/iso/win11.iso");
        assert_eq!(attachment["Type"], "Iso");
        assert_eq!(attachment["ReadOnly"], true);
    }

    #[test]
    fn network_adapter_goes_under_network_adapters() {
        // 回归：字段名是 `Devices.NetworkAdapters`，不是 `Devices.Network`。
        // 写错会被静默忽略——机器起得来，但客户机里没有网卡，最难查。
        let value: serde_json::Value = serde_json::from_str(&config::machine_document_text(
            &sample_spec(),
            "C:/d.vhdx",
            None,
            Some("11111111-2222-3333-4444-555555555555"),
        ))
        .unwrap();
        let devices = &value["VirtualMachine"]["Devices"];
        assert!(devices.get("Network").is_none(), "Network 不是有效字段");
        assert_eq!(
            devices["NetworkAdapters"]["Network adapter"]["EndpointId"],
            "11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn no_endpoint_means_no_adapter() {
        // 没端点时不该写一个空 NetworkAdapters：HCS 会据此认为要挂网卡却找不到端点。
        let value: serde_json::Value =
            serde_json::from_str(&config::machine_document_text(&sample_spec(), "C:/d.vhdx", None, None))
                .unwrap();
        assert!(value["VirtualMachine"]["Devices"].get("NetworkAdapters").is_none());
    }

    #[test]
    fn boot_target_points_at_primary_disk_by_default() {
        let value: serde_json::Value =
            serde_json::from_str(&config::machine_document_text(&sample_spec(), "C:/vm/d.vhdx", None, None))
                .unwrap();
        let boot = &value["VirtualMachine"]["Chipset"]["Uefi"]["BootThis"];
        assert_eq!(boot["DevicePath"], "Primary disk");
        assert_eq!(boot["DeviceType"], "ScsiDrive");
    }

    #[test]
    fn cdrom_boots_first_only_when_asked_and_media_present() {
        // 首次装系统：光驱优先且确实有 ISO → 从光驱引导。
        let mut spec = sample_spec();
        spec.boot_order = vec!["cdrom".to_string(), "disk".to_string()];
        spec.installer_path = Some("C:/iso/win11.iso".to_string());
        let value: serde_json::Value =
            serde_json::from_str(&config::machine_document_text(&spec, "C:/d.vhdx", None, None)).unwrap();
        assert_eq!(
            value["VirtualMachine"]["Chipset"]["Uefi"]["BootThis"]["DevicePath"],
            "Installer cdrom"
        );

        // 光驱优先但**没有** ISO：不能引导到不存在的设备，否则客户机起不来。
        let mut spec = sample_spec();
        spec.boot_order = vec!["cdrom".to_string(), "disk".to_string()];
        spec.installer_path = None;
        let value: serde_json::Value =
            serde_json::from_str(&config::machine_document_text(&spec, "C:/d.vhdx", None, None)).unwrap();
        assert_eq!(
            value["VirtualMachine"]["Chipset"]["Uefi"]["BootThis"]["DevicePath"],
            "Primary disk"
        );
    }

    #[test]
    fn properties_query_does_not_request_state() {
        // 回归：`State` 不是 `System_PropertyType` 的变体（只有 Memory /
        // Statistics / ProcessList / TerminateOnLastHandleClosed /
        // SharedMemoryRegion / GuestConnection）。请求它会让整份查询被拒，
        // 于是永远查不到状态、界面一直显示「运行中」。
        let query: serde_json::Value =
            serde_json::from_str(config::properties_query()).unwrap();
        let types = query["PropertyTypes"].as_array().unwrap();
        assert!(
            !types.iter().any(|item| item.as_str() == Some("State")),
            "State 不在合法变体里: {}",
            query
        );
        assert!(types.iter().any(|item| item.as_str() == Some("Memory")));
    }

    #[test]
    fn raw_disk_is_rejected_while_vhdx_is_accepted() {
        assert!(!config::disk_is_compatible("C:/vm/disk.raw"));
        assert!(!config::disk_is_compatible("C:/vm/disk.qcow2"));
        assert!(config::disk_is_compatible("C:/vm/disk.vhdx"));
        assert!(config::disk_is_compatible("C:/VM/DISK.VHDX"));
    }

    #[test]
    fn engine_reports_no_native_view() {
        let engine = HcsEngine {
            availability: Availability {
                available: true,
                needs_elevation: false,
                reason: String::new(),
                version: "test".to_string(),
            },
        };
        let info = engine_info_for(&engine);
        // HCS 没有 framebuffer 接口，这条必须一直为 false，否则前端会去挂视图。
        assert!(!info.caps.native_view);
        assert_eq!(info.kind, "hcs");
        assert!(info.available);
    }

    fn engine_info_for(engine: &HcsEngine) -> EngineInfo {
        use crate::engine::Engine;
        engine.info()
    }

    #[test]
    fn state_mapping_handles_hcs_vocabulary() {
        use crate::engine::hcs::engine::map_state;
        assert_eq!(map_state(r#"{"State":"Running"}"#), VmState::Running);
        assert_eq!(map_state(r#"{"State":"Paused"}"#), VmState::Paused);
        assert_eq!(map_state(r#"{"State":"Stopped"}"#), VmState::Stopped);
        assert_eq!(map_state(r#"{"State":"Created"}"#), VmState::Stopped);
        assert_eq!(map_state(r#"{"State":"Off"}"#), VmState::Stopped);
        assert_eq!(map_state(""), VmState::Stopped);
    }

    #[test]
    fn state_mapping_ignores_enum_values_that_do_not_exist() {
        // 回归：HCS 的 State 枚举里没有 Starting / Stopping / RunningFrozen。
        // 早前按那套词表写分支，永远命中不了，等于三段死代码。
        use crate::engine::hcs::engine::map_state;
        for fake in ["Starting", "Stopping", "RunningFrozen"] {
            let document = format!(r#"{{"State":"{}"}}"#, fake);
            // 不能因为这些不存在的词被判成「正在启动/停止」。
            assert!(
                !matches!(map_state(&document), VmState::Starting | VmState::Stopping),
                "{} 造成了状态误判",
                fake
            );
        }
    }

    #[test]
    fn supports_only_matching_architecture() {
        use crate::engine::Engine;
        let engine = HcsEngine {
            availability: Availability {
                available: true,
                needs_elevation: false,
                reason: String::new(),
                version: String::new(),
            },
        };
        let host_arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x86_64"
        };
        let mut spec = sample_spec();
        spec.arch = host_arch.to_string();
        assert!(engine.supports(&spec));
        spec.arch = if host_arch == "arm64" { "x86_64" } else { "arm64" }.to_string();
        assert!(!engine.supports(&spec));
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn hyperv_admins_check_ignores_group_name_substring() {
        // 回归：早前用「用户名是否出现在组成员列表里」判断组身份，
        // 而组名 Hyper-V Administrators 本身含 admin，导致恒为真。
        // 现在按 SID 查，任何组名字面都不参与匹配。
        //
        // 只检查 in_hyperv_admins 的函数体：is_elevated 用 net session 是对的，
        // 扫整个文件会误伤它。
        let source = include_str!("mod.rs");
        let start = source
            .find("pub fn in_hyperv_admins()")
            .expect("找不到 in_hyperv_admins");
        let body = &source[start..];
        let end = body.find("\n}").expect("找不到函数结尾");
        let body = &body[..end];

        assert!(
            body.contains("S-1-5-32-578"),
            "成员判断必须走固定 SID，否则会受组名/本地化影响"
        );
        assert!(
            !body.contains("crate::process::command(\"net\")"),
            "不应再用 net 命令判断组成员身份"
        );
    }

    #[test]
    fn probe_result_is_self_consistent() {
        // 不变量：available 为真时不该再报「需要提权」或给理由；
        // 为假时必须有理由，否则前端只能显示空白。
        let probe = probe();
        if probe.available {
            assert!(!probe.needs_elevation);
            assert!(probe.reason.is_empty(), "可用时不该有理由: {}", probe.reason);
        } else {
            assert!(!probe.reason.is_empty(), "不可用时必须说明原因");
        }
    }
}
