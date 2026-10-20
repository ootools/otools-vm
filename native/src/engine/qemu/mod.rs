//! QEMU 后端：非 macOS 宿主，或需要跑非本机架构客户机时的回落路径。
//!
//! 与 VZ 后端的取舍：
//! - **能跑**：virtio-gpu + virtio-blk + virtio-net + 共享目录
//!   （9p 给 Linux 客户机；virtio-fs 给 Windows 客户机，需宿主持有 virtiofsd）；
//! - **不能**：画面走 QEMU 自己的原生窗口（`-display cocoa/gtk/sdl`），
//!   因此剪贴板与「窗口缩放自动改分辨率」需要 SPICE 客户端，v1 不提供。
//!
//! 这条路径刻意保持「薄」：只做进程生命周期 + 参数拼装，不试图在插件里
//! 重写一个 VMM。Apple 芯片上的主力体验始终是 VZ。

pub mod network;
pub mod smb;
pub mod virtiofs;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use super::{Engine, ViewRect};
use crate::model::{
    EngineCaps, EngineInfo, GuestOs, NetworkMode, VmSpec, VmState, BOOT_CDROM, BOOT_DISK,
    BOOT_NET, BOOT_USB,
};

/// 一台被本进程托管的 QEMU 子进程。
struct ManagedVm {
    child: Child,
    /// QEMU HMP monitor 端口（绑在 127.0.0.1），供优雅关机用；0 表示未启用。
    monitor_port: u16,
    /// 该虚拟机名下的 virtiofsd 守护进程，随 QEMU 一起退出。
    virtiofsd: Vec<virtiofs::Daemon>,
    /// 本次启动时由本插件创建的宿主原生共享名（停止时删掉，避免越积越多）。
    /// 只有真正由我们创建成功的才会进这个列表——用户自己建的共享不动。
    smb_shares: Vec<String>,
}

static CHILDREN: Mutex<Option<HashMap<String, ManagedVm>>> = Mutex::new(None);

fn with_children<R>(work: impl FnOnce(&mut HashMap<String, ManagedVm>) -> R) -> R {
    let mut guard = CHILDREN.lock().unwrap_or_else(|poison| poison.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    work(map)
}

/// 在回环上挑一个空闲端口，交给 QEMU 的 -monitor tcp:... 用。
fn free_localhost_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
        .unwrap_or(0)
}

/// 往 QEMU monitor 发一条 HMP 命令（如 system_powerdown）。
/// QEMU 刚起来时 monitor 可能还没监听，做短暂重试。返回是否写入成功。
fn send_monitor_command(port: u16, command: &str) -> bool {
    use std::io::Write;
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..20 {
        if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));
            if stream.write_all(command.as_bytes()).is_ok() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// 优雅关机：经 monitor 发 ACPI 电源键（system_powerdown），等客户机自行退出；
/// 超时或没配 monitor 再强杀兜底。
///
/// 直接 kill() 对 Windows 客户机等于拔电，NTFS 有损坏风险；电源键让客户机走正常关机流程。
fn graceful_shutdown(vm: &mut ManagedVm) {
    let powered = if vm.monitor_port != 0 {
        send_monitor_command(vm.monitor_port, "system_powerdown\n")
    } else {
        false
    };
    if powered {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            match vm.child.try_wait() {
                Ok(Some(_)) => {
                    let _ = vm.child.wait();
                    return;
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                Err(_) => break,
            }
        }
    }
    let _ = vm.child.kill();
    let _ = vm.child.wait();
}

