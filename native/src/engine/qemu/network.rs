//! QEMU 网络后端：按网络模式拼装 netdev 参数，并管理宿主侧网卡选择。
//!
//! 三条路，各自对应 QEMU 的一个后端：
//!
//! | 模式 | QEMU 后端 | 客户机能被访问 | 需要管理员 |
//! |------|-----------|----------------|-----------|
//! | NAT | `user`（SLIRP） | 否（要靠端口转发） | 否 |
//! | 桥接 | `tap` / `vmnet-bridged` | 是，局域网内直接可达 | 通常是 |
//! | 仅主机 | `tap`（私有网卡）/ `vmnet-host` | 只有宿主 | 通常是 |
//!
//! **平台差异是这里唯一的复杂点**：
//! - Linux：`tap`，桥接靠 `br=` 把 tap 挂到网桥上，仅主机靠一张不桥接的 tap。
//! - Windows：`tap`，网卡名走 `ifname=`（OpenVPN 的 tap-windows6 驱动），
//!   桥接由 Windows 自己的「网桥」承担，QEMU 只负责连上那张 tap 网卡。
//! - macOS：内核没有 tap，改用 QEMU 8+ 的 `vmnet-bridged` / `vmnet-host`
//!   （vmnet.framework），需要特权。
//!
//! 本模块只做「参数拼装 + 环境探测」，不负责提权：提权要么由用户在安装
//! 驱动时一次性完成，要么由宿主以管理员身份重跑。做不到就给出可操作提示，
//! 而不是让 QEMU 抛一句难懂的原始错误。


use crate::model::{NetworkMode, VmSpec};

/// 一张可选的宿主网卡。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInterface {
    /// 传给 QEMU 的标识：Windows 是「网络连接」里的适配器名，其余是接口名。
    pub id: String,
    /// 展示用名称（含厂商描述时更易辨认）。
    pub label: String,
    /// 该网卡当前的 IPv4 地址，空表示没拿到（未连接 / 无地址）。
    pub ipv4: String,
    /// 是否像一张可供桥接的「物理/主」网卡（无线、有线）。
    /// 虚拟网卡（如 VirtualBox Host-Only）与技术性接口（lo / Loopback）为 false。
    pub bridgeable: bool,
    /// 该网卡是否为无线。桥接无线网卡会有吞吐抖动，前端提示用户优先选有线。
    pub wireless: bool,
}

/// 枚举宿主上网卡。拿不到时返回空列表（前端退化为手动输入）。
pub fn list_interfaces() -> Vec<HostInterface> {
    #[cfg(target_os = "windows")]
    {
        list_interfaces_windows()
    }
    #[cfg(target_os = "macos")]
    {
        list_interfaces_macos()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        list_interfaces_linux()
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        Vec::new()
    }
}

