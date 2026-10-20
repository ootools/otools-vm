//! QEMU 获取路径的端到端探针。
//!
//! 在真机上跑，把结果写文件（本机工具链吞 stdout）。只做**只读探测**：
//! 报告本机会选哪条路、winget 是否可用、现有 QEMU 在哪。
//! 不会真的下载或安装——那是用户点按钮才该发生的事。
//!
//! 用法：
//! ```text
//! cargo run --example probe_qemu --target-dir target
//! ```

use std::io::Write;

fn main() {
    let mut lines: Vec<String> = Vec::new();
    lines.push("=== QEMU 获取路径探针 ===".to_string());

    let plan = otools_vm_native::engine::qemu::install::plan();
    lines.push(format!("automatic       : {}", plan.automatic));
    lines.push(format!("strategy        : {}", plan.strategy));
    lines.push(format!("reason          : {}", plan.reason));
    lines.push(format!("note            : {}", plan.note));
    lines.push(format!("command         : {}", plan.command));
    lines.push(format!(
        "supportedPlatform: {}",
        otools_vm_native::engine::qemu::install::is_supported_platform()
    ));
    lines.push(format!(
        "binaryName      : {}",
        otools_vm_native::engine::qemu::install::binary_name()
    ));
    lines.push(format!(
        "binaryPath      : {}",
        otools_vm_native::engine::qemu::install::binary_path().display()
    ));
    lines.push(format!(
        "installRoot     : {}",
        otools_vm_native::engine::qemu::install::install_root().display()
    ));

    let status = otools_vm_native::engine::qemu::install::status();
    lines.push(format!(
        "status          : phase={} busy={}",
        if status.phase.is_empty() { "idle" } else { status.phase.as_str() },
        otools_vm_native::engine::qemu::install::is_running()
    ));

    // winget 是否可达：这是 Windows 上走哪条路的分水岭。
    let winget = std::process::Command::new("winget")
        .arg("--version")
        .output()
        .map(|out| {
            format!(
                "ok={} {}",
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim()
            )
        })
        .unwrap_or_else(|error| format!("unavailable ({})", error));
    lines.push(format!("winget          : {}", winget));

    // 现有 QEMU：引擎探测会看到什么。
    let info = {
        use otools_vm_native::engine::Engine;
        let engine = otools_vm_native::engine::qemu::QemuEngine::create_or_noop(String::new());
        engine.info()
    };
    lines.push(format!(
        "engine kind={} available={} version={} displayOutput={}",
        info.kind, info.available, info.version, info.caps.display_output
    ));
    lines.push(format!("engine reason   : {}", info.reason));
    lines.push(format!("engine accel    : {:?}", info.accelerator));

    let output = std::env::temp_dir().join("qemu-probe-out.txt");
    let mut file = std::fs::File::create(&output).expect("create output");
    for line in &lines {
        writeln!(file, "{}", line).ok();
    }
    println!("written to {}", output.display());
}