/// 连接 monitor 发一条命令并把回读内容收回来（用于 info 类查询）。
fn query_monitor(port: u16, command: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..20 {
        if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));
            if stream.write_all(command.as_bytes()).is_ok() {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                let deadline = std::time::Instant::now() + Duration::from_millis(700);
                while std::time::Instant::now() < deadline {
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(_) => break,
                    }
                }
                return Some(String::from_utf8_lossy(&buf).to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// 经 monitor 查询当前实际生效的加速器（HMP info accelerators）。
/// 只返回能识别的名字；读不到/解析不出返回 None（前端不提示，避免误报）。
fn detect_active_accel(port: u16) -> Option<String> {
    let reply = query_monitor(port, "info accelerators\n")?;
    let lower = reply.to_lowercase();
    // 输出形如 "Accelerators: tcg"；硬件名优先于 tcg 判定，避免把可用的 whpx 误报成软件模拟。
    for name in ["whpx", "hvf", "kvm", "tcg"] {
        if lower.contains(name) {
            return Some(name.to_string());
        }
    }
    None
}

/// 安装介质光盘的总线与设备名。
///
/// Windows 客户机必须用 IDE/ATAPI：boot.wim 读不到 virtio 光盘，装到一半会找不到系统盘。
/// 返回 (总线, 设备名)：IDE → ("ide", "ide-cd")，virtio → ("virtio", virtio 块设备)。
fn cdrom_bus_and_device(spec: &VmSpec) -> (&'static str, String) {
    match spec.os {
        GuestOs::Windows => ("ide", "ide-cd".to_string()),
        _ => ("virtio", virtio_blk_device().to_string()),
    }
}

/// virtio 块设备的设备名按架构选：x86_64 走 PCI，arm64 的 virt 机器走 mmio。
fn virtio_blk_device() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "virtio-blk-device"
    } else {
        "virtio-blk-pci"
    }
}

/// 启动顺序 → bootindex。越靠前 index 越小（从 1 起）；
/// 不在列表里的设备不设 bootindex，QEMU 给最低优先级（仍可启动）。
///
/// **不能同时用 -boot order=**：QEMU 文档明确说两者混用是未定义行为，
/// 固件很可能不从预期设备启动。所以这里只走 bootindex 一条路。
fn boot_index(spec: &VmSpec, device: &str) -> Option<usize> {
    spec.boot_order
        .iter()
        .position(|entry| entry == device)
        .map(|position| position + 1)
}

/// 常见 OVMF/AAVMF 固件位置（尽力而为；找不到会让用户手动指定）。
/// 用正斜杠，Rust/QEMU 在 Windows 上都能接受，免去反斜杠转义。
fn ovmf_candidates(arch: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if arch == "arm64" {
        paths.extend([
            "/usr/share/AAVMF/AAVMF_CODE.fd".to_string(),
            "/usr/share/edk2/aarch64/QEMU_EFI.fd".to_string(),
            "/opt/homebrew/share/qemu/edk2-aarch64-code.fd".to_string(),
            "/usr/local/share/qemu/edk2-aarch64-code.fd".to_string(),
        ]);
    } else {
        paths.extend([
            "C:/Program Files/qemu/share/edk2-ovmf/x64/OVMF_CODE.4m.fd".to_string(),
            "C:/Program Files/qemu/share/OVMF/OVMF_CODE.4m.fd".to_string(),
            "C:/Program Files/qemu/share/edk2/x64/OVMF_CODE.4m.fd".to_string(),
            "/usr/share/OVMF/OVMF_CODE.4m.fd".to_string(),
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd".to_string(),
            "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd".to_string(),
            "/usr/share/qemu/edk2-x86_64-code.fd".to_string(),
            "/opt/homebrew/share/qemu/edk2-x86_64-code.fd".to_string(),
            "/usr/local/share/qemu/edk2-x86_64-code.fd".to_string(),
        ]);
    }
    paths
}

pub struct QemuEngine {
    binary: PathBuf,
    version: String,
    arch: String,
    /// 运行时探测到的实际加速器（whpx/tcg/…），启动后经 monitor 查询并缓存。
    active_accel: std::sync::Mutex<Option<String>>,
}

impl QemuEngine {
    pub fn create_or_noop(_reason: String) -> Self {
        match Self::detect() {
            Some(engine) => engine,
            None => Self {
                binary: PathBuf::new(),
                version: String::new(),
                arch: String::new(),
                active_accel: std::sync::Mutex::new(None),
            },
        }
    }

