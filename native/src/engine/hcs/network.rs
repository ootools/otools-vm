//! HCN（Host Compute Network）：给 HCS 虚拟机造网卡。
//!
//! HCS 本身不管网络——`Devices.NetworkAdapters` 里只能填一个 `EndpointId`，
//! 那个端点必须先由 HCN 建好。所以「客户机有没有网」完全取决于这一层。
//!
//! ## 为什么 v1 只做 NAT
//!
//! HCN 的网络模式对应关系：
//! - **NAT**：插件可以自建（建 NAT 网络 + 一个端点），不需要宿主预配置。
//! - **透明桥接（Transparent）**：要求宿主已有可桥接的 HCN 网络，需要事先用
//!   `New-VMSwitch` 之类配好，超出「点一下就能用」的范畴。
//! - **Internal / Private**：同理，得先有网络对象。
//!
//! 所以这里实现 NAT：自建网络、自建端点，用完拆掉。其余模式明确返回「不支持」
//! 而不是静默跳过——静默跳过会表现为「机器起来了但没有网」，最难查。
//!
//! ## 端点与网络的生命期
//!
//! 两者必须活到客户机关机之后：先删端点再关机器，HCS 会报设备错误。
//! 因此把它们挂在 `ManagedVm` 上，随机器一起释放。

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;

type Handle = *mut c_void;

const S_OK: i32 = 0;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type FnCreate = unsafe extern "system" fn(*const u16, *const u16, *mut Handle) -> i32;
type FnClose = unsafe extern "system" fn(Handle);
type FnAddEndpoint = unsafe extern "system" fn(Handle, *const u16) -> i32;

struct Api {
    create_network: FnCreate,
    close_network: FnClose,
    create_endpoint: FnCreate,
    close_endpoint: FnClose,
    add_endpoint: FnAddEndpoint,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> &'static Result<Api, String> {
    API.get_or_init(load)
}

/// HCN 是否可用。
pub fn available() -> bool {
    api().is_ok()
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
    let library = wide("computenetwork.dll");
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    if module.is_null() {
        return Err("本机没有 computenetwork.dll：HCS 网络不可用".to_string());
    }
    macro_rules! need {
        ($name:literal) => {{
            match unsafe { symbol(module, $name) } {
                Some(pointer) => unsafe { std::mem::transmute(pointer) },
                None => return Err(format!("computenetwork.dll 缺少符号 {}", $name)),
            }
        }};
    }
    Ok(Api {
        create_network: need!("HcnCreateNetwork"),
        close_network: need!("HcnCloseNetwork"),
        create_endpoint: need!("HcnCreateEndpoint"),
        close_endpoint: need!("HcnCloseEndpoint"),
        add_endpoint: need!("HcnAddEndpoint"),
    })
}

/// 把 HRESULT 翻成可读文案。
fn describe(code: i32) -> String {
    match code as u32 {
        0x0000_0000 => "成功".to_string(),
        0x8007_0005 => "访问被拒绝（需要管理员权限）".to_string(),
        0x803B_0002 => "HCN 网络对象不存在".to_string(),
        0x803B_0005 => "HCN 配置文档无效".to_string(),
        0x803B_0016 => "同名 HCN 对象已存在".to_string(),
        _ => format!("HCN 操作失败（HRESULT 0x{:08X}）", code as u32),
    }
}

