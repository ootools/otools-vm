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
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_virtualization::{
    VZVirtualMachine, VZVirtualMachineConfiguration, VZVirtualMachineState, VZVirtualMachineView,
};

use super::{Engine, ViewRect};
use crate::model::{EngineCaps, EngineInfo, SnapshotSupport, VmSpec, VmState};
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
    /// 挂起/恢复的进度阶段。
    ///
    /// 这两个操作天然是**异步链**：挂起要先 pause，再落盘；恢复完成后还要
    /// resume。整条链跑在 VZ 自己的回调里（主队列），命令只需发起并立即返回，
    /// 前端靠轮询 phase/error 得知进展——这样命令层从任何线程调用都不会死锁。
    phase: SnapshotPhase,
    /// 挂起/恢复失败原因。成功完成或尚未开始时为空。
    snapshot_error: Arc<Mutex<String>>,
}

/// 存档/恢复的执行阶段（见 `VzInstance::phase`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotPhase {
    Idle,
    /// 挂起中：等 pause 完成。
    SavingPausing,
    /// 挂起中：等 save 落盘。
    SavingWriting,
    /// 恢复中：等 restore 完成。
    Restoring,
    /// 恢复中：恢复完成，等 resume 回到 running。
    RestoringResuming,
}

impl SnapshotPhase {
    fn is_saving(self) -> bool {
        matches!(self, SnapshotPhase::SavingPausing | SnapshotPhase::SavingWriting)
    }
    fn is_restoring(self) -> bool {
        matches!(self, SnapshotPhase::Restoring | SnapshotPhase::RestoringResuming)
    }
    fn to_str(self) -> &'static str {
        match self {
            SnapshotPhase::Idle => "idle",
            SnapshotPhase::SavingPausing => "savingPausing",
            SnapshotPhase::SavingWriting => "savingWriting",
            SnapshotPhase::Restoring => "restoring",
            SnapshotPhase::RestoringResuming => "restoringResuming",
        }
    }
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

/// 存档文件元信息：(字节数, 修改时间 Unix 秒)。
fn snapshot_meta(spec: &VmSpec) -> (u64, i64) {
    let path = paths::vm_snapshot_path(&spec.id);
    let Ok(meta) = std::fs::metadata(&path) else {
        return (0, 0);
    };
    let bytes = meta.len();
    let at = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    (bytes, at)
}

/// 把 VZ 回调里的 `NSError*` 转成 `Result`。
fn error_result(error: *mut NSError) -> Result<(), String> {
    if error.is_null() {
        Ok(())
    } else {
        Err(unsafe { config::ns_error_text(&*error) })
    }
}

/// 记录快照阶段的失败，并把阶段复位为 Idle。
fn fail_snapshot(instance: &mut VzInstance, message: impl Into<String>) {
    let text = message.into();
    eprintln!("[vm] 快照失败: {}", text);
    if let Ok(mut slot) = instance.snapshot_error.lock() {
        *slot = text;
    }
    instance.phase = SnapshotPhase::Idle;
}

/// 清空快照错误（开始新任务时调用）。
fn clear_snapshot_error(instance: &VzInstance) {
    if let Ok(mut slot) = instance.snapshot_error.lock() {
        slot.clear();
    }
}

/// 按 id 清空快照错误（实例不存在时静默）。
fn clear_snapshot_error_for(id: &str) {
    with_instance(id, |instance| clear_snapshot_error(instance));
}

/// 从 INSTANCES 里取一台机器去驱动异步链。
///
/// 回调链在 VZ 的队列上跑，不能借用 `RefCell`（BorrowMutError）；
/// 所以每次回调都独立借一次，用完立刻还。
fn with_instance<R>(id: &str, work: impl FnOnce(&mut VzInstance) -> R) -> Option<R> {
    INSTANCES.with(|cell| {
        let mut instances = cell.borrow_mut();
        instances.get_mut(id).map(work)
    })
}

