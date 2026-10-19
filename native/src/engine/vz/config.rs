//! 由 `VmSpec` 构造 `VZVirtualMachineConfiguration`。
//!
//! 设备组合对齐 UTM / Parallels 的「开箱可用」基线：
//! EFI 引导 + virtio-blk 系统盘 + virtio-net(NAT) + virtio-gpu + virtio-entropy
//! + virtio-snd + virtio-fs 共享目录 + SPICE agent 剪贴板。

use crate::model::{GuestOs, VmSpec};
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSString, NSURL};
use objc2_virtualization::{
    VZAudioDeviceConfiguration, VZDirectoryShare, VZDirectorySharingDeviceConfiguration,
    VZDiskImageStorageDeviceAttachment, VZEFIBootLoader, VZEFIVariableStore,
    VZEFIVariableStoreInitializationOptions, VZEntropyDeviceConfiguration,
    VZGenericMachineIdentifier, VZGenericPlatformConfiguration,
    VZGraphicsDeviceConfiguration, VZHostAudioOutputStreamSink, VZKeyboardConfiguration,
    VZConsoleDeviceConfiguration, VZLinuxRosettaAvailability,
    VZLinuxRosettaDirectoryShare,
    VZMACAddress,
    VZMultipleDirectoryShare, VZNATNetworkDeviceAttachment, VZNetworkDeviceConfiguration,
    VZPlatformConfiguration, VZPointingDeviceConfiguration, VZSerialPortAttachment,
    VZSharedDirectory,
    VZSpiceAgentPortAttachment, VZStorageDeviceConfiguration, VZUSBKeyboardConfiguration,
    VZUSBScreenCoordinatePointingDeviceConfiguration, VZVirtualMachineConfiguration,
    VZVirtioBlockDeviceConfiguration, VZVirtioConsoleDeviceConfiguration,
    VZVirtioConsolePortConfiguration,
    VZVirtioEntropyDeviceConfiguration, VZVirtioFileSystemDeviceConfiguration,
    VZVirtioGraphicsDeviceConfiguration, VZVirtioGraphicsScanoutConfiguration,
    VZVirtioNetworkDeviceConfiguration, VZVirtioSoundDeviceConfiguration,
    VZVirtioSoundDeviceOutputStreamConfiguration, VZVirtioSoundDeviceStreamConfiguration,
};

/// 多目录共享时统一的 virtiofs 标签。
const PRIMARY_SHARE_TAG: &str = "share";

/// Rosetta 在 Linux 客户机里的固定挂载标签，客户机据此挂载并注册 binfmt。
pub const ROSETTA_SHARE_TAG: &str = "rosetta";

/// 子类转父类引用（objc2 的 `extern_class!` 为继承链生成 `Deref`）。
fn up<'a, T, U>(value: &'a Retained<T>) -> &'a U
where
    T: std::ops::Deref<Target = U> + ?Sized,
    U: ?Sized,
{
    value
}

pub fn ns_error_text(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

/// 把 VZ 的校验报错翻译成用户能行动的话。
///
/// 最常见的一条是权限缺失：`com.apple.security.virtualization` 属于**进程**，
/// 插件动态库无法自带，必须由宿主主程序在签名时申请。用户看到原始英文
/// 「The process doesn't have the … entitlement」只会一头雾水。
fn explain_validate_error(error: &NSError) -> String {
    let text = ns_error_text(error);
    if text.contains("com.apple.security.virtualization") {
        return "宿主程序缺少 com.apple.security.virtualization 权限，无法创建虚拟机。\
               请在 OTools 的 src-tauri/entitlements.plist 里加上该键并重新构建安装包。"
            .to_string();
    }
    format!("虚拟机配置校验失败：{}", text)
}

fn file_url(path: &str) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(path))
}

fn build_variable_store(nvram_path: &str) -> Result<Retained<VZEFIVariableStore>, String> {
    let url = file_url(nvram_path);
    if std::path::Path::new(nvram_path).exists() {
        Ok(unsafe { VZEFIVariableStore::initWithURL(VZEFIVariableStore::alloc(), &url) })
    } else {
        if let Some(parent) = std::path::Path::new(nvram_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        unsafe {
            VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
                VZEFIVariableStore::alloc(),
                &url,
                VZEFIVariableStoreInitializationOptions::AllowOverwrite,
            )
        }
        .map_err(|error| format!("创建 EFI 变量存储失败：{}", ns_error_text(&error)))
    }
}

fn block_device(path: &str, read_only: bool) -> Result<Retained<VZVirtioBlockDeviceConfiguration>, String> {
    let url = file_url(path);
    let attachment = unsafe {
        VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url,
            read_only,
        )
    }
    .map_err(|error| format!("挂载磁盘失败 {}：{}", path, ns_error_text(&error)))?;
    Ok(unsafe {
        VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            &attachment,
        )
    })
}

