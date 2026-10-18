//! Apple Virtualization.framework 后端。
//!
//! 线程模型：VZ / AppKit 对象都不是 `Send`，且绝大多数调用必须在主线程。
//! 因此所有实例状态放在**主线程 thread_local** 里，命令层统一经 [`on_main`]
//! 调度过去。命令层自己可能跑在任意工作线程（宿主的 native 插件桥就是）。

mod config;
pub mod dialog;
mod view;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::NSWindow;
use objc2_foundation::NSError;
use objc2_virtualization::{
    VZVirtualMachine, VZVirtualMachineConfiguration, VZVirtualMachineState, VZVirtualMachineView,
};

use super::{Engine, ViewRect};
use crate::model::{EngineCaps, EngineInfo, VmSpec, VmState};
use crate::paths;

/// 一台在宿主进程内活着的虚拟机。
struct VzInstance {
    machine: Retained<VZVirtualMachine>,
    /// 构造期用到的中间设备对象，必须活到 machine 销毁（见 config::BuiltConfig）。
    _retained: Vec<Retained<objc2::runtime::AnyObject>>,
    /// 原始配置对象也要留住：VZ 在 start 之后还会经
    /// `_synchronousConfigurationForVirtualMachine` 再读一遍配置里的附件，
    /// 那时若配置已被释放，`VZSpiceAgentPortAttachment` 就是野指针。
    _config: Retained<VZVirtualMachineConfiguration>,
    view: Option<Retained<VZVirtualMachineView>>,
    /// 独立窗口模式下的窗口（内嵌模式为 None）。
    window: Option<Retained<NSWindow>>,
    error: Arc<Mutex<String>>,
    /// 构建该实例用的规格。用于判断「是否需要重建」：VZ 配置在 init 之后不可变，
    /// 改完设置重启时必须重建；但 start 与 attach_view 互斥重建会互相拆台，
    /// 所以改为「配置没变就复用」，只在规格确实变化时才销毁旧实例。
    spec: VmSpec,
}

thread_local! {
    static INSTANCES: RefCell<HashMap<String, VzInstance>> = RefCell::new(HashMap::new());
}

/// 把闭包送到主线程执行并取回结果。
///
/// 已经在主线程时直接调用 —— `dispatch_sync` 到当前队列会自死锁。
pub(crate) fn on_main<R, F>(work: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    if MainThreadMarker::new().is_some() {
        return work();
    }
    let (tx, rx) = mpsc::channel();
    DispatchQueue::main().exec_sync(move || {
        let _ = tx.send(work());
    });
    match rx.recv() {
        Ok(value) => value,
        Err(_) => panic!("主线程任务未能返回结果"),
    }
}

fn map_state(state: VZVirtualMachineState) -> VmState {
    match state {
        VZVirtualMachineState::Running => VmState::Running,
        VZVirtualMachineState::Starting => VmState::Starting,
        VZVirtualMachineState::Paused => VmState::Paused,
        VZVirtualMachineState::Pausing => VmState::Pausing,
        VZVirtualMachineState::Resuming => VmState::Resuming,
        VZVirtualMachineState::Stopping => VmState::Stopping,
        VZVirtualMachineState::Error => VmState::Error,
        _ => VmState::Stopped,
    }
}

pub struct VzEngine {
    version: String,
}

impl VzEngine {
    pub fn create() -> Result<Self, String> {
        if !cfg!(target_arch = "aarch64") {
            return Err("Virtualization.framework 仅支持 Apple 芯片（arm64）宿主".to_string());
        }
        let supported = unsafe { VZVirtualMachine::isSupported() };
        if !supported {
            return Err("当前系统未启用 Virtualization.framework".to_string());
        }
        Ok(Self {
            version: macos_version(),
        })
    }
}

/// 探测宿主进程是否具备 virtualization 权限。
///
/// 手法：拿一个**空配置**去 `validateWithError`。
/// - 缺权限 → 报 `com.apple.security.virtualization`；
/// - 有权限 → 报 `bootLoader is nil`（正常的空配置报错）。
/// 两者错误文本可区分，所以不需要真的建一台虚拟机就能判断。
///
/// 代价很低（建一个空配置对象即丢弃），但必须走主线程 —— VZ 对象不是 Send。
fn probe_host_entitlement() -> bool {
    on_main(|| {
        let config = unsafe { VZVirtualMachineConfiguration::new() };
        match unsafe { config.validateWithError() } {
            Ok(()) => true,
            Err(error) => !config::ns_error_text(&error)
                .contains("com.apple.security.virtualization"),
        }
    })
}

fn macos_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

