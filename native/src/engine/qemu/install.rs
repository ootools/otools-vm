//! QEMU 运行时获取：让用户不必自己去装。
//!
//! 之前 QEMU 不可用时只给一句「安装 QEMU 后重启宿主」——把找官网、选版本、
//! 解压、配 PATH 全推给用户。这里把它变成一次点击。
//!
//! ## 为什么不是「下载 zip 解压」
//!
//! QEMU 官方 Windows 分发只有 NSIS 安装器（`qemu-w64-setup-*.exe`），其 manifest
//! 声明 `requestedExecutionLevel = requireAdministrator`，也没有便携版 zip。
//! 想「解包不安装」就必须依赖第三方 7-Zip——而 7-Zip 不是系统组件，把成功路径
//! 建在它上面等于换个方式让用户先装别的东西。
//!
//! 所以这里按可获得性排队，取第一条能走通的：
//!
//! 1. **winget**（Win10 1809+ / Win11 自带）：`winget install -e --id <QEMU>`，
//!    自己处理下载、校验、UAC 提权与卸载登记，是最省事也最规范的一条。
//! 2. **官方安装器静默安装到插件数据目录**：`setup.exe /S /D=<dir>`。同样要过
//!    UAC，但落点在我们自己的目录里，不写 Program Files。
//! 3. **Homebrew**（macOS）：`brew install qemu`。
//! 4. 都不行 → 明确告诉用户缺什么、给出命令，而不是假装在装。
//!
//! ## 设计约束
//!
//! - **不静默提权**：UAC 必须由用户点，插件不尝试绕过。被拒就报「已取消」。
//! - **进度可查**：安装跑在后台线程，前端轮询，不是一个卡住的按钮。
//! - **失败可重试**：半截文件要清掉，否则下次会被当成已完成。
//! - **成功立刻生效**：装完主动让引擎重新探测，否则用户还得重启宿主。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// QEMU 的 winget 包标识。
///
/// 用固定 id 而不是搜索名字：搜索结果会随源更新漂移，id 是稳定的。
const WINGET_ID: &str = "SoftwareFreedomConservancy.QEMU";

/// 直接下载时的固定构建日期。
///
/// 不追 latest：版本漂移会让命令行参数兼容性变成随机事件，而我们只用它跑
/// 已经验证过的那套参数。需要升级时改这个常量并回归一次。
const WINDOWS_BUILD: &str = "20250422";

/// 获取方式。前端据此显示不同的按钮文案与说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Strategy {
    /// `winget install`。
    Winget,
    /// 下载官方安装器并静默安装到插件目录。
    Installer,
    /// `brew install qemu`。
    Homebrew,
    /// 自动获取不可用，只能手工安装。
    Manual,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Winget => "winget",
            Strategy::Installer => "installer",
            Strategy::Homebrew => "homebrew",
            Strategy::Manual => "manual",
        }
    }
}

/// 一条获取建议：走哪条路、为什么、需要用户做什么。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// 是否支持一键获取（`manual` 时为 false）。
    pub automatic: bool,
    pub strategy: String,
    /// 需要用户配合的事（例如「会弹 UAC，请点允许」）。空表示无需干预。
    pub note: String,
    /// 手工安装时给用户复制走的命令或链接。
    pub command: String,
    /// 为什么是这个策略。
    pub reason: String,
}

/// 安装进度。前端轮询 `vm_qemu_install_status` 拿到。
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatus {
    /// `idle` / `downloading` / `installing` / `done` / `failed`
    pub phase: String,
    /// 走的是哪条路（`Strategy::as_str`）。
    pub strategy: String,
    /// 0.0 ~ 1.0；总长度未知时为 0。
    pub progress: f64,
    /// 已下载字节（仅 installer 策略有值）。
    pub downloaded: u64,
    /// 总字节；未知为 0。
    pub total: u64,
    pub error: String,
    /// 成功后 QEMU 可执行文件的绝对路径。
    pub binary: String,
}

static STATUS: Mutex<Option<InstallStatus>> = Mutex::new(None);

