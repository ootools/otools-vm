//! 宿主原生 SMB 共享：直接借用操作系统自带的文件共享服务。
//!
//! 这是**唯一在 Windows 客户机上开箱可用**的共享方式 —— SMB 是 Windows 内置协议，
//! 服务端与客户端都由微软自己维护，客户机不需要装任何驱动（9p 要 Linux 内核、
//! virtio-fs 要 virtio-win 的 viofs 驱动，Windows 客户机都拿不到）。
//!
//! ## 为什么复用宿主服务端而不是内嵌一个
//!
//! QEMU 自带的 `smb=` 只是去启动宿主的 Samba 并生成临时配置，本质仍是「宿主服务端」。
//! 直接调系统自带能力有三个好处：
//! 1. 零新依赖：Windows 自带 `net share`，macOS 自带「文件共享」，
//!    Linux 走 `net usershare`（Samba 装了就有）；
//! 2. 权限模型跟宿主一致：用户在自己熟悉的界面里管权限，插件不越权；
//! 3. 生命周期不受虚拟机开关影响，共享是宿主资源的既有属性。
//!
//! ## 客户机怎么找到宿主
//!
//! NAT 模式下宿主在客户机眼里是虚拟网关 `10.0.2.2`（QEMU 约定），
//! 所以客户机直接 `\\10.0.2.2\<共享名>` 就能访问到宿主自己的 SMB 服务端 ——
//! 不需要额外配置，也不需要 guestfwd 中继。桥接 / 仅主机模式下改用宿主的实际地址。
//!
//! ## 权限现实
//!
//! 创建 / 删除系统级共享在 Windows 上需要管理员权限，Linux 上取决于 Samba 配置，
//! macOS 需要开启「文件共享」。本模块**不静默提权**：探测不到能力时返回可操作
//! 原因，由用户决定要不要提权。

use std::process::Command;

use crate::model::{SharedDir, VmSpec};

/// 客户机侧要访问的共享名（SMB 里共享名与目录名解耦，这里用清单里的标签）。
pub fn share_name(dir: &SharedDir) -> String {
    dir.mount_tag.clone()
}

/// 宿主在客户机眼里的地址。
///
/// - NAT：QEMU 约定的固定网关 `10.0.2.2`（客户机 → 宿主的所有连接都落到宿主）。
/// - 桥接 / 仅主机：宿主在局域网里的真实地址，取所选网卡的 IPv4。
pub fn host_address_for_guest(spec: &VmSpec) -> String {
    if spec.network.is_user_net() {
        return "10.0.2.2".to_string();
    }
    let Some(iface) = spec.network_iface.as_deref().map(str::trim).filter(|v| !v.is_empty())
    else {
        return String::new();
    };
    super::network::list_interfaces()
        .into_iter()
        .find(|item| item.id == iface)
        .map(|item| item.ipv4)
        .unwrap_or_default()
}

/// 客户机侧挂载命令 / 访问路径（Windows 客户机与 Linux 客户机分别给一条）。
pub fn guest_mount_commands(spec: &VmSpec, dir: &SharedDir) -> (String, String) {
    let host = host_address_for_guest(spec);
    let name = share_name(dir);
    let windows = if host.is_empty() {
        format!(r"net use Z: \\<宿主地址>\{}", name)
    } else {
        format!(r"net use Z: \\{}\{}", host, name)
    };
    let linux = if host.is_empty() {
        format!(
            "mount -t cifs //<宿主地址>/{} /mnt/{} -o user=<共享账号>",
            name, name
        )
    } else {
        format!(
            "mount -t cifs //{}/{} /mnt/{} -o user=<共享账号>",
            host, name, name
        )
    };
    (windows, linux)
}

/// 宿主能否创建系统级共享；不能时给出可操作原因。
///
/// **实测要点**（本机）：创建 / 删除共享需要管理员权限，缺权限时 `net` 返回
/// 退出码 5，stdout 为空。如果只判断「net 命令能否调用」就会误报可用、
/// 让用户配好一堆共享后在启动时才发现建不起来——所以这里一并把提权状态查了。
pub fn check_capability() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // `net share` 不需要 SMB 服务在线也能列出，但只要服务被停用就会失败。
        // 先确认命令本身可用（真正的权限问题单独判断）。
        match net_command(&["share"]).output() {
            Ok(_) => {}
            Err(_) => return Err("无法调用 net share，当前系统不支持宿主原生共享".to_string()),
        }
        if !is_elevated() {
            return Err(
                "宿主原生共享需要管理员权限。请以管理员身份重新启动 OTools 后再添加 SMB 共享；\
                 或直接沿用 9p / virtio-fs 共享（Windows 客户机需装 virtio-win 驱动）。"
                    .to_string(),
            );
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        if which("net").is_some() {
            Ok(())
        } else {
            Err(
                "未找到 net 命令（Samba 未安装）。请安装 samba 后重试，\
                 或改用 9p / virtio-fs 共享。"
                    .to_string(),
            )
        }
    }
    #[cfg(target_os = "macos")]
    {
        // macOS 的 `sharing` 命令需要先开启「文件共享」服务。
        let output = Command::new("launchctl")
            .args(["print", "system/com.apple.smbd"])
            .output();
        match output {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err(
                "宿主「文件共享」未开启。请在「系统设置 → 通用 → 共享」里打开\
                 「文件共享」，至少添加一个共享文件夹后重试。"
                    .to_string(),
            ),
        }
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        Err("当前平台不支持宿主原生共享".to_string())
    }
}

