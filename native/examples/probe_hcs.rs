//! 端到端探针：把 HCS 的探测结果打到文件。
//!
//! 本机工具链会吞掉子进程 stdout，所以结果写文件再读（此前踩过多次）。
//!
//! 预期（本机 Win11 26100、未提权）：
//! - `computecore.dll` 能加载，符号齐全；
//! - `is_elevated()` == false，`in_hyperv_admins()` == false；
//! - `probe()` 返回 available=false、needs_elevation=true，原因提到「管理员」。

fn main() {
    let mut out = String::new();

    out.push_str("=== ffi ===\n");
    match otools_vm_native::engine::hcs::ffi::api() {
        Ok(_) => out.push_str("api: OK（DLL 已加载，符号齐全）\n"),
        Err(reason) => out.push_str(&format!("api: FAIL {}\n", reason)),
    }

    out.push_str("=== 权限 ===\n");
    out.push_str(&format!("is_elevated: {}\n", otools_vm_native::engine::hcs::is_elevated()));
    out.push_str(&format!(
        "in_hyperv_admins: {}\n",
        otools_vm_native::engine::hcs::in_hyperv_admins()
    ));

    out.push_str("=== probe ===\n");
    let probe = otools_vm_native::engine::hcs::probe();
    out.push_str(&format!("available: {}\n", probe.available));
    out.push_str(&format!("needs_elevation: {}\n", probe.needs_elevation));
    out.push_str(&format!("reason: {}\n", probe.reason));
    out.push_str(&format!("version: {}\n", probe.version));

    out.push_str("=== engine info ===\n");
    {
        use otools_vm_native::engine::Engine;
        let info = otools_vm_native::engine::hcs::HcsEngine::create().info();
        out.push_str(&format!("kind: {}\n", info.kind));
        out.push_str(&format!("available: {}\n", info.available));
        out.push_str(&format!("caps.native_view: {}\n", info.caps.native_view));
        out.push_str(&format!("caps.display_output: {}\n", info.caps.display_output));
    }

    out.push_str("=== 错误翻译抽样 ===\n");
    for code in [0x8037_011B_u32, 0x8037_0102, 0x8007_0005] {
        out.push_str(&format!(
            "0x{:08X} -> {}\n",
            code,
            otools_vm_native::engine::hcs::ffi::describe(code as i32)
        ));
    }

    let path = std::env::var("TEMP")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("hcs-e2e-out.txt");
    if let Err(error) = std::fs::write(&path, &out) {
        eprintln!("写文件失败: {}", error);
    }
    println!("{}", out);
}
