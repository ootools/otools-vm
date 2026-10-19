//! 数据目录解析。遵循主仓约定：`~/.otools/local/<tool>/`。

use std::path::PathBuf;

const OTOOLS_DIR: &str = ".otools";
const OTOOLS_LOCAL_DATA_DIR: &str = "local";
const TOOL_SCOPE: &str = "vm";

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.otools/local/vm`
pub fn vm_root_dir() -> PathBuf {
    home_dir()
        .join(OTOOLS_DIR)
        .join(OTOOLS_LOCAL_DATA_DIR)
        .join(TOOL_SCOPE)
}

/// 单台虚拟机的数据目录：磁盘、NVRAM、日志都放这里。
pub fn vm_dir(id: &str) -> PathBuf {
    vm_root_dir().join("vms").join(sanitize_id(id))
}

pub fn vm_manifest_path(id: &str) -> PathBuf {
    vm_dir(id).join("vm.json")
}

pub fn vm_disk_path(id: &str) -> PathBuf {
    vm_dir(id).join("disk.raw")
}

/// EFI 变量存储（NVRAM）。删掉它等于让客户机忘记引导项。
pub fn vm_nvram_path(id: &str) -> PathBuf {
    vm_dir(id).join("nvram.bin")
}

pub fn vm_log_path(id: &str) -> PathBuf {
    vm_dir(id).join("vm.log")
}

/// 存档式快照文件（VZ 的 `.vzvmsave`，内含整机 CPU+RAM+设备状态）。
///
/// 单个文件、不可增量；恢复成功后 VZ 不会自动删除，由插件决定保留还是丢弃。
pub fn vm_snapshot_path(id: &str) -> PathBuf {
    vm_dir(id).join("state.vzvmsave")
}

/// 客户机身份：`VZGenericMachineIdentifier` 的 70 字节序列化。
///
/// **必须持久化**：VZ 每次新建 `VZGenericPlatformConfiguration` 都会随机生成
/// 新身份，身份变了存档就无法恢复（restore 报 "invalid argument"）。
pub fn vm_machine_id_path(id: &str) -> PathBuf {
    vm_dir(id).join("machine.id")
}

/// 网卡 MAC 地址。
///
/// **同样必须持久化**：`VZVirtioNetworkDeviceConfiguration` 每次构建都会随机
/// 生成 MAC，与存档里的设备状态对不上时 restore 同样失败。
pub fn vm_mac_path(id: &str) -> PathBuf {
    vm_dir(id).join("mac.txt")
}

/// 插件 id 只允许安全字符，避免路径穿越。
fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "vm".to_string()
    } else {
        cleaned
    }
}

pub fn ensure_dir(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| format!("创建目录失败 {}: {}", path.display(), error))
}
