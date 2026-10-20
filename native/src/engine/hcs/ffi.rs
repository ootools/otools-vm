//! HCS（Host Compute Service）的 FFI 绑定。
//!
//! 只绑定本引擎真正用到的那几个符号，**不引入任何 C 头文件或 .lib**：
//! 全部符号在运行时从 `computecore.dll` 取（`LoadLibraryW` + `GetProcAddress`）。
//!
//! 这么做的理由：
//! - 构建机上不需要 Windows SDK（我们的 CI 是交叉编译 + 各平台 runner）；
//! - `computecore.dll` 在 Windows 10 1809+ / Server 2019+ 恒有，晚绑定失败时
//!   能给出可读错误，而不是让整个 dylib 加载失败；
//! - HCS 的符号集随系统版本演进，晚绑定天然向前兼容。
//!
//! 所有调用都是阻塞的：HCS 的 API 是「发起 + 等结果」两段式，本模块封装成
//! 一次调用，调用方仍须放在工作线程上（引擎在 `start` 里已经是这样做的）。

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;

/// HCS 句柄都是不透明指针，具体结构由系统维护。
pub type HcsSystem = *mut c_void;
pub type HcsOperation = *mut c_void;

/// HCS 的成功码。
const S_OK: i32 = 0;

/// 单次操作等待上限。创建虚拟机通常几秒内完成，失败时也不会等太久。
const OPERATION_TIMEOUT_MS: u32 = 30_000;

/// 把 HRESULT 翻译成给用户看的话。
///
/// 只翻真正会遇到的几个——其余原样带码返回，方便用户搜。这里**不解析** HCS
/// 的结果文档（它是 JSON，但字段随版本变，靠它判断脆弱）。
pub fn describe(code: i32) -> String {
    match code as u32 {
        0x0000_0000 => "成功".to_string(),
        0x8007_0005 => "访问被拒绝（需要管理员权限）".to_string(),
        0x8037_011A => "访问被拒绝：需要管理员或 Hyper-V Administrators 组成员".to_string(),
        0x8037_011B => {
            "访问被拒绝：HCS 只允许管理员或 Hyper-V Administrators 组成员操作虚拟机".to_string()
        }
        0x8037_0102 => "缺少虚拟化组件：请开启「虚拟机平台」或 Hyper-V".to_string(),
        0x8037_0113 => "HCS 服务不可用：请开启「虚拟机平台」后重启".to_string(),
        0x8037_010C => "虚拟机配置文档无效".to_string(),
        0x8037_0105 => "虚拟机当前状态不允许该操作".to_string(),
        0x8037_010D => "找不到该虚拟机".to_string(),
        0x8037_010E => "同名虚拟机已存在".to_string(),
        0x8037_0118 => "操作超时".to_string(),
        _ => format!("HCS 操作失败（HRESULT 0x{:08X}）", code as u32),
    }
}

/// 是否属于「权限不足」这一类错误。
///
/// 前端/引擎据此把「装没装好」和「有没有权限」区分开：前者要装系统功能，
/// 后者要 UAC 提权，给用户的操作指引完全不同。
pub fn is_access_denied(code: i32) -> bool {
    matches!(code as u32, 0x8007_0005 | 0x8037_011A | 0x8037_011B)
}

/// 是否属于「本机 HCS 组件缺失」。
pub fn is_missing_component(code: i32) -> bool {
    matches!(code as u32, 0x8037_0102 | 0x8037_0113)
}

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type FnCreateOperation = unsafe extern "system" fn(*const c_void, *const c_void) -> HcsOperation;
type FnCloseOperation = unsafe extern "system" fn(HcsOperation);
type FnWaitResult = unsafe extern "system" fn(HcsOperation, u32, *mut *mut u16) -> i32;
type FnCreateSystem =
    unsafe extern "system" fn(*const u16, *const u16, HcsOperation, *const c_void, *mut HcsSystem) -> i32;