/// Windows：只取「已连接」且有 IPv4 的适配器，过滤掉回环与隧道。
fn list_interfaces_windows() -> Vec<HostInterface> {
    // PowerShell 的 stdout 捕获在部分宿主环境下不可靠，这里让它把结果落到临时文件
    // 再读回来——比直接读管道稳，也顺带避开输出里的编码噪声。
    let temp = std::env::temp_dir().join(format!(
        "otools-vm-nics-{}.json",
        std::process::id()
    ));
    let temp_literal = temp.to_string_lossy().replace('\\', "\\\\");
    // 注意 `(... if ... else ...) | Out-File` 外层这对括号**不能省**：
    // PowerShell 5.1 里把 if 语句直接接管道会报 "空管道元素"（EmptyPipeElement），
    // 整个脚本静默失败、网卡列表变成空。本机实测踩过。
    let script = format!(
        concat!(
            "$out = @(); ",
            "Get-NetIPConfiguration | ",
            "Where-Object {{ $_.NetAdapter.Status -eq 'Up' -and $_.IPv4Address }} | ",
            "ForEach-Object {{ ",
            "$a = $_.NetAdapter; ",
            "$out += [pscustomobject]@{{ ",
            "id = $a.Name; ",
            "label = ($a.InterfaceDescription); ",
            "ipv4 = $_.IPv4Address.IPAddress; ",
            "wireless = ($a.NdisPhysicalMedium -like '*Native80211*') ",
            "}} }}; ",
            "($(if ($out.Count -eq 0) {{ '[]' }} else {{ $out | ConvertTo-Json -Compress }})) | ",
            "Out-File -FilePath '{}' -Encoding utf8"
        ),
        temp_literal
    );
    let ran = crate::process::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !ran {
        let _ = std::fs::remove_file(&temp);
        return Vec::new();
    }
    let text = std::fs::read_to_string(&temp).unwrap_or_default();
    let _ = std::fs::remove_file(&temp);
    // PowerShell 的 utf8 落盘会带 BOM，解析前先剥掉。
    let text = text.trim_start_matches('\u{feff}').trim().to_string();
    if text.is_empty() {
        return Vec::new();
    }
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    // 单张网卡时 ConvertTo-Json 给的是对象而不是数组，两种都接。
    let items: Vec<&serde_json::Value> = match &parsed {
        serde_json::Value::Array(list) => list.iter().collect(),
        serde_json::Value::Null => return Vec::new(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(|value| value.as_str())?.trim().to_string();
            if id.is_empty() {
                return None;
            }
            let ipv4 = item
                .get("ipv4")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let wireless = item
                .get("wireless")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let description = item
                .get("label")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            // 没有地址的网卡桥接过去也是死路，直接不给选。
            if ipv4.is_empty() {
                return None;
            }
            // 虚拟交换机的网卡（VMware / Hyper-V / VirtualBox）桥接过去没有意义：
            // 它们本来就是给别的虚拟化平台用的内部网段。物理网卡才可桥接。
            let lowered = description.to_lowercase();
            let virtual_nic = lowered.contains("vmware")
                || lowered.contains("hyper-v")
                || lowered.contains("virtualbox")
                || lowered.contains("loopback")
                || lowered.contains("bluetooth");
            Some(HostInterface {
                label: if description.is_empty() {
                    id.clone()
                } else {
                    format!("{}（{}）", id, description)
                },
                id,
                ipv4,
                bridgeable: !virtual_nic,
                wireless,
            })
        })
        .collect()
}

/// macOS：`networksetup -listallhardwareports` 拿硬件端口 → 接口名映射，
/// 再按接口名查 IPv4。vmnet-bridged 要的就是 `en0` 这种接口名。
#[cfg(target_os = "macos")]
fn list_interfaces_macos() -> Vec<HostInterface> {
    let output = match crate::process::command("networksetup")
        .args(["-listallhardwareports"])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => return Vec::new(),
    };

    let mut ports: Vec<(String, String, bool)> = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Hardware Port:") {
            pending_name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Device:") {
            if let Some(name) = pending_name.take() {
                let device = rest.trim().to_string();
                // Wi-Fi 桥接会有抖动；先记下来，前端提示优先选有线。
                let wireless = name.to_lowercase().contains("wi-fi")
                    || name.to_lowercase().contains("airport");
                ports.push((device, name, wireless));
            }
        }
    }

    ports
        .into_iter()
        .filter_map(|(device, name, wireless)| {
            let ipv4 = ipv4_for_interface(&device).unwrap_or_default();
            if ipv4.is_empty() {
                return None;
            }
            Some(HostInterface {
                label: format!("{}（{}）", device, name),
                id: device,
                ipv4,
                bridgeable: true,
                wireless,
            })
        })
        .collect()
}

/// Linux：`ip -o -4 addr` 一行一条地址，从中取接口名与地址。
/// 过滤回环与虚拟网桥，留下能桥接的物理网卡。
#[cfg(all(unix, not(target_os = "macos")))]
fn list_interfaces_linux() -> Vec<HostInterface> {
    let output = match crate::process::command("ip")
        .args(["-o", "-4", "addr", "show"])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => return Vec::new(),
    };
    let wireless_names = linux_wireless_names();
    output
        .lines()
        .filter_map(|line| {
            // 形如：2: eth0    inet 192.168.1.5/24 brd ... scope global eth0
            let mut parts = line.split_whitespace();
            parts.next()?; // 序号
            let name = parts.next()?.trim_end_matches(':').to_string();
            let mut ipv4 = String::new();
            let mut rest: Vec<&str> = parts.collect();
            if let Some(position) = rest.iter().position(|part| *part == "inet") {
                if let Some(value) = rest.get(position + 1) {
                    ipv4 = value.split('/').next().unwrap_or_default().to_string();
                }
            }
            rest.clear();
            if name == "lo" || ipv4.is_empty() {
                return None;
            }
            // 已经存在的网桥 / docker 网卡不适合再拿去桥接。
            let is_virtual = name.starts_with("docker")
                || name.starts_with("br-")
                || name.starts_with("virbr")
                || name.starts_with("veth")
                || name.starts_with("vmnet");
            Some(HostInterface {
                label: name.clone(),
                id: name.clone(),
                ipv4,
                bridgeable: !is_virtual,
                wireless: wireless_names.contains(&name),
            })
        })
        .collect()
}