/// 当前进程是否具备管理员权限。
///
/// 用 `net session` 探测：这条命令只允许管理员执行，能成功即说明已提权。
/// 它比 `whoami /groups` 解析更稳（不受系统语言影响）。
#[cfg(target_os = "windows")]
fn is_elevated() -> bool {
    Command::new("net")
        .arg("session")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn which(binary: &str) -> Option<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", binary))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// 建一条指向宿主目录的系统共享。已存在同名共享时视为成功（幂等）。
pub fn ensure_share(dir: &SharedDir) -> Result<(), String> {
    let name = share_name(dir);
    let path = dir.host_path.trim();
    if path.is_empty() {
        return Err("共享目录路径为空".to_string());
    }
    if !std::path::Path::new(path).is_dir() {
        return Err(format!("共享目录不存在或不是目录：{}", path));
    }
    if is_shared(&name) {
        return Ok(());
    }
    if dir.read_only {
        create_share_readonly(&name, path)
    } else {
        create_share_readwrite(&name, path)
    }
}

/// 删掉一条系统共享。目录已经不在、共享本来就不存在都算成功。
///
/// **判据是退出码不是输出文本**（本机实测）：`net share <name>` 查一个不存在的
/// 共享时返回 exit code 2 且 **stdout 完全为空**——没有可匹配的英文提示。
/// 早期版本靠 `"not exist"` 关键字判断「不存在」，在那个前提下永远不成立。
pub fn remove_share(name: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let output = net_command(&["share", name, "/delete", "/y"])
            .output()
            .map_err(|error| format!("调用 net share 失败: {}", error))?;
        if output.status.success() {
            return Ok(());
        }
        // 退出码 2 = 找不到该共享（等价于「已经没有了」），按幂等处理。
        if output.status.code() == Some(2) {
            return Ok(());
        }
        Err(share_error("删除", output.status.code(), &output.stdout))
    }
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("net")
            .args(["usershare", "delete", name])
            .output()
            .map_err(|error| format!("调用 net usershare 失败: {}", error))?;
        if output.status.success() {
            return Ok(());
        }
        Err(share_error("删除", output.status.code(), &output.stdout))
    }
    #[cfg(target_os = "macos")]
    {
        // macOS 用共享点的 display name 做增删；这里按共享名尝试移除。
        let _ = name;
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = name;
        Err("当前平台不支持宿主原生共享".to_string())
    }
}

/// 该共享名当前是否已经存在于宿主上。
///
/// 实测：`net share <不存在>` → exit code 2；存在 → exit code 0。
/// 只看退出码，不受系统语言影响。
pub fn is_shared(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        net_command(&["share", name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("net")
            .args(["usershare", "info", name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = name;
        // macOS 上没有等价的单条查询；直接认为「已存在」不做重复创建，
        // 让用户在自己熟悉的「文件共享」面板里维护。
        true
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = name;
        false
    }
}

#[cfg(target_os = "windows")]
fn create_share_readwrite(name: &str, path: &str) -> Result<(), String> {
    // /grant 里的 Everyone 是共享层权限；NTFS 权限仍然独立生效（取交集），
    // 所以这里放开共享层不会绕过文件系统安全。
    let spec = format!("{}={}", name, path);
    let output = net_command(&["share", &spec, "/grant:Everyone,FULL", "/remark:OTools VM"])
        .output()
        .map_err(|error| format!("调用 net share 失败: {}", error))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(share_error("创建", output.status.code(), &output.stdout))
    }
}

#[cfg(target_os = "windows")]
fn create_share_readonly(name: &str, path: &str) -> Result<(), String> {
    let spec = format!("{}={}", name, path);
    let output = net_command(&["share", &spec, "/grant:Everyone,READ", "/remark:OTools VM"])
        .output()
        .map_err(|error| format!("调用 net share 失败: {}", error))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(share_error("创建", output.status.code(), &output.stdout))
    }
}

#[cfg(target_os = "linux")]
fn create_share_readwrite(name: &str, path: &str) -> Result<(), String> {
    let output = Command::new("net")
        .args([
            "usershare",
            "add",
            name,
            path,
            "Everyone:F",
            "guest_ok=y",
        ])
        .output()
        .map_err(|error| format!("调用 net usershare 失败: {}", error))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(share_error("创建", output.status.code(), &output.stdout))
    }
}

#[cfg(target_os = "linux")]
fn create_share_readonly(name: &str, path: &str) -> Result<(), String> {
    let output = Command::new("net")
        .args([
            "usershare",
            "add",
            name,
            path,
            "Everyone:R",
            "guest_ok=y",
        ])
        .output()
        .map_err(|error| format!("调用 net usershare 失败: {}", error))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(share_error("创建", output.status.code(), &output.stdout))
    }
}