type FnSystemHandle = unsafe extern "system" fn(HcsSystem, HcsOperation, *const u16) -> i32;
type FnCloseSystem = unsafe extern "system" fn(HcsSystem);
type FnGetSystemProps = unsafe extern "system" fn(HcsSystem, HcsOperation, *const u16) -> i32;
type FnEnumerateSystems = unsafe extern "system" fn(*const u16, HcsOperation) -> i32;
/// `HcsOpenComputeSystem(id, requestedAccess, HCS_SYSTEM*)` —— 同步返回，不走操作。
///
/// 区别于其它 API 的「发起 + 等结果」两段式：它直接给句柄。这让我们能在进程
/// 重启后接管上次遗留的机器，而不是把它们留成没人管的孤儿。
type FnOpenSystem = unsafe extern "system" fn(*const u16, u32, *mut HcsSystem) -> i32;
/// `HcsModifyComputeSystem(system, operation, configuration, identity)`。
type FnModifySystem =
    unsafe extern "system" fn(HcsSystem, HcsOperation, *const u16, *const c_void) -> i32;

/// `GENERIC_ALL`：`HcsOpenComputeSystem` 的 `requestedAccess` 只接受这个值。
const GENERIC_ALL: u32 = 0x1000_0000;

/// 启动时一次性解析出来的符号表。
pub struct Api {
    create_operation: FnCreateOperation,
    close_operation: FnCloseOperation,
    wait_result: FnWaitResult,
    create_system: FnCreateSystem,
    start_system: FnSystemHandle,
    shutdown_system: FnSystemHandle,
    terminate_system: FnSystemHandle,
    pause_system: FnSystemHandle,
    resume_system: FnSystemHandle,
    close_system: FnCloseSystem,
    get_properties: FnGetSystemProps,
    enumerate_systems: FnEnumerateSystems,
    /// 可选：老系统上可能没有，缺失时「接管残留虚拟机」降级为不支持。
    open_system: Option<FnOpenSystem>,
    /// 可选：缺它时只是不能热调资源，其余功能不受影响。
    modify_system: Option<FnModifySystem>,
}

// 函数指针本身是只读的，跨线程共享安全。
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

/// 取全局符号表；首次调用时才真正打开 DLL。
///
/// 返回 `&Result<..>` 而不是 `Option`：非 Windows 平台和 DLL 缺失的失败原因
/// 都是稳定的，缓存一次避免每次探测都去 LoadLibrary。
pub fn api() -> &'static Result<Api, String> {
    API.get_or_init(load)
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

unsafe fn symbol(module: *mut c_void, name: &str) -> Option<*mut c_void> {
    let mut terminated = name.as_bytes().to_vec();
    terminated.push(0);
    let address = GetProcAddress(module, terminated.as_ptr());
    if address.is_null() {
        None
    } else {
        Some(address)
    }
}

fn load() -> Result<Api, String> {
    let library = wide("computecore.dll");
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    if module.is_null() {
        return Err("本机没有 computecore.dll：HCS 不可用".to_string());
    }

    // 少一个符号就无法工作，逐个报出缺失的名字，便于对照系统版本定位。
    macro_rules! need {
        ($name:literal) => {{
            match unsafe { symbol(module, $name) } {
                Some(pointer) => unsafe { std::mem::transmute(pointer) },
                None => return Err(format!("computecore.dll 缺少符号 {}", $name)),
            }
        }};
    }
    // 可选符号：缺失只降级对应功能，不阻断 HCS 整体可用性。
    macro_rules! maybe {
        ($name:literal) => {{
            unsafe { symbol(module, $name) }.map(|pointer| unsafe { std::mem::transmute(pointer) })
        }};
    }

    Ok(Api {
        create_operation: need!("HcsCreateOperation"),
        close_operation: need!("HcsCloseOperation"),
        wait_result: need!("HcsWaitForOperationResult"),
        create_system: need!("HcsCreateComputeSystem"),
        start_system: need!("HcsStartComputeSystem"),
        shutdown_system: need!("HcsShutDownComputeSystem"),
        terminate_system: need!("HcsTerminateComputeSystem"),
        pause_system: need!("HcsPauseComputeSystem"),
        resume_system: need!("HcsResumeComputeSystem"),
        close_system: need!("HcsCloseComputeSystem"),
        get_properties: need!("HcsGetComputeSystemProperties"),
        enumerate_systems: need!("HcsEnumerateComputeSystems"),
        open_system: maybe!("HcsOpenComputeSystem"),
        modify_system: maybe!("HcsModifyComputeSystem"),
    })
}