/// Linux 上判断某接口是不是无线的：`/sys/class/net/<name>/wireless` 存在即可。
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_wireless_names() -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return names;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().join("wireless").exists() {
            names.push(name);
        }
    }
    names
}

/// 查某个接口的 IPv4 地址（macOS 走 `ipconfig getifaddr`）。
#[cfg(target_os = "macos")]
fn ipv4_for_interface(device: &str) -> Option<String> {
    let output = crate::process::command("ipconfig").args(["getifaddr", device]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 桥接 / 仅主机在宿主上是否具备基本条件（驱动 / 特权）。
///
/// 返回 `Err(可操作原因)` 时前端直接展示，并用它禁用相关选项。
pub fn check_backend(mode: NetworkMode) -> Result<(), String> {
    if mode.is_user_net() {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let _ = mode;
        // QEMU for Windows 的 tap 后端要求先装 TAP 驱动（tap-windows6），
        // 网卡名再填进 ifname。没有驱动时 QEMU 会报 "Could not open TAP device"。
        if has_windows_tap_driver() {
            Ok(())
        } else {
            Err(
                "未检测到 TAP 虚拟网卡驱动。桥接 / 仅主机需要在宿主安装 tap-windows \
                 （OpenVPN 安装包内置该驱动，可只勾选 TAP 组件），装好后把新网卡与\
                 目标网络桥接，再回到这里选择该网卡。"
                    .to_string(),
            )
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = mode;
        // vmnet.framework 的三种模式都要求特权（com.apple.vm.networking 权限或 root）。
        if unsafe { libc_geteuid_is_root() } {
            Ok(())
        } else {
            Err(
                "vmnet 网络后端需要管理员权限。请以管理员身份重新启动 OTools 后再使用\
                 桥接 / 仅主机网络。"
                    .to_string(),
            )
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = mode;
        if std::path::Path::new("/dev/net/tun").exists() {
            Ok(())
        } else {
            Err("未找到 /dev/net/tun，当前内核不支持 TAP 网络".to_string())
        }
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = mode;
        Err("当前平台不支持桥接 / 仅主机网络".to_string())
    }
}

/// macOS 上没有 libc 依赖，用 `id -u` 判断是否 root（够用）。
#[cfg(target_os = "macos")]
unsafe fn libc_geteuid_is_root() -> bool {
    crate::process::command("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Windows：查网卡驱动是否登记了 TAP 虚拟网卡。
///
/// **不能按服务名猜**：`TapiSrv`（Windows 自带的电话服务）同样以 `tap` 开头，
/// 用 `Services` 下 `^tap` 匹配会把它误认成 TAP 驱动，进而让 UI 放行一个
/// 注定起不来的网络模式。这里改为查网卡类下的适配器实例，按驱动提供者的
/// 描述判断，`TapiSrv` 这类服务不会出现在网卡枚举里。
#[cfg(target_os = "windows")]
fn has_windows_tap_driver() -> bool {
    // 先看有没有以 TAP 命名的网卡实例（最直接的证据）。
    let script = concat!(
        "$nics = Get-NetAdapter -ErrorAction SilentlyContinue | ",
        "Where-Object { $_.InterfaceDescription -match 'TAP-Windows|tap-windows|OpenVPN' }; ",
        "if ($nics) { 'YES' } else { 'NO' }"
    );
    if let Ok(output) = crate::process::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).to_uppercase();
            if text.contains("YES") {
                return true;
            }
        }
    }
    // 兜底：驱动未绑定到网卡时，类 GUID 下可能仍有遗留实例。
    let script = concat!(
        "$cls = '{4d36e972-e325-11ce-bfc1-08002be10318}'; ",
        "$p = \"HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\$cls\"; ",
        "$hits = Get-ChildItem $p -ErrorAction SilentlyContinue | ",
        "Where-Object { $_.GetValue('DriverDesc') -match 'TAP-Windows|OpenVPN' }; ",
        "if ($hits) { 'YES' } else { 'NO' }"
    );
    match crate::process::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_uppercase().contains("YES")
        }
        _ => false,
    }
}

/// 构造网络后端参数（`-netdev` 的值）以及是否需要额外的 `-device` 参数。
///
/// 返回 `(netdev, 附加参数)`。附加参数目前只有 Linux 桥接用的 `-netdev bridge`——
/// 那种情况下 tap 与 bridge 是两条 `-netdev`，所以用 Vec 承载更多自由。
pub fn build_netdev(spec: &VmSpec) -> Result<Vec<String>, String> {
    let mode = spec.network;
    let mut args: Vec<String> = Vec::new();

    match mode {
        NetworkMode::Nat => {
            // 用户态网络（SLIRP）：零配置，端口转发用 hostfwd 表达。
            let mut netdev = "user,id=net0".to_string();
            for rule in &spec.port_forwards {
                netdev.push_str(&format!(
                    ",hostfwd={}:{}:{}-:{}",
                    rule.protocol, rule.bind_host, rule.host_port, rule.guest_port
                ));
            }
            args.push("-netdev".to_string());
            args.push(netdev);
        }
        NetworkMode::Bridged => {
            let iface = spec
                .network_iface
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "桥接网络需要指定宿主网卡".to_string())?;
            build_shared_netdev(mode, iface, true, &mut args)?;
        }
        NetworkMode::HostOnly => {
            let iface = spec
                .network_iface
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            build_shared_netdev(mode, iface, false, &mut args)?;
        }
    }

    Ok(args)
}