fn set_status(update: impl FnOnce(&mut InstallStatus)) {
    if let Ok(mut guard) = STATUS.lock() {
        let status = guard.get_or_insert_with(InstallStatus::default);
        update(status);
    }
}

pub fn status() -> InstallStatus {
    STATUS
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_default()
}

/// 是否正在安装/下载。
pub fn is_running() -> bool {
    matches!(status().phase.as_str(), "downloading" | "installing")
}

/// 安装目标目录：`~/.otools/local/vm/qemu`。
///
/// 放在插件数据目录而不是 Program Files：卸载插件即清干净，也不需要管理员。
pub fn install_root() -> PathBuf {
    crate::paths::vm_root_dir().join("qemu")
}

/// 我们期望的 QEMU 可执行文件名（不含目录）。
///
/// `bundled_candidates` 与安装器落点都用这一份，避免两处各算一遍 arch。
pub fn binary_name() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    if cfg!(target_os = "windows") {
        format!("qemu-system-{}.exe", arch)
    } else {
        format!("qemu-system-{}", arch)
    }
}

pub fn binary_path() -> PathBuf {
    install_root().join("bin").join(binary_name())
}

/// 本平台是否能自动获取。
pub fn is_supported_platform() -> bool {
    cfg!(target_os = "windows") || cfg!(target_os = "macos")
}

/// 选出当前机器上可行的获取策略。
///
/// 顺序即优先级：winget 最规范（带校验、可卸载、能自动提权），安装器次之，
/// brew 只对 macOS 有意义。
pub fn plan() -> Plan {
    if cfg!(target_os = "windows") {
        if winget_available() {
            return Plan {
                automatic: true,
                strategy: Strategy::Winget.as_str().to_string(),
                note: "安装时可能弹出系统提权确认，请选择「是」。".to_string(),
                command: format!("winget install -e --id {}", WINGET_ID),
                reason: "用系统自带的应用安装器安装，带完整性校验，之后可在系统里正常卸载"
                    .to_string(),
            };
        }
        return Plan {
            automatic: true,
            strategy: Strategy::Installer.as_str().to_string(),
            note: "安装时可能弹出系统提权确认，请选择「是」。".to_string(),
            command: download_url().unwrap_or_default(),
            reason: "下载官方安装包并安装到插件目录，不影响系统里其他程序".to_string(),
        };
    }

    if cfg!(target_os = "macos") {
        let has_brew = command_ok("brew", &["--version"]);
        return Plan {
            automatic: has_brew,
            strategy: if has_brew {
                Strategy::Homebrew.as_str()
            } else {
                Strategy::Manual.as_str()
            }
            .to_string(),
            note: if has_brew {
                "安装过程可能需要输入系统密码。".to_string()
            } else {
                "未检测到 Homebrew，请先安装它，或从 qemu.org 手动安装。".to_string()
            },
            command: "brew install qemu".to_string(),
            reason: if has_brew {
                "macOS 上 QEMU 由一组可执行文件与固件组成，用 Homebrew 安装才能保证依赖完整"
                    .to_string()
            } else {
                "macOS 没有官方便携包，需要用包管理器安装".to_string()
            },
        };
    }

    Plan {
        automatic: false,
        strategy: Strategy::Manual.as_str().to_string(),
        note: String::new(),
        command: String::new(),
        reason: "当前平台请用系统包管理器安装 QEMU（如 apt install qemu-system-x86）".to_string(),
    }
}