/// 一次 HCS 操作的作用域：负责建操作、等结果、收句柄。
///
/// 用 RAII 而不是手写 close：HCS 操作句柄泄漏会让 vmcompute 累积状态，
/// 调试验证阶段就踩过「忘了关导致后续调用行为异常」。
struct Operation<'a> {
    api: &'a Api,
    handle: HcsOperation,
}

impl<'a> Operation<'a> {
    fn new(api: &'a Api) -> Self {
        let handle = unsafe { (api.create_operation)(std::ptr::null(), std::ptr::null()) };
        Self { api, handle }
    }

    /// 等待结果。返回 (HRESULT, 结果文档)。
    ///
    /// 结果文档在失败时通常为空，成功时可能是属性 JSON——调用方按需取用。
    fn wait(&self) -> (i32, String) {
        let mut document: *mut u16 = std::ptr::null_mut();
        let code = unsafe { (self.api.wait_result)(self.handle, OPERATION_TIMEOUT_MS, &mut document) };
        (code, take_document(document))
    }
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.close_operation)(self.handle) };
    }
}

/// 读出 HCS 返回的宽字符串并释放。HCS 用 LocalAlloc 分配，必须用 LocalFree 还。
fn take_document(pointer: *mut u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    unsafe {
        let mut length = 0usize;
        // 结果文档理论上不超几 MB；加硬上限防止异常数据把内存读穿。
        while *pointer.add(length) != 0 && length < 4 * 1024 * 1024 {
            length += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
        LocalFree(pointer as *mut c_void);
        text
    }
}

/// 一台已创建的虚拟机句柄。
///
/// 句柄必须显式关闭：HCS 的 `ShouldTerminateOnLastHandleClosed` 依赖它——
/// 句柄关掉而机器还在跑时，系统会把机器一并终止。
pub struct System<'a> {
    api: &'a Api,
    handle: HcsSystem,
}

impl<'a> System<'a> {
    /// 创建虚拟机。`id` 需全机唯一，`configuration` 是 HCS JSON 文档。
    pub fn create(api: &'a Api, id: &str, configuration: &str) -> Result<Self, String> {
        let operation = Operation::new(api);
        let id_wide = wide(id);
        let config_wide = wide(configuration);
        let mut handle: HcsSystem = std::ptr::null_mut();

        let started = unsafe {
            (api.create_system)(
                id_wide.as_ptr(),
                config_wide.as_ptr(),
                operation.handle,
                std::ptr::null(),
                &mut handle,
            )
        };
        if started != S_OK {
            return Err(describe(started));
        }

        // 注意：HCS 的同步返回值只表示「操作已受理」，真正结果要 wait 才知道。
        // 探针实测过：create 返 S_OK 但 wait 返 HCS_E_ACCESS_DENIED，句柄仍是非空的。
        let (code, _document) = operation.wait();
        if code != S_OK {
            if !handle.is_null() {
                unsafe { (api.close_system)(handle) };
            }
            return Err(describe(code));
        }
        if handle.is_null() {
            return Err("HCS 未返回虚拟机句柄".to_string());
        }
        Ok(Self { api, handle })
    }

