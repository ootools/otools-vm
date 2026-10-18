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
