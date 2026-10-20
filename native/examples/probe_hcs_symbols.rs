//! 枚举 `computecore.dll` / `computenetwork.dll` 的导出符号。
//!
//! 为什么要这一步：HCS 没有公开完整头文件在我们手上，而不同 Windows 版本的
//! 符号集不同。靠猜「有没有 HcsOpenComputeSystem」会写出编不过运行也没反应的
//! 代码；直接解析 PE 导出表，拿到的就是这台机器上**真实可用**的集合。
//!
//! 结果落盘（本机工具链会吞 stdout）。

use std::ffi::c_void;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 从已加载模块的基址解析导出名列表。
///
/// PE 布局：DOS 头 `e_lfanew` → PE 签名 → COFF 头 → 可选头 →
/// 数据目录第 0 项是导出表。全程只读，越界一律当失败返回空。
unsafe fn exports(base: *mut c_void) -> Vec<String> {
    let mut names = Vec::new();
    if base.is_null() {
        return names;
    }
    let image = base as *const u8;
    let e_lfanew = *(image.add(0x3C) as *const u32) as usize;
    // PE\0\0
    if *(image.add(e_lfanew) as *const u32) != 0x0000_4550 {
        return names;
    }
    // COFF 头之后是可选头；x64 的可选头 0xF0 字节，数据目录从 +0x70 开始。
    let optional = image.add(e_lfanew + 4 + 20);
    let magic = *(optional as *const u16);
    let data_dir = if magic == 0x20b {
        optional.add(0x70)
    } else {
        optional.add(0x60)
    };
    let export_rva = *(data_dir as *const u32) as usize;
    let export_size = *(data_dir.add(4) as *const u32) as usize;
    if export_rva == 0 || export_size == 0 {
        return names;
    }

    let export = image.add(export_rva);
    let name_count = *(export.add(24) as *const u32) as usize;
    let names_rva = *(export.add(32) as *const u32) as usize;
    if name_count == 0 || name_count > 8192 || names_rva == 0 {
        return names;
    }
    let table = image.add(names_rva) as *const u32;
    for index in 0..name_count {
        let rva = *table.add(index) as usize;
        if rva == 0 {
            continue;
        }
        let text = image.add(rva) as *const i8;
        let mut length = 0usize;
        while *text.add(length) != 0 && length < 256 {
            length += 1;
        }
        let bytes = std::slice::from_raw_parts(text as *const u8, length);
        names.push(String::from_utf8_lossy(bytes).to_string());
    }
    names
}

fn dump(module_name: &str, lines: &mut Vec<String>) {
    let module_wide = wide(module_name);
    let mut module = unsafe { GetModuleHandleW(module_wide.as_ptr()) };
    if module.is_null() {
        module = unsafe { LoadLibraryW(module_wide.as_ptr()) };
    }
    if module.is_null() {
        lines.push(format!("=== {} === 未找到", module_name));
        return;
    }
    let mut names = unsafe { exports(module) };
    names.sort();
    lines.push(format!("=== {} === 共 {} 个导出", module_name, names.len()));
    for name in names {
        lines.push(format!("  {}", name));
    }
}

fn main() {
    let mut lines = Vec::new();
    dump("computecore.dll", &mut lines);
    dump("computenetwork.dll", &mut lines);
    dump("computestorage.dll", &mut lines);

    // 关心的能力是否具备：直接列出候选名，省得人肉翻全表。
    let candidates = [
        "HcsOpenComputeSystem",
        "HcsCloseComputeSystem",
        "HcsModifyComputeSystem",
        "HcsGetComputeSystemProperties",
        "HcsSetComputeSystemCallback",
        "HcsSaveComputeSystem",
        "HcsAddNetworkAdapterEndpoint",
        "HcsCreateComputeSystem",
        "HcsEnumerateComputeSystems",
        "HcsPauseComputeSystem",
        "HcsResumeComputeSystem",
        "HcsShutDownComputeSystem",
        "HcsTerminateComputeSystem",
        "HcsCrashComputeSystem",
        "HcnCreateNetwork",
        "HcnOpenNetwork",
        "HcnCreateEndpoint",
        "HcnAddEndpoint",
        "HcnQueryEndpoints",
        "HcnCreateNamespace",
        "HcnAddNamespaceEndpoint",
    ];
    let module_wide = wide("computecore.dll");
    let core = unsafe { LoadLibraryW(module_wide.as_ptr()) };
    let net_wide = wide("computenetwork.dll");
    let net = unsafe { LoadLibraryW(net_wide.as_ptr()) };

    #[link(name = "kernel32")]
    extern "system" {
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    lines.push("=== 候选符号可用性 ===".to_string());
    for name in candidates {
        let mut terminated = name.as_bytes().to_vec();
        terminated.push(0);
        let in_core = !core.is_null()
            && !unsafe { GetProcAddress(core, terminated.as_ptr()) }.is_null();
        let in_net = !net.is_null()
            && !unsafe { GetProcAddress(net, terminated.as_ptr()) }.is_null();
        let where_ = if in_core {
            "computecore"
        } else if in_net {
            "computenetwork"
        } else {
            "-"
        };
        lines.push(format!("  {:<32} {}", name, where_));
    }

    let output = std::env::temp_dir().join("hcs-symbols.txt");
    let mut file = std::fs::File::create(&output).expect("create output");
    for line in &lines {
        writeln!(file, "{}", line).ok();
    }
    println!("written to {}", output.display());
}
