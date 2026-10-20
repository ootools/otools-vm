//! HCS 端到端探针：真机调 API，验证配置文档与网络能否真的建起来。
//!
//! 本机工具链会吞 stdout，所以结果一律落盘。
//!
//! 分三段，逐段降级（前一段失败不影响后一段能跑就尽量跑）：
//! 1. **FFI**：computecore / computenetwork 符号是否齐全（含可选符号）；
//! 2. **配置文档**：把真实 VmSpec 翻成 JSON，检查关键字段；
//! 3. **HCN 网络**：真的建一条 NAT 网络 + 端点，然后拆掉。
//!    这一步需要管理员权限；拿不到权限时如实报告，不当作失败。
//!
//! 不创建虚拟机本体：那需要一块真 VHDX 且耗时几十秒，属于「用户点按钮」时
//! 才该发生的事。这里只验证「配置与网络这两层是否自洽」。

use std::io::Write;

use otools_vm_native::engine::hcs;

fn sample_spec() -> otools_vm_native::model::VmSpec {
    otools_vm_native::model::VmSpec {
        id: "probe-demo".to_string(),
        name: "Probe Demo".to_string(),
        os: Default::default(),
        arch: "x86_64".to_string(),
        cpus: 2,
        memory_mb: 2048,
        disk_path: "C:/vm/disk.raw".to_string(),
        disk_size_gb: 20,
        installer_path: Some("C:/iso/demo.iso".to_string()),
        firmware: "uefi".to_string(),
        firmware_path: None,
        driver_iso: None,
        virtiofsd_path: None,
        network: Default::default(),
        network_iface: None,
        boot_order: vec!["cdrom".to_string(), "disk".to_string()],
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

fn main() {
    let mut out: Vec<String> = Vec::new();
    out.push("=== HCS 端到端探针 ===".to_string());

    // ---------------------------------------------------------- 1. 探测
    let probe = hcs::probe();
    out.push(format!("probe.available     : {}", probe.available));
    out.push(format!("probe.needsElevation: {}", probe.needs_elevation));
    out.push(format!("probe.reason        : {}", probe.reason));
    out.push(format!("probe.version       : {}", probe.version));
    out.push(format!("is_elevated         : {}", hcs::is_elevated()));
    out.push(format!("in_hyperv_admins    : {}", hcs::in_hyperv_admins()));

    match hcs::ffi::api() {
        Ok(_) => out.push("computecore.dll     : 符号齐全（含可选）".to_string()),
        Err(reason) => out.push(format!("computecore.dll     : 不可用 — {}", reason)),
    }
    out.push(format!("HCN available       : {}", hcs::network::available()));

    // ---------------------------------------------------------- 2. 配置文档
    let spec = sample_spec();
    let document = hcs::config::machine_document_text(
        &spec,
        "C:/vm/probe.vhdx",
        Some("C:/iso/demo.iso"),
        Some(&hcs::network::endpoint_guid("probe-demo")),
    );
    out.push(format!("document bytes      : {}", document.len()));
    match serde_json::from_str::<serde_json::Value>(&document) {
        Ok(value) => {
            let devices = &value["VirtualMachine"]["Devices"];
            out.push(format!(
                "  memory.SizeInMB   : {}",
                value["VirtualMachine"]["ComputeTopology"]["Memory"]["SizeInMB"]
            ));
            out.push(format!(
                "  memory.Backing    : {}",
                value["VirtualMachine"]["ComputeTopology"]["Memory"]
                    .get("Backing")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<不存在，正确>".to_string())
            ));
            out.push(format!(
                "  scsi.iso.Type     : {}",
                devices["Scsi"]["Installer cdrom"]["Attachments"]["0"]["Type"]
            ));
            out.push(format!(
                "  Devices.Network   : {}",
                if devices.get("Network").is_some() {
                    "存在（错误！字段名应为 NetworkAdapters）"
                } else {
                    "<不存在，正确>"
                }
            ));
            out.push(format!(
                "  NetworkAdapters   : {}",
                if devices.get("NetworkAdapters").is_some() {
                    "存在，正确"
                } else {
                    "缺失（错误！网卡不会生效）"
                }
            ));
            out.push(format!(
                "  adapter.EndpointId: {}",
                devices["NetworkAdapters"]["Network adapter"]["EndpointId"]
            ));
            out.push(format!(
                "  boot.DevicePath   : {}",
                value["VirtualMachine"]["Chipset"]["Uefi"]["BootThis"]["DevicePath"]
            ));
        }
        Err(error) => out.push(format!("  JSON 解析失败：{}", error)),
    }
    out.push(format!(
        "propertiesQuery     : {}",
        hcs::config::properties_query()
    ));

    // ---------------------------------------------------------- 3. HCN 网络
    out.push("--- HCN NAT 网络实测 ---".to_string());
    out.push(format!(
        "networkGuid         : {}",
        hcs::network::network_guid("probe-demo")
    ));
    out.push(format!(
        "endpointGuid        : {}",
        hcs::network::endpoint_guid("probe-demo")
    ));
    if !probe.available {
        out.push("跳过：HCS 不可用（通常是缺管理员权限）".to_string());
    } else {
        match hcs::network::NatNetwork::create("probe-demo") {
            Ok(net) => {
                out.push(format!("create              : 成功，endpoint={}", net.endpoint_id()));
                drop(net); // 立刻拆掉，不在系统里留测试网络
                out.push("cleanup             : 已释放".to_string());
            }
            Err(reason) => out.push(format!("create              : 失败 — {}", reason)),
        }
    }

    // ---------------------------------------------------------- 4. 孤儿枚举
    out.push("--- 遗留虚拟机 ---".to_string());
    match hcs::ffi::api() {
        Ok(_) => {
            let engine = hcs::HcsEngine::create();
            let orphans = engine.list_orphans();
            out.push(format!("orphans             : {:?}", orphans));
        }
        Err(reason) => out.push(format!("跳过：{}", reason)),
    }

    let path = std::env::temp_dir().join("hcs-e2e-out.txt");
    let mut file = std::fs::File::create(&path).expect("create output");
    for line in &out {
        writeln!(file, "{}", line).ok();
    }
    println!("written to {}", path.display());
}