/// 桥接 / 仅主机的后端分派。
///
/// `iface` 在 macOS 的仅主机模式下可以为空（vmnet-host 自己造网段）。
fn build_shared_netdev(
    mode: NetworkMode,
    iface: &str,
    bridged: bool,
    args: &mut Vec<String>,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let _ = (bridged, mode);
        // Windows 没有内核网桥原语可调：QEMU 连上一张已桥接好的 TAP 网卡即可，
        // 桥接动作本身由用户在「网络连接」里完成。
        args.push("-netdev".to_string());
        args.push(format!("tap,id=net0,ifname={},script=no,downscript=no", iface));
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = mode;
        // vmnet.framework：桥接指定物理网卡，仅主机自建隔离网段。
        args.push("-netdev".to_string());
        if bridged {
            args.push(format!("vmnet-bridged,id=net0,ifname={}", iface));
        } else {
            args.push("vmnet-host,id=net0".to_string());
        }
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = mode;
        if bridged {
            // Linux 桥接：tap 交给已有的网桥（br=），bridge 后端负责把 tap 挂上去。
            // helper 走默认的 qemu-bridge-helper，它按 /etc/qemu/bridge.conf 授权。
            args.push("-netdev".to_string());
            args.push(format!(
                "bridge,id=net0,br={},helper=/usr/lib/qemu/qemu-bridge-helper",
                iface
            ));
        } else {
            // 仅主机：一张不参与桥接的 tap，QEMU 与客户机自成一段网络。
            // 网段由用户在宿主侧给 tap 配地址（或在客户机里手动配）。
            args.push("-netdev".to_string());
            args.push(format!("tap,id=net0,ifname={},script=no,downscript=no", iface));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        let _ = (mode, iface, bridged, args);
        Err("当前平台不支持桥接 / 仅主机网络".to_string())
    }
}