impl Engine for VzEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            kind: "vz".to_string(),
            available: true,
            label: "Apple Virtualization".to_string(),
            reason: String::new(),
            version: self.version.clone(),
            host_entitled: probe_host_entitlement(),
            caps: EngineCaps {
                native_view: true,
                shared_dirs: true,
                // virtiofs 在 Linux 客户机开箱可用；Windows ARM 需要额外驱动。
                shared_dirs_guests: vec!["linux".to_string()],
                clipboard: true,
                auto_resize: true,
                audio: true,
                snapshots: false,
                clone: true,
                resize_disk: true,
                rosetta: true,
                bridged_network: false,
            },
        }
    }

    fn supports(&self, spec: &VmSpec) -> bool {
        spec.arch == "arm64"
    }

    fn start(&self, spec: &VmSpec) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            // 复用或重建一个「可启动」实例：规格没变就复用；若机器已停掉
            // （VZ 机器只能启动一次）则销毁重建，否则 start 会因 canStart 失败而报错。
            // attach_view 挂上去的视图在规格没变时也会被完整保留。
            ensure_instance(&spec, true)?;
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                let instance = instances
                    .get_mut(&spec.id)
                    .ok_or_else(|| "虚拟机实例初始化失败".to_string())?;

                let state = unsafe { instance.machine.state() };
                if state != VZVirtualMachineState::Stopped && state != VZVirtualMachineState::Error {
                    return Err("虚拟机已在运行".to_string());
                }
                if !unsafe { instance.machine.canStart() } {
                    return Err("当前状态不允许启动".to_string());
                }

                instance.error.lock().map(|mut slot| slot.clear()).ok();
                let error_slot = Arc::clone(&instance.error);
                let handler = RcBlock::new(move |error: *mut NSError| {
                    if error.is_null() {
                        return;
                    }
                    let text = unsafe { config::ns_error_text(&*error) };
                    eprintln!("[vm] 启动失败: {}", text);
                    if let Ok(mut slot) = error_slot.lock() {
                        *slot = text;
                    }
                });
                unsafe { instance.machine.startWithCompletionHandler(&handler) };
                Ok(())
            })
        })
    }

    fn stop(&self, spec: &VmSpec) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                let instance = match instances.get_mut(&spec.id) {
                    Some(instance) => instance,
                    None => return Ok(()),
                };
                // 先请客户机自己关机（ACPI），失败再硬停。
                if unsafe { instance.machine.canRequestStop() } {
                    match unsafe { instance.machine.requestStopWithError() } {
                        Ok(()) => return Ok(()),
                        Err(error) => {
                            eprintln!("[vm] ACPI 关机请求失败: {}", config::ns_error_text(&error));
                        }
                    }
                }
                if unsafe { instance.machine.canStop() } {
                    let handler = RcBlock::new(|_error: *mut NSError| {});
                    unsafe { instance.machine.stopWithCompletionHandler(&handler) };
                }
                Ok(())
            })
        })
    }

    fn pause(&self, spec: &VmSpec) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                let instance = instances
                    .get_mut(&spec.id)
                    .ok_or_else(|| "虚拟机未运行".to_string())?;
                if !unsafe { instance.machine.canPause() } {
                    return Err("当前状态不允许暂停".to_string());
                }
                let handler = RcBlock::new(|_error: *mut NSError| {});
                unsafe { instance.machine.pauseWithCompletionHandler(&handler) };
                Ok(())
            })
        })
    }

    fn resume(&self, spec: &VmSpec) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                let instance = instances
                    .get_mut(&spec.id)
                    .ok_or_else(|| "虚拟机未运行".to_string())?;
                if !unsafe { instance.machine.canResume() } {
                    return Err("当前状态不允许继续".to_string());
                }
                let handler = RcBlock::new(|_error: *mut NSError| {});
                unsafe { instance.machine.resumeWithCompletionHandler(&handler) };
                Ok(())
            })
        })
    }

    fn state(&self, spec: &VmSpec) -> VmState {
        let id = spec.id.clone();
        on_main(move || -> VmState {
            INSTANCES.with(|cell| {
                let instances = cell.borrow();
                match instances.get(&id) {
                    Some(instance) => map_state(unsafe { instance.machine.state() }),
                    None => VmState::Stopped,
                }
            })
        })
    }

    fn last_error(&self, spec: &VmSpec) -> String {
        let id = spec.id.clone();
        on_main(move || -> String {
            INSTANCES.with(|cell| {
                let instances = cell.borrow();
                instances
                    .get(&id)
                    .and_then(|instance| instance.error.lock().ok().map(|slot| slot.clone()))
                    .unwrap_or_default()
            })
        })
    }

    fn attach_view(&self, spec: &VmSpec, rect: ViewRect) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            // 复用已有实例（同一规格），不要销毁正在运行的机器或已挂好的视图。
            // attach_view 路径传 rebuild_dead=false：绝不破坏已经运行/挂好的实例。
            ensure_instance(&spec, false)?;
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                let instance = instances
                    .get_mut(&spec.id)
                    .ok_or_else(|| "虚拟机实例初始化失败".to_string())?;
                if instance.view.is_some() {
                    // 已挂载：只更新矩形。
                    return resize_view(instance, rect);
                }
                match view::attach_embedded(&instance.machine, rect) {
                    Ok(created) => {
                        view::focus(&created);
                        instance.view = Some(created);
                        Ok(())
                    }
                    Err(embedded_error) => {
                        // 宿主窗口拿不到时退化成独立窗口，至少让用户能看到画面。
                        eprintln!("[vm] 内嵌失败（{}），改用独立窗口", embedded_error);
                        let (created, window) = view::attach_detached(&instance.machine, rect)?;
                        instance.view = Some(created);
                        instance.window = Some(window);
                        Ok(())
                    }
                }
            })
        })
    }

    fn update_view_rect(&self, spec: &VmSpec, rect: ViewRect) -> Result<(), String> {
        let id = spec.id.clone();
        on_main(move || -> Result<(), String> {
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                match instances.get_mut(&id) {
                    Some(instance) => resize_view(instance, rect),
                    None => Ok(()),
                }
            })
        })
    }

    fn detach_view(&self, spec: &VmSpec) -> Result<(), String> {
        let id = spec.id.clone();
        on_main(move || -> Result<(), String> {
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                if let Some(instance) = instances.get_mut(&id) {
                    if let Some(created) = instance.view.take() {
                        view::detach(&created);
                    }
                    if let Some(window) = instance.window.take() {
                        window.close();
                    }
                }
                Ok(())
            })
        })
    }

    fn shutdown(&self) {
        on_main(|| {
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                for (_, instance) in instances.iter_mut() {
                    if unsafe { instance.machine.canRequestStop() } {
                        let _ = unsafe { instance.machine.requestStopWithError() };
                    }
                    if let Some(created) = instance.view.take() {
                        view::detach(&created);
                    }
                }
                instances.clear();
            });
        });
    }
}