    fn detect() -> Option<Self> {
        let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
        let candidates = [
            format!("qemu-system-{}", arch),
            "/opt/homebrew/bin/qemu-system-aarch64".to_string(),
            "/usr/local/bin/qemu-system-aarch64".to_string(),
            "/opt/homebrew/bin/qemu-system-x86_64".to_string(),
            "/usr/local/bin/qemu-system-x86_64".to_string(),
            "qemu-system-x86_64".to_string(),
        ];
        for candidate in candidates {
            if let Ok(output) = Command::new(&candidate).arg("--version").output() {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout).to_string();
                    let version = text
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let resolved = resolve_binary(&candidate);
                    return Some(Self {
                        binary: resolved,
                        version,
                        arch: arch.to_string(),
                        active_accel: std::sync::Mutex::new(None),
                    });
                }
            }
        }
        None
    }

    fn is_available(&self) -> bool {
        !self.binary.as_os_str().is_empty()
    }

    /// 解析 UEFI 固件（OVMF）路径。
    ///
    /// 仅当规格选了 `uefi` 才解析：优先用户显式的 `firmware_path`，否则按平台常见位置探测。
    /// 都找不到就报错（而不是默默退回 BIOS）——Windows 11 等没有 UEFI 起不来。
    fn resolve_ovmf(&self, spec: &VmSpec) -> Result<Option<String>, String> {
        if spec.firmware != "uefi" {
            return Ok(None);
        }
        if let Some(path) = spec.firmware_path.as_deref() {
            if !path.trim().is_empty() {
                if std::path::Path::new(path).is_file() {
                    return Ok(Some(path.to_string()));
                }
                return Err(format!("UEFI 固件文件不存在：{}", path));
            }
        }
        for candidate in ovmf_candidates(spec.arch.as_str()) {
            if std::path::Path::new(&candidate).is_file() {
                return Ok(Some(candidate));
            }
        }
        Err("已选择 UEFI 但未找到 OVMF 固件，请在虚拟机设置里指定固件文件路径".to_string())
    }

    fn display_arg() -> &'static str {
        if cfg!(target_os = "macos") {
            "cocoa"
        } else if cfg!(target_os = "windows") {
            "sdl"
        } else {
            "gtk"
        }
    }

    /// 加速后端。
    ///
    /// Windows 优先系统虚拟化平台 WHPX（Windows Hypervisor Platform），拿不到时
    /// 回落到 TCG 软件模拟而不是启动失败——没开「Windows Hyper-V 平台」的用户至少能跑起来。
    /// macOS 用 HVF，Linux 用 KVM；它们也都会在没有硬件虚拟化时由 QEMU 自己退回 TCG。
    fn accelerator() -> &'static str {
        if cfg!(target_os = "macos") {
            "hvf"
        } else if cfg!(target_os = "windows") {
            "whpx:tcg"
        } else {
            "kvm"
        }
    }

    fn machine_type() -> &'static str {
        if cfg!(target_arch = "aarch64") {
            "virt"
        } else {
            "q35"
        }
    }

    /// 音频后端：必须按宿主平台选，否则 QEMU 会因不认识的后端而启动失败。
    ///
    /// 旧实现硬编码 `coreaudio`（macOS 专属），在 Windows / Linux 上直接崩启动。
    fn audio_backend() -> &'static str {
        if cfg!(target_os = "macos") {
            "coreaudio"
        } else if cfg!(target_os = "windows") {
            "dsound"
        } else {
            "alsa"
        }
    }

    fn build_args(&self, spec: &VmSpec, monitor_port: u16, ovmf: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        args.push("-name".into());
        args.push(spec.name.clone());
        args.push("-machine".into());
        args.push(format!("{},accel={}", Self::machine_type(), Self::accelerator()));
        if cfg!(target_arch = "aarch64") {
            args.push("-cpu".into());
            args.push("host".into());
        }
        // UEFI 固件（OVMF）：Windows 11 等需要；仅在解析到路径时覆盖默认 BIOS。
        if let Some(ovmf) = ovmf {
            args.push("-bios".into());
            args.push(ovmf.to_string());
        }
        args.push("-smp".into());
        args.push(spec.cpus.to_string());
        args.push("-m".into());
        args.push(spec.memory_mb.to_string());

        // 系统盘。
        //
        // bootindex 只能挂在 -device 上，-drive 简写带不了（QEMU 文档：
        // "no way to set a bootindex property ... with -hda / -cdrom"）。
        // 所以只在用户配了启动顺序时展开成长格式；没配就保持简写——
        // 命令行与改动前逐字一致，零回归风险。
        match boot_index(spec, BOOT_DISK) {
            Some(index) => {
                args.push("-drive".into());
                args.push(format!(
                    "if=none,id=sysdisk,format=raw,cache=writethrough,file={}",
                    spec.disk_path
                ));
                args.push("-device".into());
                args.push(format!(
                    "{},drive=sysdisk,bootindex={}",
                    virtio_blk_device(),
                    index
                ));
            }
            None => {
                args.push("-drive".into());
                args.push(format!(
                    "file={},if=virtio,format=raw,cache=writethrough",
                    spec.disk_path
                ));
            }
        }

        // 安装介质。配了启动顺序时同样展开成长格式以携带 bootindex。
        if let Some(installer) = spec.installer_path.as_deref() {
            if !installer.trim().is_empty() {
                match boot_index(spec, BOOT_CDROM) {
                    Some(index) => {
                        let (_, device) = cdrom_bus_and_device(spec);
                        args.push("-drive".into());
                        args.push(format!(
                            "if=none,id=instcd,media=cdrom,readonly=on,file={}",
                            installer
                        ));
                        args.push("-device".into());
                        args.push(format!("{},drive=instcd,bootindex={}", device, index));
                    }
                    None => {
                        let (bus, _) = cdrom_bus_and_device(spec);
                        args.push("-drive".into());
                        args.push(format!(
                            "file={},if={},media=cdrom,readonly=on",
                            installer, bus
                        ));
                    }
                }
            }
        }

        // VirtIO 驱动 ISO（virtio-win 等）：以 IDE 光盘附加，Windows 安装「加载驱动」可直接读取。
        if let Some(iso) = spec.driver_iso.as_deref() {
            if !iso.trim().is_empty() {
                args.push("-drive".into());
                args.push(format!("file={},if=ide,media=cdrom,readonly=on", iso));
            }
        }

        // U 盘介质：RAW 磁盘镜像以 USB 大容量存储设备挂入，客户机里就是一个 U 盘，
        // 可在启动顺序里当启动项（SeaBIOS / OVMF 都能从 usb-storage 引导）。
        //
        // 不指定 bus：QEMU 会挂到机器默认的 USB 总线上，与 usb-kbd / usb-tablet 同一条。
        // 万一宿主 QEMU 没编译 USB 支持，QEMU 会明确报 "No 'USB' bus found"，
        // 比静默不出现要好。
        if let Some(media) = spec.usb_media.as_deref() {
            if !media.trim().is_empty() {
                args.push("-drive".into());
                args.push(format!("if=none,id=usbmedia,format=raw,file={}", media));
                args.push("-device".into());
                let mut device = "usb-storage,drive=usbmedia".to_string();
                if let Some(index) = boot_index(spec, BOOT_USB) {
                    device.push_str(&format!(",bootindex={}", index));
                }
                args.push(device);
            }
        }

        // 网络：按网络模式拼后端（NAT / 桥接 / 仅主机），见 network 模块。
        // 端口转发只在 NAT 下有意义——`VmSpec::validate` 已拦掉其它组合。
        match network::build_netdev(spec) {
            Ok(net_args) => args.extend(net_args),
            Err(error) => {
                // build_args 返回 Vec 而不是 Result，这里退回 NAT 保证一定能起，
                // 同时把原因写进日志——真正的拦截在 validate 里，走不到这里。
                eprintln!("[vm] 网络后端构造失败，回落到 NAT: {}", error);
                args.push("-netdev".into());
                args.push("user,id=net0".into());
            }
        }
        args.push("-device".into());
        let mut net_device = "virtio-net-pci,netdev=net0".to_string();
        if let Some(index) = boot_index(spec, BOOT_NET) {
            net_device.push_str(&format!(",bootindex={}", index));
        }
        args.push(net_device);

        // 显示与输入
        args.push("-device".into());
        args.push("virtio-gpu-pci".into());
        args.push("-device".into());
        args.push("usb-kbd".into());
        args.push("-device".into());
        args.push("usb-tablet".into());
        args.push("-device".into());
        args.push("virtio-rng-pci".into());
        if spec.audio {
            let backend = Self::audio_backend();
            if !backend.is_empty() {
                args.push("-audiodev".into());
                args.push(format!("{},id=snd0", backend));
                args.push("-device".into());
                args.push("virtio-sound-pci,audiodev=snd0".into());
            }
        }

        // 共享目录（9p）：客户机 mount -t 9p -o trans=virtio <tag> /mnt/<tag>
        // 走 SMB 的项由宿主文件服务承担，不挂进客户机设备树，这里必须排除，
        // 否则同一目录会既当 SMB 共享又当 9p 挂载点。
        for dir in spec
            .shared_dirs
            .iter()
            .filter(|dir| !virtiofs::is_virtiofs(dir) && !dir.is_smb())
        {
            let mut opts = format!(
                "local,path={},mount_tag={},security_model=none,multidevs=remap",
                dir.host_path, dir.mount_tag
            );
            if dir.read_only {
                opts.push_str(",readonly=on");
            }
            args.push("-virtfs".into());
            args.push(opts);
        }

        // 共享目录（virtio-fs）：每个共享项一个 vhost-user 设备，连到 virtiofs 模块起的 daemon。
        //
        // 两个前提，缺一不可：
        // 1. `memory-backend-memfd` + `-numa node,memdev=mem`：virtio-fs 靠共享内存传数据，
        //    size 必须与 `-m` 一致。加上 `-numa node,memdev=mem` 会替换 QEMU 的默认内存
        //    分配，所以只在真有 virtio-fs 共享项时才加。
        // 2. 宿主持有 virtiofsd（见 `virtiofs::start`）。
        let virtiofs_dirs: Vec<_> = spec
            .shared_dirs
            .iter()
            .filter(|dir| virtiofs::is_virtiofs(dir))
            .collect();
        if !virtiofs_dirs.is_empty() {
            args.push("-object".into());
            args.push(format!(
                "memory-backend-memfd,id=mem,size={}M,share=on",
                spec.memory_mb
            ));
            args.push("-numa".into());
            args.push("node,memdev=mem".into());
        }
        for (index, dir) in virtiofs_dirs.iter().enumerate() {
            let chardev = format!("vfs{}", index);
            args.push("-chardev".into());
            args.push(format!(
                "socket,id={},path={}",
                chardev,
                virtiofs::socket_path(&spec.id, &dir.mount_tag).display()
            ));
            args.push("-device".into());
            args.push(format!(
                "vhost-user-fs-pci,queue-size=1024,chardev={},tag={}",
                chardev, dir.mount_tag
            ));
        }

        // HMP monitor：接在回环 TCP 上，供优雅关机（system_powerdown）使用。
        // 这是本后端唯一的控制通道，画面窗口不带控制台。
        if monitor_port != 0 {
            args.push("-monitor".into());
            args.push(format!("tcp:127.0.0.1:{},server,nowait", monitor_port));
        }

        args.push("-display".into());
        args.push(Self::display_arg().into());

        args
    }
}