    /// 打开一台已存在的虚拟机（进程重启后接管）。
    ///
    /// 这是**同步**调用：直接返回句柄，不经过「发起 + 等结果」。
    /// `requestedAccess` 只接受 `GENERIC_ALL`，传别的一律拒绝。
    ///
    /// 用途：插件崩溃/宿主重启后，上一轮开的机器可能还在跑（尤其没配
    /// `ShouldTerminateOnLastHandleClosed` 或句柄被别的进程持有）。以前这里
    /// 返回「暂不支持接管」，结果那些机器成了界面上看不见、也停不掉的孤儿。
    pub fn open(api: &'a Api, id: &str) -> Result<Self, String> {
        let Some(function) = api.open_system else {
            return Err("当前系统的 computecore.dll 不支持接管已存在的虚拟机".to_string());
        };
        let mut handle: HcsSystem = std::ptr::null_mut();
        let code = unsafe { function(wide(id).as_ptr(), GENERIC_ALL, &mut handle) };
        if code != S_OK {
            if !handle.is_null() {
                unsafe { (api.close_system)(handle) };
            }
            return Err(describe(code));
        }
        if handle.is_null() {
            return Err("HCS 未返回虚拟机句柄".to_string());
        }
        Ok(Self { api, handle })
    }

    /// 运行中热调资源（内存 / CPU）。
    ///
    /// 文档：`ModifySettingRequest` 形如
    /// `{"ResourcePath":"VirtualMachine/ComputeTopology/Memory/SizeInMB","RequestType":"Update","Settings":2048}`。
    /// `ResourcePath` 里的 `VirtualMachine` 是固定前缀，不是机器名。
    pub fn modify(&self, resource_path: &str, settings: serde_json::Value) -> Result<(), String> {
        let Some(function) = self.api.modify_system else {
            return Err("当前系统的 computecore.dll 不支持运行中调整资源".to_string());
        };
        let document = serde_json::json!({
            "ResourcePath": resource_path,
            "RequestType": "Update",
            "Settings": settings,
        })
        .to_string();

        let operation = Operation::new(self.api);
        let code = unsafe {
            function(
                self.handle,
                operation.handle,
                wide(&document).as_ptr(),
                std::ptr::null(),
            )
        };
        if code != S_OK {
            return Err(format!("调整资源失败：{}", describe(code)));
        }
        let (result, _document) = operation.wait();
        if result == S_OK {
            Ok(())
        } else {
            Err(format!("调整资源失败：{}", describe(result)))
        }
    }

    pub fn start(&self) -> Result<(), String> {
        self.run_handle_op(self.api.start_system, "启动")
    }

    pub fn pause(&self) -> Result<(), String> {
        self.run_handle_op(self.api.pause_system, "暂停")
    }

    pub fn resume(&self) -> Result<(), String> {
        self.run_handle_op(self.api.resume_system, "继续")
    }

    /// 优雅关机：向客户机发 ACPI 关机信号。
    pub fn shutdown(&self) -> Result<(), String> {
        self.run_handle_op(self.api.shutdown_system, "关机")
    }

    /// 强制终止（等同拔电源）。
    pub fn terminate(&self) -> Result<(), String> {
        self.run_handle_op(self.api.terminate_system, "强制关闭")
    }

    /// 查询属性，返回 HCS 给的 JSON 文档。失败返回空串（调用方按「未知」处理）。
    pub fn properties(&self, query: &str) -> String {
        let operation = Operation::new(self.api);
        let query_wide = wide(query);
        let started = unsafe { (self.api.get_properties)(self.handle, operation.handle, query_wide.as_ptr()) };
        if started != S_OK {
            return String::new();
        }
        let (code, document) = operation.wait();
        if code == S_OK {
            document
        } else {
            String::new()
        }
    }

    fn run_handle_op(&self, function: FnSystemHandle, action: &str) -> Result<(), String> {
        let operation = Operation::new(self.api);
        let started = unsafe { function(self.handle, operation.handle, std::ptr::null()) };
        if started != S_OK && started != 0x8037_0116_u32 as i32 {
            // HCS_E_OPERATION_ALREADY_STARTED 不算失败，继续等结果即可。
            return Err(format!("{}失败：{}", action, describe(started)));
        }
        let (code, _document) = operation.wait();
        if code == S_OK {
            Ok(())
        } else {
            Err(format!("{}失败：{}", action, describe(code)))
        }
    }
}

