//! OTools VM 原生层。
//!
//! 编译成两种形态：
//! - `cdylib`：插件市场里以 `lib/macOS.dylib` 等形式被宿主 `libloading` 加载，
//!   入口是 `otools_plugin_invoke` / `otools_plugin_free`；
//! - `rlib`：给未来的独立 APP 壳直接引用。
//!
//! FFI 边界一律走 [`guard_invoke`]：插件里的 panic 不能跨 DLL 边界 unwind，
//! 否则宿主整个进程会被 `abort()` 掉。

pub mod engine;
pub mod model;
pub mod paths;
pub mod settings;
pub mod store;

use std::sync::Arc;

use serde_json::{json, Value};

use engine::{Engine, ViewRect};
use model::{
    EngineInfo, GuestOs, HostInfo, SharedDir, VmCloneInput, VmCreateInput, VmResizeDiskInput,
    VmSpec, VmSummary, VmUpdateInput, VmWorkspace,
};

/// 当前引擎。可替换：用户装完 QEMU 后点「重新检测」需要能换掉。
///
/// 不能用 `OnceLock`：它一旦初始化就再也改不了，等于「装了 QEMU 也必须重启
/// 宿主」——正是我们要消除的体验断点。换成 RwLock 后重新探测只是换一个 Box。
static ENGINE: std::sync::RwLock<Option<Arc<dyn Engine>>> = std::sync::RwLock::new(None);

fn engine() -> Arc<dyn Engine> {
    if let Ok(guard) = ENGINE.read() {
        if let Some(current) = guard.as_ref() {
            return Arc::clone(current);
        }
    }
    let fresh: Arc<dyn Engine> = Arc::from(engine::build_engine());
    if let Ok(mut guard) = ENGINE.write() {
        // 双重检查：并发进来时以先写入的为准，避免各自建一份引擎互相覆盖
        // （QEMU 引擎持子进程表，两份会互相看不见对方的虚拟机）。
        if let Some(existing) = guard.as_ref() {
            return Arc::clone(existing);
        }
        *guard = Some(Arc::clone(&fresh));
    }
    fresh
}

/// 丢弃现有引擎并重新探测。用于「装好 QEMU 后重新检测」。
///
/// **只在没有虚拟机在跑时调用**：换引擎等于丢掉旧引擎的子进程表，
/// 旧 QEMU 进程会变成孤儿继续占内存，而界面上再也管不到它们。
fn rebuild_engine() -> Arc<dyn Engine> {
    let replacement: Arc<dyn Engine> = Arc::from(engine::build_engine());
    if let Ok(mut guard) = ENGINE.write() {
        *guard = Some(Arc::clone(&replacement));
    }
    replacement
}

/// 装完 QEMU 后在后台静默重探。
///
/// 与 [`cmd_engine_reprobe`] 的区别：这个不报错。它由安装线程在成功那一刻调用，
/// 此时若有虚拟机在跑（理论上不太可能，用户刚点了安装），重建引擎会让那些
/// 机器变成孤儿——所以宁可**跳过重建**，等用户自己重启，也不要冒险。
pub(crate) fn reprobe_engine_silently() {
    let busy = store::list_specs().into_iter().any(|spec| {
        let current = engine();
        let state = current.state(&spec);
        state != model::VmState::Stopped
            && state != model::VmState::Error
            && state != model::VmState::Unavailable
    });
    if busy {
        return;
    }
    let _ = rebuild_engine();
}

// ---------------------------------------------------------------- 命令实现

fn cmd_workspace() -> Result<Value, String> {
    let engine = engine();
    let specs = store::list_specs();
    let mut vms = Vec::with_capacity(specs.len());
    for spec in specs {
        vms.push(summary_for(engine.as_ref(), &spec));
    }
    let workspace = VmWorkspace {
        engine: engine.info(),
        vms,
        host: host_info(),
    };
    serde_json::to_value(workspace).map_err(|error| error.to_string())
}

fn summary_for(engine: &dyn Engine, spec: &VmSpec) -> VmSummary {
    let state = if engine.supports(spec) {
        engine.state(spec)
    } else {
        model::VmState::Unavailable
    };
    VmSummary {
        spec: spec.clone(),
        state,
        engine: if engine.supports(spec) {
            engine.info().kind
        } else {
            "none".to_string()
        },
        last_error: engine.last_error(spec),
    }
}

fn cmd_engine_info() -> Result<Value, String> {
    serde_json::to_value(engine().info()).map_err(|error| error.to_string())
}