/// 客户机侧配置提示：不同网络模式下用户该怎么让客户机上外网。
///
/// 返回 `None` 表示该模式开箱可用（NAT 的 DHCP 会自己配好）。
pub fn guest_setup_hint(spec: &VmSpec) -> Option<String> {
    match spec.network {
        NetworkMode::Nat => None,
        NetworkMode::Bridged => Some(
            "桥接模式下客户机会从所在局域网的 DHCP 拿到地址；若局域网没有 DHCP，\
             请在客户机里手动配置同网段的静态地址。"
                .to_string(),
        ),
        NetworkMode::HostOnly => Some(
            "仅主机模式下客户机与宿主自成一段网络，没有外网出口；\
             请在宿主与客户机两侧配置同网段的静态地址。"
                .to_string(),
        ),
    }
}

/// 供前端展示的网络模式标签。
pub fn mode_label(mode: NetworkMode) -> &'static str {
    match mode {
        NetworkMode::Nat => "nat",
        NetworkMode::Bridged => "bridged",
        NetworkMode::HostOnly => "hostOnly",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GuestOs, VmSpec};

    fn spec() -> VmSpec {
        VmSpec {
            id: "vm-1".to_string(),
            name: "t".to_string(),
            os: GuestOs::Linux,
            arch: "x86_64".to_string(),
            cpus: 2,
            memory_mb: 2048,
            disk_path: "/tmp/d.raw".to_string(),
            disk_size_gb: 20,
            installer_path: None,
            firmware: "bios".to_string(),
            firmware_path: None,
            driver_iso: None,
            virtiofsd_path: None,
            network: NetworkMode::Nat,
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
    fn nat_netdev_carries_hostfwd_rules() {
        let mut s = spec();
        s.port_forwards.push(crate::model::PortForward {
            protocol: "tcp".to_string(),
            host_port: 3389,
            guest_port: 3389,
            bind_host: "127.0.0.1".to_string(),
        });
        let args = build_netdev(&s).unwrap();
        assert_eq!(args[0], "-netdev");
        assert_eq!(args[1], "user,id=net0,hostfwd=tcp:127.0.0.1:3389-:3389");
    }

    #[test]
    fn nat_without_rules_stays_bare() {
        // 没配端口转发时，命令行必须与改动前逐字一致（零回归）。
        let args = build_netdev(&spec()).unwrap();
        assert_eq!(args, vec!["-netdev".to_string(), "user,id=net0".to_string()]);
    }

    #[test]
    fn bridged_requires_interface_and_builds_backend() {
        let mut s = spec();
        s.network = NetworkMode::Bridged;
        // 没给网卡：必须报错而不是拼出一条空 ifname 的命令行。
        assert!(build_netdev(&s).is_err());
        s.network_iface = Some("en0".to_string());
        let args = build_netdev(&s).unwrap();
        assert_eq!(args[0], "-netdev");
        // 后端名按平台分派，这里只断言它指向了选定的网卡且带上了 id=net0。
        assert!(args[1].contains("id=net0"), "实际: {}", args[1]);
        assert!(args[1].contains("en0"), "实际: {}", args[1]);
    }

    #[test]
    fn host_only_builds_without_interface_on_macos_semantics() {
        let mut s = spec();
        s.network = NetworkMode::HostOnly;
        // macOS 的 vmnet-host 不需要网卡；其它平台需要。这里只断言不 panic 且有 netdev。
        let args = build_netdev(&s).unwrap();
        assert_eq!(args[0], "-netdev");
        assert!(args[1].contains("id=net0"));
    }

    #[test]
    fn mode_helpers_are_consistent() {
        assert!(!NetworkMode::Nat.needs_interface());
        assert!(NetworkMode::Bridged.needs_interface());
        assert!(NetworkMode::Nat.is_user_net());
        assert!(!NetworkMode::Bridged.is_user_net());
        assert!(!NetworkMode::HostOnly.is_user_net());
        // 仅主机在 macOS 上不需要网卡（vmnet-host 自建网段）。
        let expected = !cfg!(target_os = "macos");
        assert_eq!(NetworkMode::HostOnly.needs_interface(), expected);
    }

    #[test]
    fn nat_always_uses_gateway_address() {
        // NAT 下宿主在客户机里固定是 10.0.2.2 —— SMB 路径靠这个常量成立。
        let s = spec();
        assert_eq!(super::super::smb::host_address_for_guest(&s), "10.0.2.2");
    }
}