/// 后台启动获取。重复调用只生效一次（已在跑则直接返回错误）。
pub fn start_install() -> Result<(), String> {
    let current_plan = plan();
    if !current_plan.automatic {
        return Err(current_plan.reason);
    }
    if is_running() {
        return Err("已经在安装中，请等待当前进度完成".to_string());
    }

    let strategy = match current_plan.strategy.as_str() {
        "winget" => Strategy::Winget,
        "installer" => Strategy::Installer,
        "homebrew" => Strategy::Homebrew,
        other => return Err(format!("未知的获取方式: {}", other)),
    };

    // installer 会先下载再装，其余直接进入安装状态。
    let phase = match strategy {
        Strategy::Installer => "downloading",
        _ => "installing",
    };
    set_status(|status| {
        *status = InstallStatus {
            phase: phase.to_string(),
            strategy: strategy.as_str().to_string(),
            ..InstallStatus::default()
        }
    });

    std::thread::spawn(move || {
        let result = match strategy {
            Strategy::Winget => install_with_winget(),
            Strategy::Installer => install_with_installer(),
            Strategy::Homebrew => install_with_brew(),
            Strategy::Manual => Err("当前平台请手动安装 QEMU".to_string()),
        };
        match result {
            Ok(binary) => {
                set_status(|status| {
                    status.phase = "done".to_string();
                    status.progress = 1.0;
                    status.binary = binary.to_string_lossy().to_string();
                    status.error.clear();
                });
                // 让引擎重新探测，否则用户装完了还得重启宿主才发现。
                // 运行中的机器不受影响——重探测只在无机器在跑时替换引擎。
                crate::reprobe_engine_silently();
            }
            Err(reason) => {
                set_status(|status| {
                    status.phase = "failed".to_string();
                    status.error = reason;
                });
            }
        }
    });
    Ok(())
}

fn download_url() -> Option<String> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    Some(format!(
        "https://qemu.weilnetz.de/w64/2025/qemu-w64-setup-{}.exe",
        WINDOWS_BUILD
    ))
}

// ---------------------------------------------------------------- 各条路径

/// `winget install`：最规范的一条。
///
/// 参数要点：`--accept-package-agreements` / `--accept-source-agreements` 让它
/// 在非交互环境下不卡在协议确认上；`--disable-interactivity` 避免它弹自己的
/// 进度 UI 跟我们抢前台。UAC 该弹还是会弹，那是用户的选择。
fn install_with_winget() -> Result<PathBuf, String> {
    if let Some(found) = find_installed_binary() {
        return Ok(found);
    }

    let mut command = std::process::Command::new("winget");
    command.args([
        "install",
        "--id",
        WINGET_ID,
        "--exact",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ]);
    hide_console(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("无法启动 winget: {}", error))?;

    // winget 的失败也常带「已安装」语义，所以**先查实物、再看退出码**：
    // 已装过时 winget 返回非 0，但机器上是好的。
    if let Some(found) = find_installed_binary() {
        return Ok(found);
    }

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Err(describe_winget_failure(&combined, output.status.code()))
}

/// 把 winget 的输出翻译成用户能照着做的说明。
fn describe_winget_failure(text: &str, code: Option<i32>) -> String {
    let lower = text.to_lowercase();
    // 用户点了 UAC 的「否」时 winget 的典型文案。
    if lower.contains("0x800704c7") || lower.contains("cancel") || lower.contains("已取消") {
        return "安装被取消：系统提权确认没有通过。请重试并在弹窗里选择「是」。".to_string();
    }
    if lower.contains("no applicable installer") || lower.contains("找不到适用的安装程序") {
        return "winget 没有找到适用于本机的 QEMU 安装包，请改用官方安装包手动安装。".to_string();
    }
    if lower.contains("msstore") && lower.contains("source") {
        return "winget 源不可用，请先运行 `winget source update` 或改用官方安装包。".to_string();
    }
    let tail: String = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect();
    if tail.is_empty() {
        format!(
            "winget 安装失败（退出码 {:?}）。请改用官方安装包手动安装。",
            code
        )
    } else {
        format!("winget 安装失败：{}", tail)
    }
}