/// 重新探测引擎，用于「刚装好 QEMU」的场景。
///
/// 有虚拟机在跑时拒绝重建：换引擎会丢掉旧引擎的子进程表，正在运行的 QEMU
/// 会变成没人管的孤儿进程——既不显示在界面上，也停不掉。
fn cmd_engine_reprobe() -> Result<Value, String> {
    let running: Vec<String> = store::list_specs()
        .into_iter()
        .filter(|spec| {
            let state = engine().state(spec);
            state != model::VmState::Stopped
                && state != model::VmState::Error
                && state != model::VmState::Unavailable
        })
        .map(|spec| spec.name)
        .collect();
    if !running.is_empty() {
        return Err(format!(
            "请先关闭所有虚拟机再重新检测（正在运行：{}）",
            running.join("、")
        ));
    }
    let engine = rebuild_engine();
    serde_json::to_value(engine.info()).map_err(|error| error.to_string())
}

fn cmd_host_info() -> Result<Value, String> {
    serde_json::to_value(host_info()).map_err(|error| error.to_string())
}

fn cmd_create(payload: &Value) -> Result<Value, String> {
    let input: VmCreateInput = serde_json::from_value(payload.clone())
        .map_err(|error| format!("创建参数解析失败: {}", error))?;

    let engine = engine();
    let info = engine.info();
    if !info.available {
        return Err(info.reason);
    }

    let id = new_vm_id();
    let arch = if input.arch.trim().is_empty() {
        if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" }.to_string()
    } else {
        input.arch.clone()
    };

    let disk_path = paths::resolve_disk_path(&id, input.storage_dir.as_deref())?;

    // 建盘前先确认放得下。几 GB 的稀疏文件在写满前不会真占空间，但用户
    // 一旦往里装系统就会立刻要这么多——先拦下来比装到一半失败好。
    let needed = input.disk_size_gb.saturating_mul(1024 * 1024 * 1024);
    if let Some(available) = paths::available_bytes(&disk_path) {
        // 留 1 GB 余量给元数据与文件系统开销。
        let reserve = 1024 * 1024 * 1024;
        if available < needed.saturating_add(reserve) {
            return Err(format!(
                "存储目录空间不足：需要约 {} GB 可用，当前只剩 {} GB。请换一个目录或减小磁盘容量。",
                needed / (1024 * 1024 * 1024) + 1,
                available / (1024 * 1024 * 1024)
            ));
        }
    }

    store::create_sparse_disk(&disk_path, input.disk_size_gb)?;

    let shared_dirs: Vec<SharedDir> = input
        .shared_dirs
        .into_iter()
        .enumerate()
        .map(|(index, dir)| {
            let tag = if dir.mount_tag.trim().is_empty() {
                SharedDir::derive_tag(&dir.host_path, index)
            } else {
                dir.mount_tag
            };
            SharedDir {
                host_path: dir.host_path,
                mount_tag: tag,
                protocol: dir.protocol,
                read_only: dir.read_only,
            }
        })
        .collect();

    let spec = VmSpec {
        id: id.clone(),
        name: input.name,
        os: input.os,
        arch,
        cpus: input.cpus,
        memory_mb: input.memory_mb,
        disk_path: disk_path.to_string_lossy().to_string(),
        disk_size_gb: input.disk_size_gb,
        installer_path: input.installer_path.filter(|path| !path.trim().is_empty()),
        firmware: input
            .firmware
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "bios".to_string()),
        firmware_path: input.firmware_path.filter(|path| !path.trim().is_empty()),
        driver_iso: input.driver_iso.filter(|path| !path.trim().is_empty()),
        virtiofsd_path: input.virtiofsd_path.filter(|path| !path.trim().is_empty()),
        network: input.network.unwrap_or_default(),
        network_iface: input
            .network_iface
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        boot_order: input.boot_order,
        usb_media: input.usb_media.filter(|path| !path.trim().is_empty()),
        port_forwards: input.port_forwards,
        shared_dirs,
        clipboard: input.clipboard.unwrap_or(true),
        auto_resize: input.auto_resize.unwrap_or(true),
        audio: input.audio.unwrap_or(true),
        rosetta: input.rosetta.unwrap_or(false),
        display_width: input.display_width.unwrap_or(1280),
        display_height: input.display_height.unwrap_or(800),
        notes: String::new(),
        created_at: now_seconds(),
    };
    spec.validate()?;
    store::save_spec(&spec)?;

    serde_json::to_value(summary_for(engine.as_ref(), &spec)).map_err(|error| error.to_string())
}