impl Drop for System<'_> {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.api.close_system)(self.handle) };
        }
    }
}

/// 枚举当前进程能看到的虚拟机 id 列表。
///
/// 只用于「清理上次残留」：我们的 id 带固定前缀，可以据此识别自己的机器。
pub fn enumerate(api: &Api) -> Vec<String> {
    let operation = Operation::new(api);
    let started = unsafe { (api.enumerate_systems)(std::ptr::null(), operation.handle) };
    if started != S_OK {
        return Vec::new();
    }
    let (code, document) = operation.wait();
    if code != S_OK || document.is_empty() {
        return Vec::new();
    }
    // 文档形如 [{"Id":"xxx","SystemType":"VirtualMachine",...}]；
    // 这里不引 JSON 依赖，按 `"Id":"..."` 直接抠，够用且不依赖 schema 版本。
    let mut ids = Vec::new();
    let mut cursor = 0usize;
    let bytes = document.as_bytes();
    const NEEDLE: &[u8] = b"\"Id\":\"";
    while let Some(found) = find(&bytes[cursor..], NEEDLE) {
        let start = cursor + found + NEEDLE.len();
        let Some(end_offset) = bytes[start..].iter().position(|&byte| byte == b'"') else {
            break;
        };
        ids.push(document[start..start + end_offset].to_string());
        cursor = start + end_offset + 1;
    }
    ids
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_codes_are_recognised() {
        // 本机实测：非提权进程调 HcsCreateComputeSystem 拿到 0x8037011B。
        // 这三个码都必须被认成「权限问题」，否则前端会给出错误的引导。
        assert!(is_access_denied(0x8037_011B_u32 as i32));
        assert!(is_access_denied(0x8037_011A_u32 as i32));
        assert!(is_access_denied(0x8007_0005_u32 as i32));
        assert!(!is_access_denied(0x8037_0102_u32 as i32));
        assert!(!is_access_denied(0));
    }

    #[test]
    fn missing_component_codes_are_recognised() {
        assert!(is_missing_component(0x8037_0102_u32 as i32));
        assert!(is_missing_component(0x8037_0113_u32 as i32));
        assert!(!is_missing_component(0x8037_011B_u32 as i32));
    }

    #[test]
    fn described_messages_name_the_action() {
        // 权限错误必须说到「管理员」，否则用户不知道该做什么。
        assert!(describe(0x8037_011B_u32 as i32).contains("管理员"));
        assert!(describe(0x8037_0102_u32 as i32).contains("虚拟化"));
        // 未知码要带上原始值，方便用户搜索。
        let unknown = describe(0x1234_5678);
        assert!(unknown.contains("0x12345678"), "{}", unknown);
    }

    #[test]
    fn byte_search_finds_and_misses_correctly() {
        assert_eq!(find(b"a\"Id\":\"x\"", b"\"Id\":\""), Some(1));
        assert_eq!(find(b"nothing", b"\"Id\":\""), None);
        assert_eq!(find(b"", b"x"), None);
        assert_eq!(find(b"abc", b""), None);
    }

    #[test]
    fn enumerate_extracts_ids_from_document_shape() {
        // 直接测解析：把 HCS 的返回形态喂进来，确认能取出 id。
        let document = r#"[{"Id":"otools-vm-a","SystemType":"VirtualMachine"},{"Id":"otools-vm-b"}]"#;
        let bytes = document.as_bytes();
        let needle: &[u8] = b"\"Id\":\"";
        let mut ids = Vec::new();
        let mut cursor = 0usize;
        while let Some(found) = find(&bytes[cursor..], needle) {
            let start = cursor + found + needle.len();
            let Some(end_offset) = bytes[start..].iter().position(|&byte| byte == b'"') else {
                break;
            };
            ids.push(document[start..start + end_offset].to_string());
            cursor = start + end_offset + 1;
        }
        assert_eq!(ids, vec!["otools-vm-a", "otools-vm-b"]);
    }
}