/// 下载官方安装器 + 静默安装到插件目录。
///
/// `/S` 是 NSIS 的静默开关，`/D=` 指定目标目录——**必须是最后一个参数且不能加
/// 引号**，这是 NSIS 的硬性要求，加了引号会被当成路径的一部分。
fn install_with_installer() -> Result<PathBuf, String> {
    let root = install_root();
    let bin_dir = root.join("bin");
    crate::paths::ensure_dir(&bin_dir)?;

    let url = download_url().ok_or_else(|| "当前平台没有可用的下载源".to_string())?;
    // 半截文件单独放：下载成功才继续，中断的残留不会污染下次尝试。
    let archive = root.join("qemu-setup.exe.part");
    fetch(&url, &archive)?;

    set_status(|status| status.phase = "installing".to_string());
    let mut command = std::process::Command::new(&archive);
    command.arg("/S").arg(format!("/D={}", root.display()));
    hide_console(&mut command);
    let status = command
        .status()
        .map_err(|error| format!("无法启动安装程序: {}", error))?;
    if !status.success() {
        let _ = std::fs::remove_file(&archive);
        return Err("安装程序返回失败，请改用官方安装包手动安装。".to_string());
    }

    // 安装器可能把可执行文件放在 root 下任意层级，也可能直接铺在 root 里。
    let found = find_binary(&root).ok_or_else(|| {
        "安装已完成，但没有找到 qemu-system 可执行文件。请改用官方安装包手动安装。".to_string()
    })?;

    // 统一收进 bin/，和 `bundled_candidates` 的约定一致。
    let target = bin_dir.join(found.file_name().unwrap_or_default());
    if found != target {
        std::fs::rename(&found, &target)
            .or_else(|_| std::fs::copy(&found, &target).map(|_| ()))
            .map_err(|error| format!("移动 QEMU 可执行文件失败: {}", error))?;
    }

    let _ = std::fs::remove_file(&archive);
    verify_runs(&target)?;
    Ok(target)
}

fn install_with_brew() -> Result<PathBuf, String> {
    let status = std::process::Command::new("brew")
        .args(["install", "qemu"])
        .status()
        .map_err(|error| format!("无法启动 Homebrew: {}", error))?;
    if !status.success() {
        return Err("Homebrew 安装失败，请检查网络或手动执行 `brew install qemu`。".to_string());
    }
    find_installed_binary()
        .ok_or_else(|| "Homebrew 安装完成，但没能找到 qemu-system 可执行文件".to_string())
}

// ---------------------------------------------------------------- 探测与下载

/// 在已知位置找一个能跑的 QEMU。
///
/// 与 `qemu::bundled_candidates` 的区别：这里只关心「装到哪了」，不参与引擎
/// 首选顺序；而且要把 winget / 官方安装器落在 Program Files 的位置也认出来。
fn find_installed_binary() -> Option<PathBuf> {
    let name = binary_name();
    let mut candidates: Vec<PathBuf> = vec![binary_path()];
    if cfg!(target_os = "windows") {
        for base in ["C:/Program Files/qemu", "C:/Program Files (x86)/qemu"] {
            candidates.push(PathBuf::from(base).join(&name));
        }
    } else {
        for base in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            candidates.push(PathBuf::from(base).join(&name));
        }
    }
    for candidate in candidates {
        if candidate.is_file() && verify_runs(&candidate).is_ok() {
            return Some(candidate);
        }
    }
    // PATH 里可能有（比如 winget 把 Links 目录塞进了 PATH）。
    if let Ok(output) = std::process::Command::new(&name).arg("--version").output() {
        if output.status.success() {
            return Some(PathBuf::from(&name));
        }
    }
    None
}

/// 确认这个文件真能跑起来。
///
/// 拿不到版本号通常意味着缺 DLL：早报比等到启动虚拟机再报好。
fn verify_runs(binary: &Path) -> Result<(), String> {
    std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|_| ())
        .ok_or_else(|| {
            format!(
                "QEMU 无法运行（{}），可能缺少依赖库。请改用官方安装包安装。",
                binary.display()
            )
        })
}

