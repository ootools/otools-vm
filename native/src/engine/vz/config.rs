//! 由 `VmSpec` 构造 `VZVirtualMachineConfiguration`。
//!
//! 设备组合对齐 UTM / Parallels 的「开箱可用」基线：
//! EFI 引导 + virtio-blk 系统盘 + virtio-net(NAT) + virtio-gpu + virtio-entropy
//! + virtio-snd + virtio-fs 共享目录 + SPICE agent 剪贴板。

use crate::model::VmSpec;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSDictionary, NSError, NSString, NSURL};
use objc2_virtualization::{
    VZAudioDeviceConfiguration, VZDirectoryShare, VZDirectorySharingDeviceConfiguration,
    VZDiskImageStorageDeviceAttachment, VZEFIBootLoader, VZEFIVariableStore,
    VZEFIVariableStoreInitializationOptions, VZEntropyDeviceConfiguration,
    VZGraphicsDeviceConfiguration, VZHostAudioOutputStreamSink, VZKeyboardConfiguration,
    VZMultipleDirectoryShare, VZNATNetworkDeviceAttachment, VZNetworkDeviceConfiguration,
    VZPointingDeviceConfiguration, VZSerialPortAttachment, VZSerialPortConfiguration, VZSharedDirectory,
    VZSpiceAgentPortAttachment, VZStorageDeviceConfiguration, VZUSBKeyboardConfiguration,
    VZUSBScreenCoordinatePointingDeviceConfiguration, VZVirtualMachineConfiguration,
    VZVirtioBlockDeviceConfiguration, VZVirtioConsoleDeviceSerialPortConfiguration,
    VZVirtioEntropyDeviceConfiguration, VZVirtioFileSystemDeviceConfiguration,
    VZVirtioGraphicsDeviceConfiguration, VZVirtioGraphicsScanoutConfiguration,
    VZVirtioNetworkDeviceConfiguration, VZVirtioSoundDeviceConfiguration,
    VZVirtioSoundDeviceOutputStreamConfiguration, VZVirtioSoundDeviceStreamConfiguration,
};

/// 多目录共享时统一的 virtiofs 标签。
const PRIMARY_SHARE_TAG: &str = "share";

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

/// 构造完整配置。**必须在主线程调用**（VZ 对象不是 Send）。
pub fn build(
    spec: &VmSpec,
    nvram_path: &str,
) -> Result<Retained<VZVirtualMachineConfiguration>, String> {
    spec.validate()?;

    if !std::path::Path::new(&spec.disk_path).exists() {
        return Err(format!("系统盘不存在：{}", spec.disk_path));
    }

    let config = unsafe { VZVirtualMachineConfiguration::new() };

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
    let network = unsafe { VZVirtioNetworkDeviceConfiguration::new() };
    let attachment = unsafe { VZNATNetworkDeviceAttachment::new() };
    unsafe {
        network.setAttachment(Some(&attachment));
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
        unsafe {
            let share_ref: &VZDirectoryShare = up(&multi);
            fs.setShare(Some(share_ref));
            let device: &VZDirectorySharingDeviceConfiguration = up(&fs);
            config.setDirectorySharingDevices(&NSArray::from_slice(&[device]));
        }
    }

    // ---- 剪贴板：SPICE agent 走 virtio-console ----
    // 客户机需要 spice-vdagent（Linux）或 spice-guest-tools（Windows）才能真正生效。
    if spec.clipboard {
        let spice = unsafe { VZSpiceAgentPortAttachment::new() };
        let port = unsafe { VZVirtioConsoleDeviceSerialPortConfiguration::new() };
        unsafe {
            spice.setSharesClipboard(true);
            let attachment_ref: &VZSerialPortAttachment = up(&spice);
            port.setAttachment(Some(attachment_ref));
            let port_ref: &VZSerialPortConfiguration = up(&port);
            config.setSerialPorts(&NSArray::from_slice(&[port_ref]));
        }
    }

    unsafe { config.validateWithError() }
        .map_err(|error| format!("虚拟机配置校验失败：{}", ns_error_text(&error)))?;

    Ok(config)
}