/// `build` 的产物：配置 + 一批必须与虚拟机同生共死的设备对象。
///
/// VZ 不在 `setXxxDevices:` 里强留所有中间对象：`VZSpiceAgentPortAttachment`
/// 这类「附件」被 console port 引用后，如果宿主提前把它释放，VZ 在
/// `_synchronousConfigurationForVirtualMachine` 里再读 `_attachment` 就是野指针，
/// 主线程直接 SIGSEGV（实测 KERN_INVALID_ADDRESS 0x20）。
/// 所以把这些对象随配置一起交出去，由调用方持有到 machine 销毁。
pub struct BuiltConfig {
    pub config: Retained<VZVirtualMachineConfiguration>,
    /// 只为延长生命周期，不参与任何逻辑。
    pub retained: Vec<Retained<objc2::runtime::AnyObject>>,
}

/// 读写「构建期必须保持稳定的身份」：通用平台 machineIdentifier 与网卡 MAC。
///
/// 为什么必须持久化：VZ 在每次构建配置时都为这两者**随机生成新值**（实测
/// MAC 三次构建三个值、machineIdentifier 70 字节随机）。`restoreMachineStateFromURL`
/// 会校验「当前配置」与「存档时配置」是否兼容，身份一变就报
/// `VZErrorRestore ... invalid argument`——所以不持久化的话，存档**永远**无法恢复。
fn load_identity(vm_id: &str) -> (Retained<VZGenericMachineIdentifier>, Retained<VZMACAddress>) {
    let dir = crate::paths::vm_dir(vm_id);
    let platform_path = crate::paths::vm_machine_id_path(vm_id);
    let mac_path = crate::paths::vm_mac_path(vm_id);

    let machine_id = std::fs::read(&platform_path)
        .ok()
        .and_then(|bytes| {
            let data = NSData::with_bytes(&bytes);
            unsafe { VZGenericMachineIdentifier::initWithDataRepresentation(
                VZGenericMachineIdentifier::alloc(),
                &data,
            ) }
        })
        .unwrap_or_else(|| {
            let fresh = unsafe { VZGenericMachineIdentifier::init(VZGenericMachineIdentifier::alloc()) };
            unsafe {
                let data = fresh.dataRepresentation();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&platform_path, data.to_vec());
            }
            fresh
        });

    let mac = std::fs::read_to_string(&mac_path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .and_then(|text| unsafe {
            VZMACAddress::initWithString(VZMACAddress::alloc(), &NSString::from_str(&text))
        })
        .unwrap_or_else(|| {
            let fresh = unsafe { VZMACAddress::randomLocallyAdministeredAddress() };
            unsafe {
                let text = fresh.string().to_string();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&mac_path, text);
            }
            fresh
        });

    (machine_id, mac)
}

