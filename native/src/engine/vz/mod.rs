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
use objc2_app_kit::{NSView, NSWindow};
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
            // 每次启动都重建 machine：CPU / 内存 / 共享目录改动写在 VmSpec 里，
            // 而 VZ 的配置在 init 之后不可变。缓存旧实例会让「改完设置重启」失效。
            INSTANCES.with(|cell| {
                cell.borrow_mut().remove(&spec.id);
            });
            ensure_instance(&spec)?;
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
            // 每次启动都重建 machine：CPU / 内存 / 共享目录改动写在 VmSpec 里，
            // 而 VZ 的配置在 init 之后不可变。缓存旧实例会让「改完设置重启」失效。
            INSTANCES.with(|cell| {
                cell.borrow_mut().remove(&spec.id);
            });
            ensure_instance(&spec)?;
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
    let parent: Option<Retained<NSView>> = unsafe { created.superview() };
    match parent {
        Some(parent) => {
            view::resize(created, &parent, rect);
            Ok(())
        }
        None => Ok(()),
    }
}

/// 按需创建 machine（不启动）。视图可以在启动前就挂上去，用户先看到「等待启动」。
fn ensure_instance(spec: &VmSpec) -> Result<(), String> {
    INSTANCES.with(|cell| {
        let mut instances = cell.borrow_mut();
        if instances.contains_key(&spec.id) {
            return Ok(());
        }
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
            },
        );
        Ok(())
    })
}

