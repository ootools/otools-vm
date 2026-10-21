//! 规格持久化：一台虚拟机一个目录，`vm.json` 是唯一真相。

use crate::model::VmSpec;
use crate::paths;
use std::fs;
use std::path::PathBuf;

/// 读出所有虚拟机规格。单个文件损坏不影响其它虚拟机，只在日志里记一笔。
pub fn list_specs() -> Vec<VmSpec> {
    let root = paths::vm_root_dir().join("vms");
    let mut out = Vec::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let manifest = entry.path().join("vm.json");
        if !manifest.is_file() {
            continue;
        }
        match fs::read_to_string(&manifest).map_err(|error| error.to_string()).and_then(|text| {
            serde_json::from_str::<VmSpec>(&text).map_err(|error| error.to_string())
        }) {
            Ok(spec) => out.push(spec),
            Err(error) => eprintln!("[vm] 跳过损坏的规格 {}: {}", manifest.display(), error),
        }
    }
    out.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    out
}

pub fn load_spec(id: &str) -> Result<VmSpec, String> {
    let manifest = paths::vm_manifest_path(id);
    let text = fs::read_to_string(&manifest)
        .map_err(|error| format!("读取虚拟机配置失败 {}: {}", manifest.display(), error))?;
    serde_json::from_str(&text).map_err(|error| format!("解析虚拟机配置失败: {}", error))
}

pub fn save_spec(spec: &VmSpec) -> Result<(), String> {
    let dir = paths::vm_dir(&spec.id);
    paths::ensure_dir(&dir)?;
    let manifest = paths::vm_manifest_path(&spec.id);
    let text = serde_json::to_string_pretty(spec)
        .map_err(|error| format!("序列化虚拟机配置失败: {}", error))?;
    // 先写临时文件再 rename：中途崩溃不会留下半个 JSON。
    let tmp = manifest.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|error| format!("写入虚拟机配置失败: {}", error))?;
    fs::rename(&tmp, &manifest).map_err(|error| format!("提交虚拟机配置失败: {}", error))?;
    Ok(())
}

/// 删除虚拟机目录。磁盘镜像也在这里，所以调用方必须先让用户确认。
pub fn remove_vm_dir(id: &str) -> Result<(), String> {
    let dir = paths::vm_dir(id);
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).map_err(|error| format!("删除虚拟机目录失败 {}: {}", dir.display(), error))
}

/// 删除位于默认目录之外的磁盘文件。
///
/// 用户把存储目录选到别的盘时，磁盘不在 `vm_dir` 里，`remove_vm_dir` 清不到。
/// 只在确认它**确实不在默认目录里**才动——否则会和 `remove_vm_dir` 抢着删，
/// 或者把别的虚拟机的盘误删（文件名带 id，但目录仍是用户自己挑的）。
///
/// 已转换出的 VHDX（HCS 后端用）一并清掉，否则同样会残留。
pub fn remove_detached_disk(spec: &VmSpec) {
    let default_dir = paths::vm_dir(&spec.id);
    let disk = std::path::Path::new(&spec.disk_path);
    let detached = disk
        .parent()
        .map(|parent| parent != default_dir)
        .unwrap_or(false);
    if !detached {
        return;
    }
    // 失败只记录不报错：删除虚拟机时元数据已清，磁盘残留是可接受的次优结果，
    // 不该因此让整个删除操作失败、把用户卡在「删不掉」的状态里。
    if disk.is_file() {
        if let Err(error) = fs::remove_file(disk) {
            eprintln!("[vm] 删除磁盘失败 {}: {}", disk.display(), error);
        }
    }
    // 同一目录下可能还有 HCS 转换出的 .vhdx。
    let vhdx = disk.with_extension("vhdx");
    if vhdx.is_file() {
        if let Err(error) = fs::remove_file(&vhdx) {
            eprintln!("[vm] 删除磁盘失败 {}: {}", vhdx.display(), error);
        }
    }
}

/// 复制一台虚拟机：规格 + 磁盘 + NVRAM。
///
/// macOS 上用 `cp -c`（APFS clonefile，写时复制）—— 秒级完成、**不占额外空间**，
/// 直到新虚拟机真的写入才分配块。其它平台退化为普通复制。
///
/// NVRAM 必须一起复制：它是客户机的引导项与固件状态，漏掉会让克隆出来的机器
/// 直接落到 EFI shell。
pub fn clone_vm(source: &VmSpec, new_id: &str, new_name: &str) -> Result<VmSpec, String> {
    let target_dir = paths::vm_dir(new_id);
    paths::ensure_dir(&target_dir)?;

    // 克隆出来的盘跟着**源盘所在目录**走：用户把源机器放在大容量盘上，
    // 克隆若回落到用户目录，很快就会把系统盘撑满。
    let target_disk = match std::path::Path::new(&source.disk_path).parent() {
        Some(parent) if parent != paths::vm_dir(&source.id) => {
            parent.join(format!("{}.raw", paths::sanitize_id(new_id)))
        }
        _ => paths::vm_disk_path(new_id),
    };
    clone_file(std::path::Path::new(&source.disk_path), &target_disk)?;

    let source_nvram = paths::vm_nvram_path(&source.id);
    if source_nvram.exists() {
        let target_nvram = paths::vm_nvram_path(new_id);
        clone_file(&source_nvram, &target_nvram)?;
    }

    let mut spec = source.clone();
    spec.id = new_id.to_string();
    spec.name = new_name.to_string();
    spec.disk_path = target_disk.to_string_lossy().to_string();
    spec.created_at = now_seconds();
    save_spec(&spec)?;
    Ok(spec)
}