/// 带进度的下载。
///
/// 实现选择：调用系统自带的 `curl.exe`（Windows 10 1803+ 都有），而不是自己
/// 拼 HTTP 客户端——跟随重定向 + TLS 需要额外依赖，只为下个文件不划算。
/// 进度靠**轮询目标文件大小**得到：跨 curl 版本可靠，不依赖它把进度打到 stderr
/// 的具体格式。
fn fetch(url: &str, target: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(target);

    let mut command = std::process::Command::new("curl");
    command
        .args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(target)
        .arg(url);
    hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 curl: {}", error))?;

    // 后台线程报进度；主线程等 curl 退出。
    let watched = target.to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_for_watcher = std::sync::Arc::clone(&stop);
    let watcher = std::thread::spawn(move || {
        // 总量未知，用安装包的名义大小（约 180 MB）估算，只影响进度条观感。
        const ESTIMATE: u64 = 180 * 1024 * 1024;
        while !stop_for_watcher.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(meta) = std::fs::metadata(&watched) {
                let downloaded = meta.len();
                set_status(|status| {
                    status.downloaded = downloaded;
                    status.total = ESTIMATE;
                    status.progress = (downloaded as f64 / ESTIMATE as f64).min(0.99);
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    });

    let result = child.wait();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = watcher.join();

    let status = result.map_err(|error| format!("等待 curl 失败: {}", error))?;
    if !status.success() || !target.is_file() {
        let _ = std::fs::remove_file(target);
        return Err("下载失败：请检查网络，或改用官方安装包手动安装".to_string());
    }
    Ok(())
}

/// 在目录树里找 QEMU 可执行文件。
///
/// 安装器解出来的层级不固定（可能多一层 `qemu/`），所以用栈遍历并带上访问上限，
/// 防止扫到奇怪的深目录。
fn find_binary(root: &Path) -> Option<PathBuf> {
    let wanted = binary_name();
    let mut queue = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = queue.pop() {
        visited += 1;
        if visited > 2000 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                queue.push(path);
            } else if path
                .file_name()
                .map(|name| name.to_string_lossy() == wanted.as_str())
                .unwrap_or(false)
            {
                return Some(path);
            }
        }
    }
    None
}

fn command_ok(program: &str, args: &[&str]) -> bool {
    let mut command = std::process::Command::new(program);
    command.args(args);
    hide_console(&mut command);
    command
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn winget_available() -> bool {
    if !cfg!(target_os = "windows") {
        return false;
    }
    command_ok("winget", &["--version"])
}

/// 不弹黑色控制台窗口。
///
/// 插件跑在 GUI 进程里，随手 spawn 一个命令行工具会在用户屏幕上闪一个黑框——
/// 装 QEMU 要跑几分钟，那个黑框会一直在。
#[cfg(target_os = "windows")]
fn hide_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hide_console(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_matches_platform_arch() {
        let name = binary_name();
        if cfg!(target_arch = "aarch64") {
            assert!(name.contains("aarch64"), "{}", name);
        } else {
            assert!(name.contains("x86_64"), "{}", name);
        }
        // Windows 上必须是 .exe，否则 Path::is_file 与执行都会失败。
        assert_eq!(name.ends_with(".exe"), cfg!(target_os = "windows"));
    }

    #[test]
    fn install_root_lives_under_plugin_data() {
        // 装到插件数据目录而不是 Program Files：卸载即清干净，不需要管理员。
        let root = install_root();
        assert!(root.starts_with(crate::paths::vm_root_dir()));
        assert!(root.ends_with("qemu"));
    }

    #[test]
    fn status_starts_not_running() {
        let current = status();
        assert!(!matches!(current.phase.as_str(), "downloading" | "installing"));
    }

    #[test]
    fn plan_is_self_consistent() {
        let current = plan();
        // 自动获取必须给出策略；手工获取必须给出可照做的理由。
        if current.automatic {
            assert!(
                ["winget", "installer", "homebrew"].contains(&current.strategy.as_str()),
                "{}",
                current.strategy
            );
        } else {
            assert_eq!(current.strategy, "manual");
        }
        assert!(!current.reason.is_empty());
    }

    #[test]
    fn winget_cancel_is_reported_as_user_cancelled() {
        // 用户点了 UAC 的「否」时，不能只说「安装失败」——那会让人以为程序坏了。
        let message = describe_winget_failure("Installer failed with exit code: 0x800704C7", Some(1));
        assert!(message.contains("取消"), "{}", message);
    }

    #[test]
    fn winget_failure_keeps_actionable_tail() {
        let message = describe_winget_failure("搜索源时出错\r\n无法访问 msstore 源", Some(1));
        assert!(
            message.contains("msstore") || message.contains("源不可用"),
            "{}",
            message
        );
    }
}