/// 取一台机器的 `VZVirtualMachine` 引用（retain 一次），**不持有 `INSTANCES` 借用**。
///
/// 异步链的每一步都要用这个：`pause`/`save`/`restore`/`resume` 都是「发起请求」
/// 后面跟一个回调。如果发起请求时还攥着 `INSTANCES` 的 `RefCell` 借用，而回调
/// 又在同一队列上同步被调用，回调里再借用就会 `RefCell already borrowed` 并
/// **abort 整个宿主进程**（实测如此）。
fn machine_of(id: &str) -> Option<Retained<VZVirtualMachine>> {
    with_instance(id, |instance| Retained::clone(&instance.machine))
}

/// 存档第一步：把机器暂停（已在 Paused 则直接进入落盘）。
///
/// 必须在主线程调用。返回后操作仍在异步进行，进度看 `phase`。
///
/// ⚠️ 关键：`INSTANCES` 是 `RefCell`，回调里**不能**在借用期间再调
/// `begin_save`/`start_save_write`（它们自己也要借用）——那会直接
/// `RefCell already borrowed` 并 abort 整个宿主进程。所以这里先在闭包里
/// 只做「发起请求」，闭包结束后再由回调独立借用做下一步。
fn begin_save(id: &str, path: &std::path::Path) -> Result<(), String> {
    let path_text = path.to_string_lossy().to_string();
    let id_for_cb = id.to_string();
    let path_for_cb = path_text.clone();

    let machine = machine_of(id).ok_or_else(|| "虚拟机未运行，无法存档".to_string())?;

    match unsafe { machine.state() } {
        VZVirtualMachineState::Paused => {
            // 已经暂停：直接进入落盘。
            start_save_write(id, &path_text)
        }
        VZVirtualMachineState::Running => {
            if !unsafe { machine.canPause() } {
                return Err("当前状态不允许暂停，无法挂起".to_string());
            }
            with_instance(id, |instance| instance.phase = SnapshotPhase::SavingPausing);

            let handler = RcBlock::new(move |error: *mut NSError| {
                if let Err(text) = error_result(error) {
                    with_instance(&id_for_cb, |instance| {
                        fail_snapshot(instance, format!("暂停失败，无法挂起：{}", text))
                    });
                    return;
                }
                // 借用早已释放，可以安全进入下一阶段。
                if let Err(text) = start_save_write(&id_for_cb, &path_for_cb) {
                    with_instance(&id_for_cb, |instance| fail_snapshot(instance, text));
                }
            });
            unsafe { machine.pauseWithCompletionHandler(&handler) };
            Ok(())
        }
        _ => Err("请先启动虚拟机再存档".to_string()),
    }
}

/// 存档第二步：把已暂停的机器写到磁盘。
///
/// 必须在主线程调用（由 pause 回调驱动）。
///
/// ⚠️ 回调里不要嵌套借用 INSTANCES（见 `begin_save` 的说明）。
fn start_save_write(id: &str, path_text: &str) -> Result<(), String> {
    let path = std::path::PathBuf::from(path_text);
    // VZ 要求目标文件不存在，先清掉旧存档。
    if path.exists() {
        std::fs::remove_file(&path).map_err(|error| format!("删除旧存档失败：{}", error))?;
    }

    let machine = machine_of(id).ok_or_else(|| "虚拟机实例已失效".to_string())?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(path_text));
    let id_for_cb = id.to_string();
    let path_for_cb = path_text.to_string();

    with_instance(id, |instance| instance.phase = SnapshotPhase::SavingWriting);

    let handler = RcBlock::new(move |error: *mut NSError| {
        if let Err(text) = error_result(error) {
            let _ = std::fs::remove_file(&path_for_cb);
            with_instance(&id_for_cb, |instance| {
                fail_snapshot(instance, format!("挂起失败：{}", text))
            });
            return;
        }
        // 成功：机器停在 Paused —— 这正是「挂起」要的效果。
        // 用户下次点「继续」恢复运行；关掉 App / 重启 Mac 后再点「继续」
        // 也能从这份存档接着用。
        with_instance(&id_for_cb, |instance| {
            instance.phase = SnapshotPhase::Idle;
            clear_snapshot_error(instance);
        });
    });
    unsafe { machine.saveMachineStateToURL_completionHandler(&url, &handler) };
    Ok(())
}