/// 优先走写时复制，失败再退化为整盘复制。
fn clone_file(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    if !source.exists() {
        return Err(format!("源文件不存在：{}", source.display()));
    }
    #[cfg(target_os = "macos")]
    {
        let status = crate::process::command("cp")
            .arg("-c")
            .arg(source)
            .arg(target)
            .status();
        if let Ok(status) = status {
            if status.success() {
                return Ok(());
            }
        }
        eprintln!("[vm] clonefile 失败，退化为普通复制：{}", source.display());
    }
    std::fs::copy(source, target)
        .map(|_| ())
        .map_err(|error| format!("复制文件失败 {}: {}", source.display(), error))
}

/// 扩容磁盘。只增不减：稀疏文件扩长是安全的，缩短会直接切掉客户机数据。
///
/// ⚠️ 宿主侧扩完只是「块设备变大了」，客户机里还要自己扩分区/文件系统：
/// Linux `growpart` + `resize2fs`，Windows 在「磁盘管理」里扩展卷。
///
/// **虚拟磁盘（VHD/VHDX）必须走专用路径**：`set_len` 只会把文件撑大，不会
/// 更新 VHDX 的元数据与 BAT 表，结果就是宿主上白占几十 GB、客户机里容量
/// 一点没变。这种「看起来成功、实际没生效」必须直接报错，不能放过去。
pub fn resize_disk(spec: &VmSpec, new_size_gb: u64) -> Result<u64, String> {
    if new_size_gb < spec.disk_size_gb {
        return Err(format!(
            "磁盘只能扩大：当前 {} GB，不能缩到 {} GB",
            spec.disk_size_gb, new_size_gb
        ));
    }
    if new_size_gb == spec.disk_size_gb {
        return Ok(new_size_gb);
    }
    let path = std::path::Path::new(&spec.disk_path);
    if !path.exists() {
        return Err(format!("系统盘不存在：{}", spec.disk_path));
    }

    // HCS 后端挂的是同名 `.vhdx`，而 `spec.disk_path` 仍是 `.raw`。
    // 两块都要照顾：先扩真正在用的那块，再同步原始盘，避免下次切回 QEMU
    // 时容量倒退。
    #[cfg(target_os = "windows")]
    {
        use crate::engine::hcs::disk;

        if disk::is_virtual_disk(path) {
            disk::expand_vhdx(path, new_size_gb)?;
            return Ok(new_size_gb);
        }

        // 原始盘是 RAW，但 HCS 可能已经转出了 .vhdx —— 那块才是客户机看到的。
        let vhdx = path.with_extension("vhdx");
        if vhdx.exists() {
            disk::expand_vhdx(&vhdx, new_size_gb)?;
            // 顺手把 RAW 也扩上：不扩的话切回 QEMU 会看到旧容量。
            extend_sparse_file(path, new_size_gb)?;
            return Ok(new_size_gb);
        }
    }

    extend_sparse_file(path, new_size_gb)?;
    Ok(new_size_gb)
}

/// 把文件拉长到指定大小（稀疏语义）。仅适用于 RAW 这类「长度即容量」的镜像。
fn extend_sparse_file(path: &std::path::Path, new_size_gb: u64) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("打开系统盘失败: {}", error))?;
    file.set_len(new_size_gb.saturating_mul(1024 * 1024 * 1024))
        .map_err(|error| format!("扩容系统盘失败: {}", error))
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

/// 创建稀疏 RAW 磁盘。
///
/// VZ 只接受 RAW/ASIF，稀疏文件在宿主上不占实际空间，客户机写入时才落盘。
/// `set_len` 在 APFS / NTFS 上都是稀疏语义。
pub fn create_sparse_disk(path: &PathBuf, size_gb: u64) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        paths::ensure_dir(parent)?;
    }
    let file = fs::File::create(path)
        .map_err(|error| format!("创建磁盘镜像失败 {}: {}", path.display(), error))?;
    let bytes = size_gb.saturating_mul(1024 * 1024 * 1024);
    file.set_len(bytes)
        .map_err(|error| format!("分配磁盘容量失败: {}", error))?;
    Ok(())
}