/// 从虚拟机 id 派生一个**确定性** GUID。
///
/// 为什么不是随机：HCN 的 id 必须是 GUID 形式，而重启后我们要能重新算出同一个
/// id 才能复用/清理上次的对象。用运行期随机数会逼着我们把 GUID 存进 vm.json，
/// 多一份要维护的状态。这里用 FNV-1a 打散 16 字节——不是密码学哈希，只要求
/// 「同 id 同 GUID、不同 id 极不碰撞」。
pub fn guid_for(prefix: u8, id: &str) -> String {
    // 两个不同种子的 FNV-1a，拼出 16 字节。
    let mut bytes = [0u8; 16];
    let mut first: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in prefix.to_le_bytes().iter().chain(id.as_bytes()) {
        first ^= *byte as u64;
        first = first.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut second: u64 = 0x9e37_79b9_7f4a_7c15 ^ first;
    for byte in id.as_bytes().iter().rev() {
        second ^= *byte as u64;
        second = second.wrapping_mul(0x0000_0100_0000_01b3);
    }
    bytes[..8].copy_from_slice(&first.to_le_bytes());
    bytes[8..].copy_from_slice(&second.to_le_bytes());
    // 标成 v4（随机）变体，避免被当成时间序 GUID 触发额外的排序假设。
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// 网络 id：从虚拟机 id 派生，带固定前缀区分网络与端点。
pub fn network_guid(id: &str) -> String {
    guid_for(0x4e, id) // 'N'
}

/// 端点 id。同一台机器恒得同一个，重启后可复用。
pub fn endpoint_guid(id: &str) -> String {
    guid_for(0x45, id) // 'E'
}

/// 一条 NAT 网络 + 挂在它上面的一个端点。
///
/// Drop 顺序很重要：先删端点再关网络，反过来 HCN 会报「网络仍在使用」。
pub struct NatNetwork {
    api: &'static Api,
    network: Handle,
    endpoint: Handle,
    endpoint_id: String,
}

impl NatNetwork {
    /// 建一条 NAT 网络并挂一个端点。
    pub fn create(id: &str) -> Result<Self, String> {
        let api = match api() {
            Ok(api) => api,
            Err(reason) => return Err(reason.clone()),
        };

        let network_id = network_guid(id);
        let endpoint_id = endpoint_guid(id);

        let mut network: Handle = std::ptr::null_mut();
        let code = unsafe {
            (api.create_network)(
                wide(&network_id).as_ptr(),
                wide(&network_document()).as_ptr(),
                &mut network,
            )
        };
        if code != S_OK {
            return Err(format!("创建 HCN 网络失败：{}", describe(code)));
        }

        let mut endpoint: Handle = std::ptr::null_mut();
        let code = unsafe {
            (api.create_endpoint)(
                wide(&endpoint_id).as_ptr(),
                wide(&endpoint_document(&network_id)).as_ptr(),
                &mut endpoint,
            )
        };
        if code != S_OK {
            unsafe { (api.close_network)(network) };
            return Err(format!("创建 HCN 端点失败：{}", describe(code)));
        }

        // 端点建出来是「孤立」的，必须显式加入网络后才可用于 HCS。
        let code = unsafe { (api.add_endpoint)(network, wide(&endpoint_id).as_ptr()) };
        if code != S_OK {
            unsafe {
                (api.close_endpoint)(endpoint);
                (api.close_network)(network);
            }
            return Err(format!("把端点加入网络失败：{}", describe(code)));
        }

        Ok(Self {
            api,
            network,
            endpoint,
            endpoint_id,
        })
    }

    /// 该端点的 Guid，填进 HCS 的 `NetworkAdapter.EndpointId`。
    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }
}

impl Drop for NatNetwork {
    fn drop(&mut self) {
        if !self.endpoint.is_null() {
            unsafe { (self.api.close_endpoint)(self.endpoint) };
        }
        if !self.network.is_null() {
            unsafe { (self.api.close_network)(self.network) };
        }
    }
}

// ---------------------------------------------------------------- 文档构造

/// NAT 网络的 HCN 文档。
///
/// `Type: "NAT"` 让 HCN 自己维护 NAT 规则，不必调 ICS 或 `New-NetNat`。
/// `Ipams` 留空 = 用默认 NAT 池，避免与宿主已有网段冲突。
fn network_document() -> String {
    serde_json::json!({
        "SchemaVersion": { "Major": 2, "Minor": 0 },
        "Name": "OTools VM NAT",
        "Type": "NAT",
        "Ipams": [],
        "Policies": [],
    })
    .to_string()
}

/// 端点的 HCN 文档：挂到指定网络下，IP 交给 HCN 的 IPAM 分配。
fn endpoint_document(network_id: &str) -> String {
    serde_json::json!({
        "SchemaVersion": { "Major": 2, "Minor": 0 },
        "Name": "OTools VM endpoint",
        "HostComputeNetwork": network_id,
        "Policies": [],
        // 空 IpConfigurations = 由网络的 IPAM 自动分配。
        "IpConfigurations": [],
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nat_is_supported() {
        use crate::model::NetworkMode;
        // 桥接 / 仅主机都要宿主预配置 HCN 网络，v1 不做。
        assert!(crate::engine::hcs::config::network_supported(NetworkMode::Nat));
        assert!(!crate::engine::hcs::config::network_supported(NetworkMode::Bridged));
        assert!(!crate::engine::hcs::config::network_supported(NetworkMode::HostOnly));
    }

    #[test]
    fn guid_is_stable_per_id_and_differs_across_ids() {
        // 关键不变量：同 id 每次算出来必须一样（否则重启后清不掉上次的对象），
        // 不同 id 必须不同（否则两台机器抢同一个端点）。
        assert_eq!(network_guid("vm-a"), network_guid("vm-a"));
        assert_eq!(endpoint_guid("vm-a"), endpoint_guid("vm-a"));
        assert_ne!(network_guid("vm-a"), network_guid("vm-b"));
        assert_ne!(network_guid("vm-a"), endpoint_guid("vm-a"));
    }

    #[test]
    fn guid_is_well_formed() {
        // HCN 只接受标准 GUID 文本：8-4-4-4-12 的十六进制。
        let text = endpoint_guid("abc");
        assert_eq!(text.len(), 36, "{}", text);
        let parts: Vec<&str> = text.split('-').collect();
        assert_eq!(parts.len(), 5, "{}", text);
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
        assert!(text.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{}", text);
        // v4 变体位：第 3 段首字符必须是 4，第 4 段首字符落在 8/9/a/b。
        assert!(parts[2].starts_with('4'), "{}", text);
        assert!(matches!(parts[3].chars().next().unwrap(), '8' | '9' | 'a' | 'b'), "{}", text);
    }

    #[test]
    fn network_document_is_nat_and_valid_json() {
        let value: serde_json::Value = serde_json::from_str(&network_document()).unwrap();
        assert_eq!(value["Type"], "NAT");
        assert_eq!(value["SchemaVersion"]["Major"], 2);
        // Ipams 留空 = 由 HCN 分配网段，避免与宿主已有网段冲突。
        assert!(value["Ipams"].as_array().unwrap().is_empty());
    }

    #[test]
    fn endpoint_document_points_at_its_network() {
        let value: serde_json::Value = serde_json::from_str(&endpoint_document("net-x")).unwrap();
        assert_eq!(value["HostComputeNetwork"], "net-x");
        assert!(value["IpConfigurations"].as_array().unwrap().is_empty());
    }
}