fn resolve_binary(candidate: &str) -> PathBuf {
    if candidate.contains('/') {
        return PathBuf::from(candidate);
    }
    if let Ok(output) = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", candidate))
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    PathBuf::from(candidate)
}

impl Engine for QemuEngine {
    fn info(&self) -> EngineInfo {
        if !self.is_available() {
            return EngineInfo {
                kind: "qemu".to_string(),
                available: false,
                label: "QEMU".to_string(),
                reason: "未找到 qemu-system-*。安装 QEMU 后重启宿主即可启用跨平台虚拟机。"
                    .to_string(),
                version: String::new(),
                // QEMU 走子进程，不需要 macOS 虚拟化权限。
                host_entitled: true,
                accelerator: None,
                caps: EngineCaps::unsupported(),
            };
        }
        EngineInfo {
            kind: "qemu".to_string(),
            available: true,
            label: "QEMU".to_string(),
            reason: String::new(),
            version: self.version.clone(),
            host_entitled: true,
            accelerator: self.active_accel.lock().ok().and_then(|guard| guard.clone()),
            caps: EngineCaps {
                native_view: false,
                shared_dirs: true,
                // 9p 覆盖 Linux；virtio-fs 覆盖 Windows（需 virtio-win 的 viofs 驱动）。
                shared_dirs_guests: vec!["linux".to_string(), "windows".to_string()],
                clipboard: false,
                auto_resize: false,
                audio: true,
                snapshots: false,
                // 进程式后端没有写时复制，克隆退化为整盘复制（仍然可用）。
                clone: true,
                resize_disk: true,
                rosetta: false,
                // 桥接 / 仅主机：Windows 走 tap 驱动，macOS 走 vmnet.framework，
                // Linux 走 tap / 宿主网桥。三者都已接通，具体可用性由
                // `vm_network_backend` 命令逐项探测后告诉前端。
                bridged_network: true,
            },
        }
    }