fn cmd_update(payload: &Value) -> Result<Value, String> {
    let input: VmUpdateInput = serde_json::from_value(payload.clone())
        .map_err(|error| format!("更新参数解析失败: {}", error))?;

    let engine = engine();
    let mut spec = store::load_spec(&input.id)?;

    let running = engine.state(&spec);
    if running.is_busy() || running == model::VmState::Running || running == model::VmState::Paused {
        return Err("请先关闭虚拟机再修改配置".to_string());
    }

    spec.name = input.name;
    spec.cpus = input.cpus;
    spec.memory_mb = input.memory_mb;
    spec.shared_dirs = input
        .shared_dirs
        .into_iter()
        .enumerate()
        .map(|(index, dir)| {
            let tag = if dir.mount_tag.trim().is_empty() {
                SharedDir::derive_tag(&dir.host_path, index)
            } else {
                dir.mount_tag
            };
            SharedDir {
                host_path: dir.host_path,
                mount_tag: tag,
                protocol: dir.protocol,
                read_only: dir.read_only,
            }
        })
        .collect();
    spec.boot_order = input.boot_order;
    spec.usb_media = input.usb_media.filter(|path| !path.trim().is_empty());
    spec.port_forwards = input.port_forwards;
    if let Some(value) = input.clipboard {
        spec.clipboard = value;
    }
    if let Some(value) = input.auto_resize {
        spec.auto_resize = value;
    }
    if let Some(value) = input.audio {
        spec.audio = value;
    }
    if let Some(value) = input.rosetta {
        spec.rosetta = value;
    }
    if let Some(value) = input.installer_path {
        spec.installer_path = if value.trim().is_empty() { None } else { Some(value) };
    }
    if let Some(value) = input.firmware {
        let value = value.trim();
        if !value.is_empty() {
            spec.firmware = value.to_string();
        }
    }
    if let Some(value) = input.firmware_path {
        spec.firmware_path = if value.trim().is_empty() { None } else { Some(value) };
    }
    if let Some(value) = input.driver_iso {
        spec.driver_iso = if value.trim().is_empty() { None } else { Some(value) };
    }
    if let Some(value) = input.virtiofsd_path {
        spec.virtiofsd_path = if value.trim().is_empty() { None } else { Some(value) };
    }
    if let Some(value) = input.network {
        spec.network = value;
        // 从非 NAT 切回 NAT 时把网卡选择一并清掉：留着会让下一次切到桥接时
        // 拿到一个早已失效的网卡名。
        if value == model::NetworkMode::Nat {
            spec.network_iface = None;
        }
    }
    if let Some(value) = input.network_iface {
        let trimmed = value.trim().to_string();
        spec.network_iface = if trimmed.is_empty() { None } else { Some(trimmed) };
    }
    if let Some(value) = input.notes {
        spec.notes = value;
    }
    spec.validate()?;
    store::save_spec(&spec)?;

    serde_json::to_value(summary_for(engine.as_ref(), &spec)).map_err(|error| error.to_string())
}

fn cmd_delete(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let engine = engine();
    if let Ok(spec) = store::load_spec(&id) {
        let _ = engine.stop(&spec);
        let _ = engine.detach_view(&spec);
        // 磁盘可能在用户选的自定义目录里，`remove_vm_dir` 只清默认目录。
        // 不单独删就会留下几十 GB 的孤儿文件，而界面上已经看不到这台机器了。
        store::remove_detached_disk(&spec);
    }
    store::remove_vm_dir(&id)?;
    Ok(json!({ "ok": true }))
}

fn cmd_start(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    let engine = engine();
    if !engine.supports(&spec) {
        return Err(format!(
            "当前引擎（{}）不支持 {} 架构的客户机",
            engine.info().label,
            spec.arch
        ));
    }
    engine.start(&spec)?;
    Ok(json!({ "ok": true }))
}

fn cmd_stop(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    engine().stop(&spec)?;
    Ok(json!({ "ok": true }))
}

fn cmd_pause(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    engine().pause(&spec)?;
    Ok(json!({ "ok": true }))
}

fn cmd_resume(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    engine().resume(&spec)?;
    Ok(json!({ "ok": true }))
}

/// 存档式快照能力：该配置能不能存、有没有现成存档、当前任务进度。
///
/// 逐台判定，因为 VZ 的 `validateSaveRestoreSupport` 是按配置算的
/// （设备组合变了结果就可能变）；进度也挂在这里，前端一个轮询就够。
fn cmd_snapshot_support(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    let engine = engine();
    let support = if engine.supports(&spec) {
        engine.snapshot_support(&spec)
    } else {
        model::SnapshotSupport::unsupported()
    };
    serde_json::to_value(support).map_err(|error| error.to_string())
}

/// 发起存档。**立即返回**，进度与结果看 `vm_snapshot_support` 的 phase/error。
fn cmd_save_state(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    let engine = engine();
    if !engine.supports(&spec) {
        return Err(format!("当前引擎（{}）不支持该虚拟机", engine.info().label));
    }
    engine.save_state(&spec)?;
    serde_json::to_value(engine.snapshot_support(&spec)).map_err(|error| error.to_string())
}

/// 发起恢复。**立即返回**，进度与结果看 `vm_snapshot_support` 的 phase/error。
fn cmd_restore_state(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    let engine = engine();
    if !engine.supports(&spec) {
        return Err(format!("当前引擎（{}）不支持该虚拟机", engine.info().label));
    }
    engine.restore_state(&spec)?;
    serde_json::to_value(engine.snapshot_support(&spec)).map_err(|error| error.to_string())
}

fn cmd_discard_state(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    engine().discard_state(&spec)?;
    Ok(json!({ "ok": true }))
}

