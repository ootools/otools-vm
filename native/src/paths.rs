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
///
/// HCS 后端的虚拟机 id 也复用它：HCS 的 id 会进系统服务，同样不能带奇怪字符。
pub fn sanitize_id(id: &str) -> String {
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

/// 解析系统盘的最终落盘路径。
///
/// `custom` 为空时沿用默认目录（与元数据同处）；非空时把盘放到该目录下，
/// 并带上虚拟机 id 作为文件名——多个虚拟机共用一个存储目录时不会互相覆盖。
///
/// **只有磁盘会搬**：`vm.json`、NVRAM、日志这些必须留在默认目录，因为
/// `list_specs` 是靠扫默认目录发现虚拟机的，元数据搬走机器就从列表里消失了。
pub fn resolve_disk_path(id: &str, custom: Option<&str>) -> Result<PathBuf, String> {
    let Some(raw) = custom.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(vm_disk_path(id));
    };
    let dir = PathBuf::from(raw);
    if !dir.is_absolute() {
        return Err(format!("存储目录必须是绝对路径：{}", raw));
    }
    // 文件名带 id：同一个目录放多台机器时不会撞名。
    Ok(dir.join(format!("{}.raw", sanitize_id(id))))
}

/// 磁盘所在卷的可用空间（字节）。
///
/// 用于在建盘前拦一下「明明装不下还硬建」。取不到时返回 `None`，调用方应当
/// 放行而不是误报——查询失败不该等于「没有空间」。
pub fn available_bytes(path: &std::path::Path) -> Option<u64> {
    // `fs2` 之类会引进新依赖；这里直接问系统：
    // Windows 用 `GetDiskFreeSpaceExW`，类 Unix 用 `statvfs`。
    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(
                directory: *const u16,
                free_for_caller: *mut u64,
                total: *mut u64,
                total_free: *mut u64,
            ) -> i32;
        }
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        // 目标目录可能还不存在，先退到最近的已存在祖先。
        let mut probe = path.to_path_buf();
        while !probe.exists() {
            match probe.parent() {
                Some(parent) if parent != probe => probe = parent.to_path_buf(),
                _ => return None,
            }
        }
        let wide: Vec<u16> = OsStr::new(&probe)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free_for_caller: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_for_caller,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            None
        } else {
            Some(free_for_caller)
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // 类 Unix 用 `df -k` 避免引入 libc 绑定差异；输出第二列是可用 KB。
        let probe = if path.exists() {
            path.to_path_buf()
        } else {
            path.ancestors()
                .find(|candidate| candidate.exists())
                .map(|candidate| candidate.to_path_buf())?
        };
        let output = crate::process::command("df")
            .arg("-k")
            .arg(&probe)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().nth(1)?;
        let available_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
        Some(available_kb.saturating_mul(1024))
    }
}