/// 恢复第一步：把存档灌回一台静止的机器。
///
/// 机器必须处于「从未启动」的状态，所以调用方要先把旧实例换成全新的
/// （同一份配置 → 同一 platform id / MAC，否则 restore 会报 invalid argument）。
/// 必须在主线程调用。
fn begin_restore(id: &str, path: &std::path::Path) -> Result<(), String> {
    let path_text = path.to_string_lossy().to_string();
    let machine = machine_of(id).ok_or_else(|| "虚拟机实例已失效".to_string())?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path_text));
    let id_for_cb = id.to_string();

    with_instance(id, |instance| instance.phase = SnapshotPhase::Restoring);

    let handler = RcBlock::new(move |error: *mut NSError| {
        if let Err(text) = error_result(error) {
            with_instance(&id_for_cb, |instance| {
                fail_snapshot(instance, format!("恢复失败：{}", text))
            });
            return;
        }
        // restore 成功后机器是 Paused，用户点「恢复」是要看到画面 → 直接续跑。
        if let Err(text) = start_restore_resume(&id_for_cb) {
            with_instance(&id_for_cb, |instance| fail_snapshot(instance, text));
        }
    });
    unsafe { machine.restoreMachineStateFromURL_completionHandler(&url, &handler) };
    Ok(())
}