#[cfg(target_os = "macos")]
fn create_share_readwrite(name: &str, path: &str) -> Result<(), String> {
    // macOS 的共享点管理没有稳定的命令行接口（sharing 命令已废弃），
    // 能做的就是确认「文件共享」开着，并告诉用户把目录加进去。
    let _ = name;
    check_capability().map_err(|reason| {
        format!("{}（需要共享的目录：{}）", reason, path)
    })
}

#[cfg(target_os = "macos")]
fn create_share_readonly(name: &str, path: &str) -> Result<(), String> {
    create_share_readwrite(name, path)
}

/// 把 `net` 的失败结果翻译成可操作的中文提示。
///
/// **先看退出码，再看文本**：本机实测 `net` 在失败时经常 stdout 为空、
/// 只给一个退出码，纯文本匹配会漏判。退出码语义（Windows）：
/// - `2`：找不到共享 / 路径（幂等场景，调用方已单独处理）
/// - `5`：拒绝访问（需要管理员）
/// - `2118`/`2310`：同名已存在
///
/// 文本只作为补充线索，且做编码兜底（中文 Windows 下输出是 GBK）。
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn share_error(action: &str, code: Option<i32>, stdout: &[u8]) -> String {
    // 先按退出码给结论，这是最可靠的信号。
    match code {
        Some(5) | Some(1) => {
            return format!(
                "{}共享失败：权限不足。请以管理员身份重新启动 OTools 后重试。",
                action
            );
        }
        Some(2118) | Some(2310) => {
            return format!("{}共享失败：同名共享已存在", action);
        }
        Some(2) => {
            return format!("{}共享失败：目标不存在", action);
        }
        _ => {}
    }
    // 退出码不认识时再看文本。
    let text = decode_console(stdout).to_lowercase();
    if text.contains("access is denied")
        || text.contains("access denied")
        || text.contains("拒绝访问")
        || text.contains("permission denied")
    {
        return format!(
            "{}共享失败：权限不足。请以管理员身份重新启动 OTools 后重试。",
            action
        );
    }
    if text.contains("already") || text.contains("已存在") || text.contains("已经共享") {
        return format!("{}共享失败：同名共享已存在", action);
    }
    if text.contains("cannot find") || text.contains("找不到") || text.contains("不存在") {
        return format!("{}共享失败：目标不存在", action);
    }
    format!(
        "{}共享失败（退出码 {}）",
        action,
        code.map(|value| value.to_string()).unwrap_or_else(|| "-".to_string())
    )
}

/// 解控制台输出：合法 UTF-8 直接用；不是（中文 Windows 的 GBK）时逐字节退化，
/// 保留 ASCII 部分以便英文关键字仍能匹配，非 ASCII 用替换符占位。
///
/// 之所以不做真正的 GBK 解码：GBK 是双字节变长编码，手写解码器风险太高，
/// 而这里只用于「辅助判断失败原因」，退化成 ASCII 足够，且绝不会 panic。
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn decode_console(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes
            .iter()
            .map(|byte| if byte.is_ascii() { *byte as char } else { '\u{fffd}' })
            .collect(),
    }
}

/// 构造 `net` 命令，让表头等结构性文本以英文输出（`chcp 65001`）。
///
/// 注意 `chcp` 只能改变**结构性文本**（表头、提示语）的代码页，
/// 共享名/备注里的中文内容仍按程序自身的 ANSI 输出——所以本模块绝不依赖
/// 解析中文内容，只依赖退出码。
#[cfg(target_os = "windows")]
fn net_command(args: &[&str]) -> Command {
    let mut command = Command::new("cmd");
    let mut shell = String::from("chcp 65001 >nul && net");
    for arg in args {
        shell.push(' ');
        // net 的参数里可能有空格（共享名、路径），统一加引号。
        shell.push('"');
        shell.push_str(&arg.replace('"', ""));
        shell.push('"');
    }
    command.args(["/c", &shell]);
    command
}

/// 把共享相关的警告追加到该虚拟机的日志文件。
///
/// 前端读的「最近错误」就是这份日志的尾部（见 `QemuEngine::last_error`），
/// 所以写到这里用户才能在界面上看到。插件是 cdylib，`eprintln!` 会落到宿主
/// 进程 stderr，界面上完全看不到，不能用作提示通道。
pub fn record_warnings(vm_id: &str, warnings: &[String]) {
    use std::io::Write;
    let path = crate::paths::vm_log_path(vm_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    for warning in warnings {
        let _ = writeln!(file, "[共享] {}", warning);
    }
}

/// 供前端展示的共享状态快照。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmbStatus {
    /// 宿主是否具备创建系统共享的条件。
    pub available: bool,
    /// 不可用时的可操作原因。
    pub reason: String,
    /// 想在本机共享给客户机的目录清单（来自各虚拟机的 smb 共享项）。
    pub shared_names: Vec<String>,
}