fn cmd_attach_view(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let rect = read_rect(payload)?;
    let spec = store::load_spec(&id)?;
    engine().attach_view(&spec, rect)?;
    Ok(json!({ "ok": true }))
}

fn cmd_update_view_rect(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let rect = read_rect(payload)?;
    let spec = store::load_spec(&id)?;
    engine().update_view_rect(&spec, rect)?;
    Ok(json!({ "ok": true }))
}

fn cmd_detach_view(payload: &Value) -> Result<Value, String> {
    let id = read_id(payload)?;
    let spec = store::load_spec(&id)?;
    engine().detach_view(&spec)?;
    Ok(json!({ "ok": true }))
}

fn cmd_pick_folder(payload: &Value) -> Result<Value, String> {
    let title = payload
        .get("title")
        .and_then(|value| value.as_str())
        .unwrap_or("选择共享目录");
    pick_path(title, true)
}

fn cmd_pick_file(payload: &Value) -> Result<Value, String> {
    let title = payload
        .get("title")
        .and_then(|value| value.as_str())
        .unwrap_or("选择安装镜像");
    pick_path(title, false)
}

fn pick_path(title: &str, directory: bool) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        let picked = if directory {
            engine::vz::dialog::pick_directory(title)?
        } else {
            engine::vz::dialog::pick_file(title)?
        };
        Ok(json!({ "path": picked }))
    }
    // 非 macOS 宿主没有本机原生面板——**但要回到前端去走宿主提供的
    // `otools.dialog.open()`**（Tauri 的 tauri-plugin-dialog，网页/App 都能用），
    // 而不是在这里自己造一个。这里返回错误即是给前端的信号：
    // 收到该错误就切换到宿主对话框，两边都不再依赖「用户手动粘贴路径」。
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, directory);
        Err(PICK_DIALOG_UNAVAILABLE.to_string())
    }
}

/// 非 macOS 宿主上 `vm_pick_file` / `vm_pick_folder` 的统一错误标记。
///
/// 前端用它精确判断「不是真失败，是该走宿主对话框了」，避免用模糊的
/// 字符串匹配或把真正的错误也一并吞掉。
const PICK_DIALOG_UNAVAILABLE: &str = "PICK_DIALOG_UNAVAILABLE";

fn cmd_clone(payload: &Value) -> Result<Value, String> {
    let input: VmCloneInput = serde_json::from_value(payload.clone())
        .map_err(|error| format!("克隆参数解析失败: {}", error))?;
    let engine = engine();
    let source = store::load_spec(&input.id)?;

    let name = input.name.trim();
    if name.is_empty() {
        return Err("克隆后的名称不能为空".to_string());
    }

    // 克隆必须在一台「安静」的机器上做：运行中复制会拿到撕裂的磁盘状态。
    let state = engine.state(&source);
    if state != model::VmState::Stopped && state != model::VmState::Error {
        return Err("请先关闭虚拟机再克隆".to_string());
    }

    let new_id = new_vm_id();
    let clone = store::clone_vm(&source, &new_id, name)?;
    serde_json::to_value(summary_for(engine.as_ref(), &clone)).map_err(|error| error.to_string())
}

fn cmd_resize_disk(payload: &Value) -> Result<Value, String> {
    let input: VmResizeDiskInput = serde_json::from_value(payload.clone())
        .map_err(|error| format!("扩容参数解析失败: {}", error))?;
    let engine = engine();
    let mut spec = store::load_spec(&input.id)?;

    let state = engine.state(&spec);
    if state != model::VmState::Stopped && state != model::VmState::Error {
        return Err("请先关闭虚拟机再扩容磁盘".to_string());
    }

    let size = store::resize_disk(&spec, input.disk_size_gb)?;
    spec.disk_size_gb = size;
    store::save_spec(&spec)?;

    serde_json::to_value(summary_for(engine.as_ref(), &spec)).map_err(|error| error.to_string())
}

/// 宿主网卡清单：供前端在桥接 / 仅主机模式下选择具体网卡。
///
/// 只返回当前「已连接且有 IPv4」的网卡——没有地址的网卡桥接过去也是死路。
fn cmd_network_interfaces() -> Result<Value, String> {
    let interfaces = engine::qemu::network::list_interfaces();
    serde_json::to_value(interfaces).map_err(|error| error.to_string())
}

/// 各网络模式在当前宿主上的可用性：能不能用、不能用时为什么、怎么解决。
///
/// 前端据此禁用不可选模式并直接展示 `reason`，而不是让用户点了才看到启动失败。
fn cmd_get_settings() -> Result<Value, String> {
    serde_json::to_value(settings::load()).map_err(|error| error.to_string())
}

