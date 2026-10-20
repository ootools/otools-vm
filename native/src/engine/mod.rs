//! 引擎抽象层。
//!
//! 设计目标：**同一份 UI 与命令层跑在两种后端上**。
//! - macOS / Apple 芯片 → Apple Virtualization.framework（原生视图、GPU 加速、virtiofs）
//! - 其它平台 → QEMU 子进程（HVF / WHPX / KVM 加速）
//!
//! 引擎之间只共享 `VmSpec` 与 `VmState`，不共享实现细节。

use crate::model::{EngineCaps, EngineInfo, SnapshotSupport, VmSpec, VmState};
use serde::Serialize;

pub mod qemu;
#[cfg(target_os = "macos")]
pub mod vz;

/// 客户机视图在宿主窗口里的位置（CSS 像素，原点左上）。
#[derive(Debug, Clone, Copy, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// 页面 devicePixelRatio，用于换算 backing store。
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// 页面视口尺寸（window.innerWidth/Height）。宿主顶部有原生标题栏覆盖区时，
    /// web 视口高度会小于 NSView 高度，须用视口高做翻转基准（否则原生视图会上移
    /// 这段标题栏 inset、盖住顶部工具栏）。
    #[serde(default)]
    pub viewport_width: f64,
    #[serde(default)]
    pub viewport_height: f64,
}

fn default_scale() -> f64 {
    1.0
}

impl ViewRect {
    pub fn is_valid(&self) -> bool {
        self.width > 8.0 && self.height > 8.0
    }
}

/// 引擎统一接口。实现必须是 `Send + Sync`：命令层可能从任意工作线程调用。
pub trait Engine: Send + Sync {
    fn info(&self) -> EngineInfo;

    /// 该规格能否由本引擎承担（架构不匹配时回落）。
    fn supports(&self, spec: &VmSpec) -> bool;

    fn start(&self, spec: &VmSpec) -> Result<(), String>;
    fn stop(&self, spec: &VmSpec) -> Result<(), String>;
    fn pause(&self, spec: &VmSpec) -> Result<(), String>;
    fn resume(&self, spec: &VmSpec) -> Result<(), String>;
    fn state(&self, spec: &VmSpec) -> VmState;
    fn last_error(&self, spec: &VmSpec) -> String;

    /// 存档式快照（把整机 CPU+RAM 状态落到磁盘）。默认不支持。
    ///
    /// 语义（VZ 原生存盘）：
    /// - `save_state`：要求机器处于 Paused；写出存档文件后机器状态不变（仍 Paused）。
    /// - `restore_state`：要求机器处于 Stopped 且从未启动；成功后机器进入 Paused，
    ///   实现会继续 resume 回 Running。
    ///
    /// 这两者是**异步链**（save = pause→落盘；restore = load→resume），命令只发起
    /// 并立即返回，进度与结果由 `snapshot_phase` / `snapshot_error` 汇报。
    fn snapshot_support(&self, _spec: &VmSpec) -> SnapshotSupport {
        SnapshotSupport::unsupported()
    }
    fn save_state(&self, _spec: &VmSpec) -> Result<(), String> {
        Err("当前引擎不支持存档式快照".to_string())
    }
    fn restore_state(&self, _spec: &VmSpec) -> Result<(), String> {
        Err("当前引擎不支持存档式快照".to_string())
    }
    /// 快照任务当前阶段：`idle` / `savingPausing` / `savingWriting` /
    /// `restoring` / `restoringResuming`。前端据此显示进度并判断何时该刷新。
    fn snapshot_phase(&self, _spec: &VmSpec) -> String {
        "idle".to_string()
    }
    /// 最近一次快照失败原因（成功或未开始时为空）。
    fn snapshot_error(&self, _spec: &VmSpec) -> String {
        String::new()
    }
    /// 删除已存下的快照文件（连同「有存档」的状态标志）。
    fn discard_state(&self, _spec: &VmSpec) -> Result<(), String> {
        Ok(())
    }

    /// 把客户机画面挂到宿主窗口的指定矩形。
    fn attach_view(&self, spec: &VmSpec, rect: ViewRect) -> Result<(), String>;
    fn update_view_rect(&self, spec: &VmSpec, rect: ViewRect) -> Result<(), String>;
    fn detach_view(&self, spec: &VmSpec) -> Result<(), String>;

    /// 进程退出前清理：停掉所有虚拟机与子进程。
    fn shutdown(&self);
}

/// 能力为空、不可用的引擎，用于「宿主不支持」的降级展示。
pub struct NoopEngine {
    reason: String,
}

impl NoopEngine {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Engine for NoopEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            kind: "none".to_string(),
            available: false,
            label: "不可用".to_string(),
            reason: self.reason.clone(),
            version: String::new(),
            host_entitled: false,
            accelerator: None,
            caps: EngineCaps::unsupported(),
        }
    }
    fn supports(&self, _spec: &VmSpec) -> bool {
        false
    }
    fn start(&self, _spec: &VmSpec) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn stop(&self, _spec: &VmSpec) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn pause(&self, _spec: &VmSpec) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn resume(&self, _spec: &VmSpec) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn state(&self, _spec: &VmSpec) -> VmState {
        VmState::Unavailable
    }
    fn last_error(&self, _spec: &VmSpec) -> String {
        self.reason.clone()
    }
    fn attach_view(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn update_view_rect(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn detach_view(&self, _spec: &VmSpec) -> Result<(), String> {
        Ok(())
    }
    fn shutdown(&self) {}
}

/// 选定并构造宿主可用的引擎。只构造一次，进程内复用。
pub fn build_engine() -> Box<dyn Engine> {
    #[cfg(target_os = "macos")]
    {
        match vz::VzEngine::create() {
            Ok(engine) => return Box::new(engine),
            Err(reason) => {
                eprintln!("[vm] Virtualization.framework 不可用: {}，回落到 QEMU", reason);
                return Box::new(qemu::QemuEngine::create_or_noop(reason));
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(qemu::QemuEngine::create_or_noop(String::new()))
    }
}