/// 恢复第二步：让恢复后的机器继续运行。必须在主线程调用。
fn start_restore_resume(id: &str) -> Result<(), String> {
    let machine = machine_of(id).ok_or_else(|| "虚拟机实例已失效".to_string())?;
    if !unsafe { machine.canResume() } {
        return Err("恢复后状态异常，无法继续运行".to_string());
    }
    let id_for_cb = id.to_string();

    with_instance(id, |instance| instance.phase = SnapshotPhase::RestoringResuming);

    let handler = RcBlock::new(move |error: *mut NSError| {
        if let Err(text) = error_result(error) {
            with_instance(&id_for_cb, |instance| {
                fail_snapshot(instance, format!("恢复后继续运行失败：{}", text))
            });
            return;
        }
        with_instance(&id_for_cb, |instance| {
            instance.phase = SnapshotPhase::Idle;
            clear_snapshot_error(instance);
        });
    });
    unsafe { machine.resumeWithCompletionHandler(&handler) };
    Ok(())
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
                // 整机态存档（saveMachineStateTo / restoreMachineStateFrom）。
                // 是**按配置**判定能否存盘的：macOS 14/15 时代不含 GUI Linux 的
                // Virtio GPU，macOS 26+ 已放开（本机 macOS 27 实测 GPU+剪贴板
                // 配置 validateSaveRestoreSupport 通过）。所以这里给 true，
                // 具体每台机器能否存盘由 snapshot_support() 逐台问 VZ。
                snapshots: true,
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
            let live = INSTANCES.with(|cell| {
                let instances = cell.borrow();
                instances.get(&id).map(|instance| unsafe { instance.machine.state() })
            });
            match live {
                Some(state) => map_state(state),
                None => {
                    // 进程里没有实例，但存在挂起存档 —— 对用户来说这台机器是
                    // 「挂起中」而不是「已停止」：点「继续」就能接着用。
                    // 这个判断让 App 重启后的 UI 语义正确（否则会显示成关机）。
                    let snapshot = paths::vm_snapshot_path(&id);
                    if snapshot.exists() {
                        VmState::Paused
                    } else {
                        VmState::Stopped
                    }
                }
            }
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

    fn snapshot_support(&self, spec: &VmSpec) -> SnapshotSupport {
        let spec = spec.clone();
        on_main(move || -> SnapshotSupport {
            let (snapshot_bytes, snapshot_at) = snapshot_meta(&spec);
            let has_snapshot = snapshot_bytes > 0;

            // 阶段与错误属于「实例运行时状态」，有实例就报真的，没有就是 idle。
            let (phase, error) = INSTANCES.with(|cell| {
                let instances = cell.borrow();
                match instances.get(&spec.id) {
                    Some(instance) => (
                        instance.phase.to_str().to_string(),
                        instance
                            .snapshot_error
                            .lock()
                            .ok()
                            .map(|slot| slot.clone())
                            .unwrap_or_default(),
                    ),
                    None => ("idle".to_string(), String::new()),
                }
            });

            let nvram = paths::vm_nvram_path(&spec.id);
            let nvram_text = nvram.to_string_lossy().to_string();
            // 只是「问 Apple 这台配置支不支持存盘」，不需要真起机。
            // 注意：build() 会创建 NVRAM 与身份文件，所以必须先确保目录存在。
            let _ = paths::ensure_dir(&paths::vm_dir(&spec.id));
            match config::build(&spec, &nvram_text) {
                Ok(built) => match unsafe { built.config.validateSaveRestoreSupportWithError() } {
                    Ok(()) => SnapshotSupport {
                        supported: true,
                        reason: String::new(),
                        has_snapshot,
                        snapshot_bytes,
                        snapshot_at,
                        phase,
                        error,
                    },
                    Err(err) => SnapshotSupport {
                        supported: false,
                        reason: config::ns_error_text(&err),
                        has_snapshot,
                        snapshot_bytes,
                        snapshot_at,
                        phase,
                        error,
                    },
                },
                Err(err) => SnapshotSupport {
                    supported: false,
                    reason: err,
                    has_snapshot,
                    snapshot_bytes,
                    snapshot_at,
                    phase,
                    error,
                },
            }
        })
    }

    fn save_state(&self, spec: &VmSpec) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            let path = paths::vm_snapshot_path(&spec.id);
            if let Some(parent) = path.parent() {
                paths::ensure_dir(parent)?;
            }
            // 已有任务在跑就别重复触发：VZ 会对同一台机器重入报错，
            // 而且用户看到两个进度会很困惑。
            if let Some(instance) = with_instance(&spec.id, |instance| instance.phase) {
                if instance.is_saving() {
                    return Err("正在存档，请稍候".to_string());
                }
                if instance.is_restoring() {
                    return Err("正在恢复，请稍候".to_string());
                }
            }
            clear_snapshot_error_for(&spec.id);
            begin_save(&spec.id, &path)
        })
    }

    fn restore_state(&self, spec: &VmSpec) -> Result<(), String> {
        let spec = spec.clone();
        on_main(move || -> Result<(), String> {
            let path = paths::vm_snapshot_path(&spec.id);
            if !path.exists() {
                return Err("没有可恢复的存档".to_string());
            }
            if let Some(phase) = with_instance(&spec.id, |instance| instance.phase) {
                if phase.is_saving() {
                    return Err("正在存档，请稍候".to_string());
                }
                if phase.is_restoring() {
                    return Err("正在恢复，请稍候".to_string());
                }
            }

            // restore 要求机器处于「从未启动」的状态：VZ 只能把存档灌进一台
            // 尚未 start 的机器。所以先丢掉现存的实例，再用**同一份配置**
            // （同一 platform id / MAC）重建一台全新的。
            INSTANCES.with(|cell| {
                let mut instances = cell.borrow_mut();
                if let Some(mut old) = instances.remove(&spec.id) {
                    if let Some(created) = old.view.take() {
                        view::detach(&created);
                    }
                    if let Some(window) = old.window.take() {
                        window.close();
                    }
                }
            });
            ensure_instance(&spec, false)?;
            clear_snapshot_error_for(&spec.id);
            begin_restore(&spec.id, &path)
        })
    }

    fn snapshot_phase(&self, spec: &VmSpec) -> String {
        let id = spec.id.clone();
        on_main(move || -> String {
            INSTANCES.with(|cell| {
                let instances = cell.borrow();
                match instances.get(&id) {
                    Some(instance) => instance.phase.to_str().to_string(),
                    None => "idle".to_string(),
                }
            })
        })
    }

    fn snapshot_error(&self, spec: &VmSpec) -> String {
        let id = spec.id.clone();
        on_main(move || -> String {
            INSTANCES.with(|cell| {
                let instances = cell.borrow();
                instances
                    .get(&id)
                    .and_then(|instance| {
                        instance.snapshot_error.lock().ok().map(|slot| slot.clone())
                    })
                    .unwrap_or_default()
            })
        })
    }

    fn discard_state(&self, spec: &VmSpec) -> Result<(), String> {
        let path = paths::vm_snapshot_path(&spec.id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| format!("删除存档失败：{}", error))?;
        }
        Ok(())
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
                phase: SnapshotPhase::Idle,
                snapshot_error: Arc::new(Mutex::new(String::new())),
            },
        );
        Ok(())
    })
}