/// 切换引擎偏好。
///
/// 只写设置、不切换本进程已选定的引擎：引擎是进程级单例，切换需要重启插件
/// 并让所有虚拟机先停下——那不是一次设置修改该附带的行为。前端负责在提示
/// 「重启后生效」之前先确认没有运行中的机器。
fn cmd_set_engine_preference(payload: &Value) -> Result<Value, String> {
    let raw = payload
        .get("preference")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "缺少 preference".to_string())?;
    let preference = match raw {
        "auto" => settings::EnginePreference::Auto,
        "hcs" => settings::EnginePreference::Hcs,
        "qemu" => settings::EnginePreference::Qemu,
        other => return Err(format!("未知的引擎偏好: {}", other)),
    };
    let mut current = settings::load();
    current.engine_preference = preference;
    settings::save(&current)?;
    serde_json::to_value(current).map_err(|error| error.to_string())
}

/// HCS 引擎的可用性详情。
///
/// 单独给前端一个命令（而不是只看 `vm_engine_info` 的 reason 字符串），因为
/// 用户要据此做动作：缺权限去提权，缺组件去开系统功能——两者完全不同。
#[cfg(target_os = "windows")]
fn cmd_hcs_status() -> Result<Value, String> {
    let probe = engine::hcs::probe();
    Ok(json!({
        "supported": true,
        "available": probe.available,
        "needsElevation": probe.needs_elevation,
        "elevated": engine::hcs::is_elevated(),
        "inHyperVAdmins": engine::hcs::in_hyperv_admins(),
        "reason": probe.reason,
        "version": probe.version,
    }))
}

#[cfg(not(target_os = "windows"))]
fn cmd_hcs_status() -> Result<Value, String> {
    Ok(json!({
        "supported": false,
        "available": false,
        "needsElevation": false,
        "elevated": false,
        "inHyperVAdmins": false,
        "reason": engine::hcs::unsupported_reason(),
        "version": "",
    }))
}

/// HCS 后端在 HCS 里遗留的虚拟机（界面看不见、但确实在跑）。
///
/// 场景：插件崩溃、宿主被强杀、或换了引擎。这些机器会一直占着内存和 VHDX，
/// 以前只能让用户去任务管理器杀 `vmwp.exe`。现在给出清单让用户一键清掉。
///
/// 非 Windows / 当前不是 HCS 引擎时恒返回空数组，调用方不必自己判断平台。
fn cmd_hcs_orphans() -> Result<Value, String> {
    #[cfg(target_os = "windows")]
    {
        if engine().info().kind != "hcs" {
            return Ok(json!({ "orphans": Vec::<String>::new() }));
        }
        // 这里必须构造具体类型：`list_orphans` 不在 `Engine` trait 上（只有 HCS 有），
        // 且它不需要 `start()` 过的状态，重新探测一次即可。
        let hcs = engine::hcs::HcsEngine::create();
        Ok(json!({ "orphans": hcs.list_orphans() }))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(json!({ "orphans": Vec::<String>::new() }))
    }
}

/// 按 spec id 直接收掉孤儿虚拟机。
///
/// **不能复用 `vm_stop`**：那条路径要先 `store::load_spec(id)`，而孤儿机器的
/// spec 往往已经被用户删掉了（正是删了才成孤儿），查不到就直接报错，永远
/// 清不掉。所以这里绕开 store，直接走 HCS 的 open + shutdown/terminate。
///
/// 传空数组表示「全部清理」。单个失败不影响其它项，失败项如实回报。
fn cmd_hcs_orphans_kill(payload: &Value) -> Result<Value, String> {
    #[cfg(target_os = "windows")]
    {
        let requested: Vec<String> = payload
            .get("ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if engine().info().kind != "hcs" {
            return Ok(json!({ "killed": Vec::<String>::new(), "failed": Vec::<String>::new() }));
        }
        let hcs = engine::hcs::HcsEngine::create();
        // 空清单 = 清全部；先把「当前真实存在的孤儿」取出来做交集，
        // 避免前端拿旧清单来打一个已经被清掉的 id。
        let live = hcs.list_orphans();
        let targets: Vec<String> = if requested.is_empty() {
            live
        } else {
            requested.into_iter().filter(|id| live.contains(id)).collect()
        };
        let (killed, failed) = hcs.kill_orphans(&targets);
        Ok(json!({ "killed": killed, "failed": failed }))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = payload;
        Ok(json!({ "killed": Vec::<String>::new(), "failed": Vec::<String>::new() }))
    }
}

