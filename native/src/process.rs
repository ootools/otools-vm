//! 起子进程的统一入口 —— 保证在 Windows 上不弹黑窗口。
//!
//! 插件跑在 GUI 进程里。只要随手 `std::process::Command::new(...)` 起一个
//! 命令行工具，Windows 就会为它分配一个控制台，在用户屏幕上**闪一个黑框**。
//! 起一次闪一次：查一次加速器、探一次网络、问一次 GPU —— 用户看到的就是
//! 「终端窗口不停跳出来」。
//!
//! 之前只有 `install.rs` 里自己写了个私有 `hide_console`，其余二十多处调用
//! 全都没设标志。这类问题不能靠「记得设」，所以统一收到这里：**新代码用
//! `command()`，不要直接用 `Command::new`**。
//!
//! `CREATE_NO_WINDOW` 比 `DETACHED_PROCESS` 合适：前者创建进程但不分配
//! 控制台，子进程的 stdout/stderr 仍能正常重定向（`output()` 照样能读到）；
//! 后者会彻底脱离，某些工具的输出行为会变得不可预期。我们大量依赖读取
//! 子进程输出（`whoami`、`ipconfig`、`-device help`），所以取前者。

use std::process::Command;

/// Windows 上创建进程但不分配控制台窗口。
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 建一个「不会弹黑框」的命令。
///
/// 所有起子进程的地方都该走这里。直接 `Command::new` 在 Windows 上会让
/// 用户屏幕上闪一个控制台窗口——这是肉眼可见的体验问题，不是洁癖。
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// 给已建好的命令补上「不弹窗」标志。
///
/// 用于那些已经在别处构造了 `Command`、只想补一个标志的场景
/// （例如需要先 `args()` 再 spawn 的流程）。
#[cfg(target_os = "windows")]
pub fn suppress_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
pub fn suppress_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：`command()` 必须能正常起进程并读到输出。
    ///
    /// `CREATE_NO_WINDOW` 容易被误设成 `DETACHED_PROCESS`，后者会让子进程
    /// 的输出管道行为变掉。这条用例保证「不弹窗」与「能读输出」可以同时成立。
    #[test]
    fn command_can_still_capture_output() {
        #[cfg(target_os = "windows")]
        let mut cmd = command("cmd");
        #[cfg(target_os = "windows")]
        let cmd = cmd.args(["/C", "echo", "ok"]);

        #[cfg(not(target_os = "windows"))]
        let mut cmd = command("echo");
        #[cfg(not(target_os = "windows"))]
        let cmd = cmd.arg("ok");

        let output = cmd.output().expect("起进程失败");
        assert!(output.status.success(), "命令应成功退出");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("ok"), "应能读到输出，实际: {}", text);
    }
}