fn resize_view(instance: &mut VzInstance, rect: ViewRect) -> Result<(), String> {
    let created = match instance.view.as_ref() {
        Some(created) => created,
        None => return Ok(()),
    };
    if !rect.is_valid() {
        return Ok(());
    }
    // 独立窗口模式下 view 就是 contentView，跟随窗口即可，不需要手动改 frame。
    if instance.window.is_some() {
        return Ok(());
    }
    // 内嵌模式：按前端矩形重新定位。坐标换算（含与宿主 chrome 的对齐）在 view 层做。
    view::resize(created, rect);
    Ok(())
}

/// 按需创建 machine（不启动）。视图可以在启动前就挂上去，用户先看到「等待启动」。
///
/// 复用策略由 `rebuild_dead` 控制，因为 Apple Virtualization.framework 的虚拟机
/// 对象**只能启动一次**：停掉之后必须销毁重建，否则 `canStart` 永远返回 false。
///
/// - `rebuild_dead = false`（attach_view 路径）：规格没变就直接复用，绝不破坏
///   正在运行的机器或已经挂好的视图——这正是修复「窗口一闪而过」的关键：
///   start 建好运行中的机器后，状态变 Running 触发前端 attach，如果这里再 remove
///   重建一台停止的机器，运行中的实例就被拆掉，poll 看到 Stopped 又会 detach，窗口闪退。
/// - `rebuild_dead = true`（start 路径）：规格没变时——
///     * 运行中/启动中 → 复用（随后 start 自带「已在运行」拦截，不会误删）；
///     * 从未启动的停止机器（等待启动期建立的）→ 复用并启动；
///     * 已停掉的死机器（stop→restart）→ 销毁重建，否则无法再次启动。
///   规格变了（停机改设置后重启）→ 总是销毁重建。
fn ensure_instance(spec: &VmSpec, rebuild_dead: bool) -> Result<(), String> {
    INSTANCES.with(|cell| {
        let mut instances = cell.borrow_mut();
        if let Some(existing) = instances.get(&spec.id) {
            if existing.spec == *spec {
                if !rebuild_dead {
                    // attach_view：保留运行中的机器与已挂视图。
                    return Ok(());
                }
                let state = unsafe { existing.machine.state() };
                if state == VZVirtualMachineState::Running
                    || state == VZVirtualMachineState::Starting
                {
                    // 运行中/启动中：保留，交给 start 的「已在运行」检查拦截。
                    return Ok(());
                }
                if unsafe { existing.machine.canStart() } {
                    // 从未启动过的停止机器（等待启动期建立的）：直接复用再启动。
                    return Ok(());
                }
                // 落到下面：已停掉的死机器，必须重建。
            }
        }
        // 首次、规格变化、或 start 遇上死机器：丢掉旧实例重建。
        instances.remove(&spec.id);
        let nvram = paths::vm_nvram_path(&spec.id);
        let nvram_text = nvram.to_string_lossy().to_string();
        let built = config::build(spec, &nvram_text)?;
        let machine = unsafe {
            VZVirtualMachine::initWithConfiguration_queue(
                VZVirtualMachine::alloc(),
                &built.config,
                DispatchQueue::main(),
            )
        };
        instances.insert(
            spec.id.clone(),
            VzInstance {
                machine,
                _retained: built.retained,
                _config: built.config,
                view: None,
                window: None,
                error: Arc::new(Mutex::new(String::new())),
                spec: spec.clone(),
            },
        );
        Ok(())
    })
}