fn cmd_network_backend(payload: &Value) -> Result<Value, String> {
    let mut items = Vec::new();
    for mode in [
        model::NetworkMode::Nat,
        model::NetworkMode::Bridged,
        model::NetworkMode::HostOnly,
    ] {
        let probe = engine::qemu::network::check_backend(mode);
        items.push(json!({
            "mode": engine::qemu::network::mode_label(mode),
            "available": probe.is_ok(),
            "reason": probe.err().unwrap_or_default(),
            "needsInterface": mode.needs_interface(),
        }));
    }
    let smb_probe = engine::qemu::smb::check_capability();
    let value = json!({
        "modes": items,
        "smb": {
            "available": smb_probe.is_ok(),
            "reason": smb_probe.err().unwrap_or_default(),
        },
        "hostAddress": payload
            .get("id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .and_then(|id| store::load_spec(id).ok())
            .map(|spec| engine::qemu::smb::host_address_for_guest(&spec))
            .unwrap_or_default(),
    });
    Ok(value)
}

/// Rosetta 在宿主上的可用状态：`installed` / `notInstalled` / `notSupported`。
fn cmd_rosetta_status() -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_virtualization::{VZLinuxRosettaAvailability, VZLinuxRosettaDirectoryShare};
        if !cfg!(target_arch = "aarch64") {
            return Ok(json!({ "status": "notSupported" }));
        }
        let availability = unsafe { VZLinuxRosettaDirectoryShare::availability() };
        let status = if availability == VZLinuxRosettaAvailability::Installed {
            "installed"
        } else if availability == VZLinuxRosettaAvailability::NotInstalled {
            "notInstalled"
        } else {
            "notSupported"
        };
        Ok(json!({ "status": status }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(json!({ "status": "notSupported" }))
    }
}

/// 把所有已挂载的原生视图从宿主窗口摘下来。
fn detach_all_views() {
    let engine = engine();
    for spec in store::list_specs() {
        let _ = engine.detach_view(&spec);
    }
}

fn cmd_shutdown() -> Result<Value, String> {
    if let Ok(guard) = ENGINE.read() {
        if let Some(engine) = guard.as_ref() {
            engine.shutdown();
        }
    }
    Ok(json!({ "ok": true }))
}

/// QEMU 的一键获取建议：走哪条路、需要用户做什么、失败时给什么命令。
///
/// 单独一个命令而不是塞进 `vm_engine_info`：引擎信息是「现在能不能用」，
/// 这里是「不能用时怎么办」，前端展示位置与刷新时机都不同。
fn cmd_qemu_plan() -> Result<Value, String> {
    let plan = engine::qemu::install::plan();
    Ok(json!({
        "automatic": plan.automatic,
        "strategy": plan.strategy,
        "note": plan.note,
        "command": plan.command,
        "reason": plan.reason,
        "supportedPlatform": engine::qemu::install::is_supported_platform(),
        "binaryPath": engine::qemu::install::binary_path().to_string_lossy().to_string(),
    }))
}

/// 发起 QEMU 获取。**立即返回**，进度看 `vm_qemu_install_status`。
fn cmd_qemu_install() -> Result<Value, String> {
    engine::qemu::install::start_install()?;
    serde_json::to_value(engine::qemu::install::status()).map_err(|error| error.to_string())
}

fn cmd_qemu_install_status() -> Result<Value, String> {
    serde_json::to_value(engine::qemu::install::status()).map_err(|error| error.to_string())
}

/// 某个目录所在卷的可用空间，供创建对话框实时提示。
///
/// 传空路径时返回默认数据目录的数值。取不到就给 `null`，前端据此隐藏提示
/// 而不是误报「空间不足」。
fn cmd_disk_space(payload: &Value) -> Result<Value, String> {
    let raw = payload
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let probe = if raw.is_empty() {
        paths::vm_root_dir()
    } else {
        std::path::PathBuf::from(&raw)
    };
    Ok(json!({
        "path": probe.to_string_lossy().to_string(),
        "availableBytes": paths::available_bytes(&probe),
    }))
}

// ---------------------------------------------------------------- 小工具

fn read_id(payload: &Value) -> Result<String, String> {
    payload
        .get("id")
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少虚拟机 id".to_string())
}

fn read_rect(payload: &Value) -> Result<ViewRect, String> {
    let rect = payload
        .get("rect")
        .cloned()
        .ok_or_else(|| "缺少控制台区域".to_string())?;
    serde_json::from_value(rect).map_err(|error| format!("控制台区域解析失败: {}", error))
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

fn new_vm_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos() as u64)
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("vm-{:x}{:02x}", nanos, seq)
}

fn host_info() -> HostInfo {
    let total_memory_mb = total_memory_bytes()
        .map(|bytes| bytes / 1024 / 1024)
        .unwrap_or(0);
    let cpus = std::thread::available_parallelism()
        .map(|value| value.get() as u32)
        .unwrap_or(4);
    HostInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        hostname: std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "localhost".to_string()),
        recommended_cpus: cpus.saturating_sub(2).max(2),
        recommended_memory_mb: if total_memory_mb == 0 {
            4096
        } else {
            (total_memory_mb / 2).max(2048)
        },
        total_memory_mb,
    }
}

fn total_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
        let kb: u64 = line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse().ok())?;
        Some(kb * 1024)
    }
}

// ---------------------------------------------------------------- 分发