    fn supports(&self, spec: &VmSpec) -> bool {
        self.is_available() && spec.arch == self.arch
    }

    fn start(&self, spec: &VmSpec) -> Result<(), String> {
        if !self.is_available() {
            return Err(self.info().reason);
        }
        spec.validate()?;
        if !std::path::Path::new(&spec.disk_path).exists() {
            return Err(format!("系统盘不存在：{}", spec.disk_path));
        }

        let mut running = false;
        with_children(|children| {
            if let Some(vm) = children.get_mut(&spec.id) {
                if let Ok(None) = vm.child.try_wait() {
                    running = true;
                }
            }
        });
        if running {
            return Err("虚拟机已在运行".to_string());
        }

        // 桥接 / 仅主机需要宿主侧条件（驱动或特权），启前先探一次，
        // 好给出「装驱动 / 提权」这类可操作提示，而不是 QEMU 的原始报错。
        if spec.network != NetworkMode::Nat {
            network::check_backend(spec.network)?;
            if spec.network.needs_interface() && spec.network_iface.is_none() {
                return Err("桥接 / 仅主机网络需要指定宿主网卡".to_string());
            }
        }

        // virtio-fs 共享目录：先起 daemon 再起 QEMU——QEMU 会连 daemon 的 socket，
        // 顺序反了会连不上；起 QEMU 失败时把已起的 daemon 一并收回。
        let mut virtiofsd: Vec<virtiofs::Daemon> = Vec::new();
        if virtiofs::spec_uses_virtiofs(spec) {
            if !virtiofs::host_supports_virtiofs() {
                return Err(virtiofs::unsupported_reason());
            }
            let daemon = virtiofs::resolve_daemon(spec.virtiofsd_path.as_deref())?;
            for dir in spec.shared_dirs.iter().filter(|dir| virtiofs::is_virtiofs(dir)) {
                virtiofsd.push(virtiofs::start(spec, dir, &daemon)?);
            }
        }

        // 宿主原生 SMB 共享：在起 QEMU 之前建好，客户机一起来就能访问到。
        // 只处理走 SMB 的共享项；9p / virtio-fs 由 QEMU 自己的设备参数承担。
        //
        // 建失败**不**中断启动（用户可能只是想先跑起来），但必须让用户看得见：
        // 插件是 cdylib，eprintln 落到宿主进程 stderr，界面上看不到。这里把失败
        // 原因带出为警告文案，由 start 的返回结果交给前端提示。
        let mut smb_shares: Vec<String> = Vec::new();
        let mut smb_warnings: Vec<String> = Vec::new();
        for dir in spec.shared_dirs.iter().filter(|dir| dir.is_smb()) {
            let name = smb::share_name(dir);
            match smb::ensure_share(dir) {
                Ok(()) => smb_shares.push(name),
                Err(error) => {
                    // 不是我们建的，别放进清理列表。
                    smb_warnings.push(format!("共享「{}」不可用：{}", dir.mount_tag, error));
                }
            }
        }
        if !smb_warnings.is_empty() {
            smb::record_warnings(&spec.id, &smb_warnings);
        }

        let monitor_port = free_localhost_port();
        let ovmf = self.resolve_ovmf(spec)?;
        let args = self.build_args(spec, monitor_port, ovmf.as_deref());
        let log_path = crate::paths::vm_log_path(&spec.id);
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let log = std::fs::File::create(&log_path).ok();

        let mut command = Command::new(&self.binary);
        command.args(&args).stdin(Stdio::null());
        if let Some(file) = log {
            let cloned = file.try_clone().ok();
            command.stdout(file);
            if let Some(cloned) = cloned {
                command.stderr(cloned);
            }
        } else {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                for daemon in &mut virtiofsd {
                    daemon.stop();
                }
                for name in &smb_shares {
                    let _ = smb::remove_share(name);
                }
                return Err(format!("启动 QEMU 失败: {}", error));
            }
        };
        with_children(|children| {
            children.insert(
                spec.id.clone(),
                ManagedVm {
                    child,
                    monitor_port,
                    virtiofsd,
                    smb_shares,
                },
            );
        });
        // 起成功后探一次实际加速器，供前端在跑 TCG 软件模拟时提示开启硬件加速。
        if monitor_port != 0 {
            if let Some(accel) = detect_active_accel(monitor_port) {
                if let Ok(mut guard) = self.active_accel.lock() {
                    *guard = Some(accel);
                }
            }
        }
        Ok(())
    }

    fn stop(&self, spec: &VmSpec) -> Result<(), String> {
        let removed = with_children(|children| children.remove(&spec.id));
        if let Some(mut vm) = removed {
            graceful_shutdown(&mut vm);
            for daemon in &mut vm.virtiofsd {
                daemon.stop();
            }
            for name in &vm.smb_shares {
                let _ = smb::remove_share(name);
            }
        }
        Ok(())
    }

    fn pause(&self, _spec: &VmSpec) -> Result<(), String> {
        Err("QEMU 后端暂不支持暂停".to_string())
    }

    fn resume(&self, _spec: &VmSpec) -> Result<(), String> {
        Err("QEMU 后端暂不支持继续".to_string())
    }

    fn state(&self, spec: &VmSpec) -> VmState {
        with_children(|children| match children.get_mut(&spec.id) {
            Some(vm) => match vm.child.try_wait() {
                Ok(None) => VmState::Running,
                Ok(Some(_)) => {
                    children.remove(&spec.id);
                    VmState::Stopped
                }
                Err(_) => VmState::Error,
            },
            None => VmState::Stopped,
        })
    }

    fn last_error(&self, spec: &VmSpec) -> String {
        let path = crate::paths::vm_log_path(&spec.id);
        std::fs::read_to_string(path)
            .map(|text| {
                text.lines()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn attach_view(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Err("QEMU 后端的画面在 QEMU 自己的窗口里，不支持内嵌".to_string())
    }

    fn update_view_rect(&self, _spec: &VmSpec, _rect: ViewRect) -> Result<(), String> {
        Ok(())
    }

    fn detach_view(&self, _spec: &VmSpec) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&self) {
        with_children(|children| {
            for (_, mut vm) in children.drain() {
                let _ = vm.child.kill();
                let _ = vm.child.wait();
                for daemon in &mut vm.virtiofsd {
                    daemon.stop();
                }
                for name in &vm.smb_shares {
                    let _ = smb::remove_share(name);
                }
            }
        });
    }
}

#[allow(dead_code)]
fn guest_is_linux(spec: &VmSpec) -> bool {
    matches!(spec.os, GuestOs::Linux)
}