/// 构造完整配置。**必须在主线程调用**（VZ 对象不是 Send）。
pub fn build(spec: &VmSpec, nvram_path: &str) -> Result<BuiltConfig, String> {
    spec.validate()?;

    if !std::path::Path::new(&spec.disk_path).exists() {
        return Err(format!("系统盘不存在：{}", spec.disk_path));
    }

    let config = unsafe { VZVirtualMachineConfiguration::new() };

    // ---- 平台身份：必须持久化，否则存档无法跨会话恢复 ----
    let (machine_id, mac) = load_identity(&spec.id);
    let platform = unsafe { VZGenericPlatformConfiguration::new() };
    unsafe {
        platform.setMachineIdentifier(&machine_id);
        let platform_ref: &VZPlatformConfiguration = up(&platform);
        config.setPlatform(platform_ref);
    }

    // ---- 引导：EFI（ARM Windows / Linux 通用）----
    let store = build_variable_store(nvram_path)?;
    let boot_loader = unsafe { VZEFIBootLoader::new() };
    unsafe {
        boot_loader.setVariableStore(Some(&store));
        config.setBootLoader(Some(&boot_loader));
    }

    // ---- 计算资源 ----
    unsafe {
        config.setCPUCount(spec.cpus as usize);
        config.setMemorySize(spec.memory_mb * 1024 * 1024);
    }

    // ---- 存储：系统盘 + 可选安装介质 ----
    let mut disks: Vec<Retained<VZVirtioBlockDeviceConfiguration>> = Vec::new();
    disks.push(block_device(&spec.disk_path, false)?);
    if let Some(installer) = spec.installer_path.as_deref() {
        if !installer.trim().is_empty() {
            if !std::path::Path::new(installer).exists() {
                return Err(format!("安装介质不存在：{}", installer));
            }
            disks.push(block_device(installer, true)?);
        }
    }
    let disk_refs: Vec<&VZStorageDeviceConfiguration> = disks.iter().map(up).collect();
    unsafe { config.setStorageDevices(&NSArray::from_slice(&disk_refs)) };

    // ---- 网络：NAT ----
    // MAC 必须是持久化的固定值（随身份一起读写）：VZ 默认每次构建都随机生成，
    // 那样跨进程 restore 会因配置不兼容而失败（见 load_identity 注释）。
    let network = unsafe { VZVirtioNetworkDeviceConfiguration::new() };
    let attachment = unsafe { VZNATNetworkDeviceAttachment::new() };
    unsafe {
        network.setAttachment(Some(&attachment));
        network.setMACAddress(&mac);
        let device: &VZNetworkDeviceConfiguration = up(&network);
        config.setNetworkDevices(&NSArray::from_slice(&[device]));
    }

    // ---- 图形：virtio-gpu，单 scanout ----
    let scanout = unsafe {
        VZVirtioGraphicsScanoutConfiguration::initWithWidthInPixels_heightInPixels(
            VZVirtioGraphicsScanoutConfiguration::alloc(),
            spec.display_width as isize,
            spec.display_height as isize,
        )
    };
    let graphics = unsafe { VZVirtioGraphicsDeviceConfiguration::new() };
    unsafe {
        graphics.setScanouts(&NSArray::from_retained_slice(&[scanout]));
        let device: &VZGraphicsDeviceConfiguration = up(&graphics);
        config.setGraphicsDevices(&NSArray::from_slice(&[device]));
    }

    // ---- 熵源：缺了它客户机首次启动会卡在 entropy starvation ----
    let entropy = unsafe { VZVirtioEntropyDeviceConfiguration::new() };
    unsafe {
        let device: &VZEntropyDeviceConfiguration = up(&entropy);
        config.setEntropyDevices(&NSArray::from_slice(&[device]));
    }

    // ---- 输入设备 ----
    let keyboard = unsafe { VZUSBKeyboardConfiguration::new() };
    let pointing = unsafe { VZUSBScreenCoordinatePointingDeviceConfiguration::new() };
    unsafe {
        let keyboard_ref: &VZKeyboardConfiguration = up(&keyboard);
        let pointing_ref: &VZPointingDeviceConfiguration = up(&pointing);
        config.setKeyboards(&NSArray::from_slice(&[keyboard_ref]));
        config.setPointingDevices(&NSArray::from_slice(&[pointing_ref]));
    }

    // ---- 声卡 ----
    if spec.audio {
        let sink = unsafe { VZHostAudioOutputStreamSink::new() };
        let stream = unsafe { VZVirtioSoundDeviceOutputStreamConfiguration::new() };
        let sound = unsafe { VZVirtioSoundDeviceConfiguration::new() };
        unsafe {
            stream.setSink(Some(&sink));
            let stream_ref: &VZVirtioSoundDeviceStreamConfiguration = up(&stream);
            sound.setStreams(&NSArray::from_slice(&[stream_ref]));
            let audio_ref: &VZAudioDeviceConfiguration = up(&sound);
            config.setAudioDevices(&NSArray::from_slice(&[audio_ref]));
        }
    }

    // ---- 共享目录：virtio-fs ----
    let mut share_devices: Vec<Retained<VZDirectorySharingDeviceConfiguration>> = Vec::new();

    if !spec.shared_dirs.is_empty() {
        let mut keys: Vec<Retained<NSString>> = Vec::new();
        let mut values: Vec<Retained<VZSharedDirectory>> = Vec::new();
        for dir in &spec.shared_dirs {
            let url = file_url(&dir.host_path);
            let shared = unsafe {
                VZSharedDirectory::initWithURL_readOnly(
                    VZSharedDirectory::alloc(),
                    &url,
                    dir.read_only,
                )
            };
            keys.push(NSString::from_str(&dir.mount_tag));
            values.push(shared);
        }
        let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
        let dictionary = NSDictionary::from_retained_objects(&key_refs, &values);
        let multi = unsafe {
            VZMultipleDirectoryShare::initWithDirectories(
                VZMultipleDirectoryShare::alloc(),
                &dictionary,
            )
        };
        let tag = if spec.shared_dirs.len() == 1 {
            spec.shared_dirs[0].mount_tag.as_str()
        } else {
            PRIMARY_SHARE_TAG
        };
        let fs = unsafe {
            VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &NSString::from_str(tag),
            )
        };
        let share_ref: &VZDirectoryShare = up(&multi);
        unsafe {
            fs.setShare(Some(share_ref));
            // 子类 → 父类容器：VZDirectorySharingDeviceConfiguration 是 fs 的父类。
            share_devices.push(Retained::cast_unchecked(fs));
        }
    }

    // ---- Rosetta：让 Linux 客户机直接跑 x86_64 二进制 ----
    //
    // 这是 Parallels / UTM 在 Apple 芯片上最有价值的能力之一：Steam 上的 x86_64
    // 游戏、只有 amd64 构建的 CLI 工具，不用换客户机架构就能跑。
    // Rosetta 目录共享走的是 virtiofs 挂载，但只有 Linux 客户机会去用它。
    if spec.rosetta && spec.os == GuestOs::Linux {
        if unsafe { VZLinuxRosettaDirectoryShare::availability() }
            != VZLinuxRosettaAvailability::Installed
        {
            return Err(
                "Rosetta 尚未安装。首次使用需要在终端执行 softwareupdate --install-rosetta"
                    .to_string(),
            );
        }
        let rosetta = unsafe { VZLinuxRosettaDirectoryShare::initWithError(VZLinuxRosettaDirectoryShare::alloc()) }
            .map_err(|error| format!("初始化 Rosetta 目录共享失败：{}", ns_error_text(&error)))?;
        let fs = unsafe {
            VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &NSString::from_str(ROSETTA_SHARE_TAG),
            )
        };
        let share_ref: &VZDirectoryShare = up(&rosetta);
        unsafe {
            fs.setShare(Some(share_ref));
            // 子类 → 父类容器：VZDirectorySharingDeviceConfiguration 是 fs 的父类。
            share_devices.push(Retained::cast_unchecked(fs));
        }
    }

    if !share_devices.is_empty() {
        let refs: Vec<&VZDirectorySharingDeviceConfiguration> =
            share_devices.iter().map(|item| &**item).collect();
        unsafe { config.setDirectorySharingDevices(&NSArray::from_slice(&refs)) };
    }

    // ---- 剪贴板：SPICE agent 走 virtio-console ----
    //
    // ⚠️ SPICE agent 必须挂在 **console device 的 port** 上，不是 serial port。
    // 挂到 `VZVirtioConsoleDeviceSerialPortConfiguration` 上不会立刻报错，
    // 但 VZ 之后重建配置时 `_attachment` 是 nil，主线程直接 SIGSEGV
    // （KERN_INVALID_ADDRESS 0x20，堆栈落在 -[VZSpiceAgentPortAttachment _attachment]）。
    // 实测：换成 file-handle 附件就正常，说明问题在「容器用错」而不是对象生命周期。
    let mut console_ports: Vec<Retained<VZVirtioConsolePortConfiguration>> = Vec::new();
    let mut retained: Vec<Retained<objc2::runtime::AnyObject>> = Vec::new();
    // 平台对象与身份标识也一并持有：VZ 在 start/restore 之后仍会回读配置里的
    // platform，提前释放同样会变成野指针（与 SPICE agent 同一类问题）。
    unsafe {
        retained.push(Retained::cast_unchecked(machine_id.clone()));
        retained.push(Retained::cast_unchecked(mac.clone()));
        retained.push(Retained::cast_unchecked(platform.clone()));
    }
    if spec.clipboard {
        let spice = unsafe { VZSpiceAgentPortAttachment::new() };
        let port = unsafe { VZVirtioConsolePortConfiguration::new() };
        unsafe {
            spice.setSharesClipboard(true);
            let attachment_ref: &VZSerialPortAttachment = up(&spice);
            port.setAttachment(Some(attachment_ref));
            // 端口名必须是 VZ 约定的那个，客户机的 vdagent 按名字找端口。
            port.setName(Some(&VZSpiceAgentPortAttachment::spiceAgentPortName()));
            console_ports.push(port);
            retained.push(
                Retained::retain(&*spice as *const VZSpiceAgentPortAttachment as *mut _)
                    .expect("retain 不会返回 None"),
            );
        }
    }
    if !console_ports.is_empty() {
        let device = unsafe { VZVirtioConsoleDeviceConfiguration::new() };
        unsafe {
            // `ports` 没有 setter，返回的就是设备自己的可变数组，直接往里塞。
            let ports = device.ports();
            for (index, port) in console_ports.iter().enumerate() {
                ports.setObject_atIndexedSubscript(Some(&**port), index);
            }
            let device_ref: &VZConsoleDeviceConfiguration = up(&device);
            config.setConsoleDevices(&NSArray::from_slice(&[device_ref]));
        }
        retained.push(unsafe {
            Retained::retain(&*device as *const VZVirtioConsoleDeviceConfiguration as *mut _)
                .expect("retain 不会返回 None")
        });
    }

    unsafe { config.validateWithError() }.map_err(|error| explain_validate_error(&error))?;

    Ok(BuiltConfig { config, retained })
}