fn dispatch(method: &str, payload: Value) -> Result<Value, String> {
    match method {
        "ping" => Ok(json!({ "message": "pong", "engine": engine().info().kind })),
        "vm_get_workspace" => cmd_workspace(),
        "vm_engine_info" => cmd_engine_info(),
        "vm_engine_reprobe" => cmd_engine_reprobe(),
        "vm_qemu_plan" => cmd_qemu_plan(),
        "vm_qemu_install" => cmd_qemu_install(),
        "vm_qemu_install_status" => cmd_qemu_install_status(),
        "vm_disk_space" => cmd_disk_space(&payload),
        "vm_host_info" => cmd_host_info(),
        "vm_create" => cmd_create(&payload),
        "vm_update" => cmd_update(&payload),
        "vm_delete" => cmd_delete(&payload),
        "vm_start" => cmd_start(&payload),
        "vm_stop" => cmd_stop(&payload),
        "vm_pause" => cmd_pause(&payload),
        "vm_resume" => cmd_resume(&payload),
        "vm_snapshot_support" => cmd_snapshot_support(&payload),
        "vm_save_state" => cmd_save_state(&payload),
        "vm_restore_state" => cmd_restore_state(&payload),
        "vm_discard_state" => cmd_discard_state(&payload),
        "vm_attach_view" => cmd_attach_view(&payload),
        "vm_update_view_rect" => cmd_update_view_rect(&payload),
        "vm_detach_view" => cmd_detach_view(&payload),
        "vm_clone" => cmd_clone(&payload),
        "vm_resize_disk" => cmd_resize_disk(&payload),
        "vm_network_interfaces" => cmd_network_interfaces(),
        "vm_network_backend" => cmd_network_backend(&payload),
        "vm_hcs_status" => cmd_hcs_status(),
        "vm_hcs_orphans" => cmd_hcs_orphans(),
        "vm_hcs_orphans_kill" => cmd_hcs_orphans_kill(&payload),
        "vm_get_settings" => cmd_get_settings(),
        "vm_set_engine_preference" => cmd_set_engine_preference(&payload),
        "vm_rosetta_status" => cmd_rosetta_status(),
        "vm_pick_folder" => cmd_pick_folder(&payload),
        "vm_pick_file" => cmd_pick_file(&payload),
        "vm_shutdown" => cmd_shutdown(),
        // 宿主热更新插件前的停机握手：只摘掉挂在宿主窗口上的原生视图，
        // 正在运行的虚拟机保持不动 —— 更新插件不该顺手杀掉用户的机器。
        "__otools_shutdown__" => {
            detach_all_views();
            Ok(json!({ "ok": true }))
        }
        // 宿主会每 200ms 轮询一次；本插件不排队事件，状态由前端主动拉。
        "poll_events" => Ok(json!({ "events": [] })),
        other => Err(format!("Unknown method: {}", other)),
    }
}

// ---------------------------------------------------------------- FFI

/// 在 FFI 边界内捕获 panic，返回宿主可解析的 JSON。
fn guard_invoke<F>(
    input_ptr: *const u8,
    input_len: usize,
    output_len: *mut usize,
    logic: F,
) -> *mut u8
where
    F: FnOnce(&[u8]) -> Result<Value, String>,
{
    if input_ptr.is_null() || output_len.is_null() {
        return std::ptr::null_mut();
    }

    let input = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| logic(input)));

    let response = match outcome {
        Ok(Ok(value)) => {
            if value.get("ok").is_some() {
                value
            } else {
                json!({ "ok": true, "data": value })
            }
        }
        Ok(Err(error)) => json!({ "ok": false, "error": error }),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            eprintln!("[vm] 插件内部 panic: {}", message);
            json!({ "ok": false, "error": format!("插件内部错误: {}", message), "panic": true })
        }
    };

    let mut bytes = serde_json::to_vec(&response)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"serialize failed"}"#.to_vec());
    if bytes.is_empty() {
        bytes.extend_from_slice(b"{}");
    }
    let len = bytes.len();
    unsafe {
        *output_len = len;
    }
    let ptr = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    ptr
}

fn invoke_impl(input: &[u8]) -> Result<Value, String> {
    let parsed: Value = serde_json::from_slice(input)
        .map_err(|error| format!("请求解析失败: {}", error))?;
    let method = parsed
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let payload = parsed.get("payload").cloned().unwrap_or(Value::Null);
    dispatch(&method, payload)
}

#[no_mangle]
pub extern "C" fn otools_plugin_invoke(
    input_ptr: *const u8,
    input_len: usize,
    output_len: *mut usize,
) -> *mut u8 {
    guard_invoke(input_ptr, input_len, output_len, invoke_impl)
}

#[no_mangle]
pub extern "C" fn otools_plugin_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

/// 供独立 APP 壳直接调用的 Rust API（不经 JSON）。
pub fn workspace_json() -> String {
    match cmd_workspace() {
        Ok(value) => value.to_string(),
        Err(error) => json!({ "error": error }).to_string(),
    }
}

pub fn engine_info() -> EngineInfo {
    engine().info()
}

#[allow(dead_code)]
fn guest_label(os: GuestOs) -> &'static str {
    match os {
        GuestOs::Windows => "windows",
        GuestOs::Linux => "linux",
        GuestOs::Macos => "macos",
        GuestOs::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_method_is_rejected() {
        assert!(dispatch("nope", Value::Null).is_err());
    }

    #[test]
    fn id_is_required() {
        assert!(read_id(&json!({})).is_err());
        assert_eq!(read_id(&json!({ "id": " abc " })).unwrap(), "abc");
    }

    #[test]
    fn share_tag_is_guest_safe() {
        // 全非 ASCII 目录名：退回 shareN，保证客户机侧标签唯一且可用。
        assert_eq!(SharedDir::derive_tag("/Users/me/我的 项目", 0), "share1");
        // 含大写与空格：小写化并替换分隔符，追加序号避免重名。
        assert_eq!(SharedDir::derive_tag("/Users/me/My Projects", 0), "my-projects-1");
        let tag = SharedDir::derive_tag("/Users/me/我的 项目", 2);
        assert!(tag
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'));
    }

    #[test]
    fn smb_share_name_rejects_windows_reserved_chars() {
        // Windows 不允许共享名含 \ / : * ? " < > |，这些必须一律拒绝。
        assert!(!SharedDir::is_valid_smb_share_name("bad/name"));
        assert!(!SharedDir::is_valid_smb_share_name("bad\\name"));
        assert!(!SharedDir::is_valid_smb_share_name("bad:name"));
        assert!(!SharedDir::is_valid_smb_share_name("bad*name"));
        assert!(!SharedDir::is_valid_smb_share_name("bad?name"));
        assert!(!SharedDir::is_valid_smb_share_name("bad\"name"));
        assert!(!SharedDir::is_valid_smb_share_name("bad<name>"));
        assert!(!SharedDir::is_valid_smb_share_name("bad|name"));
        assert!(!SharedDir::is_valid_smb_share_name(""));
        // 允许的字符集：字母数字与 - _ .
        assert!(SharedDir::is_valid_smb_share_name("share-1"));
        assert!(SharedDir::is_valid_smb_share_name("My.Share_2"));
        // 长度上限 80。
        assert!(!SharedDir::is_valid_smb_share_name(&"a".repeat(81)));
        assert!(SharedDir::is_valid_smb_share_name(&"a".repeat(80)));
    }

    /// 造一台最小的合法虚拟机，供校验相关测试改字段用。
    fn base_spec() -> VmSpec {
        VmSpec {
            id: "vm-test".to_string(),
            name: "test".to_string(),
            os: GuestOs::Linux,
            arch: "x86_64".to_string(),
            cpus: 2,
            memory_mb: 2048,
            disk_path: "/tmp/disk.raw".to_string(),
            disk_size_gb: 20,
            installer_path: None,
            firmware: "bios".to_string(),
            firmware_path: None,
            driver_iso: None,
            virtiofsd_path: None,
            network: model::NetworkMode::Nat,
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
    fn bridged_network_requires_interface() {
        let mut spec = base_spec();
        spec.network = model::NetworkMode::Bridged;
        // 桥接没给网卡：必须报错，否则 QEMU 会拿到一个空 ifname。
        assert!(spec.validate().is_err());
        spec.network_iface = Some("  ".to_string());
        // 只有空白也算没给。
        assert!(spec.validate().is_err());
        spec.network_iface = Some("en0".to_string());
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn port_forward_only_allowed_on_nat() {
        let mut spec = base_spec();
        spec.port_forwards.push(model::PortForward {
            protocol: "tcp".to_string(),
            host_port: 3389,
            guest_port: 3389,
            bind_host: "127.0.0.1".to_string(),
        });
        // NAT 下合法。
        assert!(spec.validate().is_ok());
        // 桥接 / 仅主机下 hostfwd 不被 QEMU 接受，必须提前拦下。
        spec.network = model::NetworkMode::Bridged;
        spec.network_iface = Some("en0".to_string());
        assert!(spec.validate().is_err());
        spec.network = model::NetworkMode::HostOnly;
        assert!(spec.validate().is_err());
    }

    #[test]
    fn smb_protocol_is_accepted_and_name_checked() {
        let mut spec = base_spec();
        let dir = tempfile_dir();
        spec.shared_dirs.push(SharedDir {
            host_path: dir.clone(),
            mount_tag: "bad/name".to_string(),
            protocol: "smb".to_string(),
            read_only: false,
        });
        // 非法 SMB 共享名要被拦下（斜杠在 Windows 上是保留字符）。
        assert!(spec.validate().is_err());
        spec.shared_dirs[0].mount_tag = "vm-share".to_string();
        assert!(spec.validate().is_ok());
    }

    /// 造一个真实存在的临时目录（校验里会检查 host_path 是否为目录）。
    fn tempfile_dir() -> String {
        let dir = std::env::temp_dir().join("otools-vm-test-share");
        let _ = std::fs::create_dir_all(&dir);
        dir.to_string_lossy().replace('\\', "/")
    }
}
