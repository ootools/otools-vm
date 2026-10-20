//! 插件本地设置。落在 `~/.otools/local/vm/settings.json`。
//!
//! 目前只有一个选项：引擎偏好。之所以需要它，是因为 HCS 与 QEMU 的取舍
//! 无法自动决断——HCS 性能更好（CPU 由 Hyper-V 直接执行），但**拿不到客户机
//! 画面**。这种「更好但少一个功能」的选择必须交给用户，不能替他决定。
//!
//! 默认值刻意选 `auto` = QEMU：画面是虚拟机最基本的能力，宁可慢一点也不该
//! 让用户建好机器后什么都看不见。想用 HCS 的用户可以在设置里显式开启。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 引擎偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EnginePreference {
    /// 按平台默认：macOS 用 VZ，其它用 QEMU。HCS 不参与。
    #[default]
    Auto,
    /// 优先 HCS（仅 Windows，且需管理员权限）。拿不到就回落 QEMU。
    Hcs,
    /// 明确只用 QEMU，即使 HCS 可用。
    Qemu,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub engine_preference: EnginePreference,
}

fn settings_path() -> PathBuf {
    crate::paths::vm_root_dir().join("settings.json")
}

/// 读取设置。文件不存在或损坏时返回默认值——设置读不出来不该让插件起不来。
pub fn load() -> Settings {
    let path = settings_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        crate::paths::ensure_dir(parent)?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| format!("写入设置失败 {}: {}", path.display(), error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preference_keeps_qemu_so_display_works() {
        // 这是刻意的产品决定：HCS 没有画面，不能当默认。
        assert_eq!(EnginePreference::default(), EnginePreference::Auto);
    }

    #[test]
    fn preference_round_trips_through_json() {
        let settings = Settings {
            engine_preference: EnginePreference::Hcs,
        };
        let text = serde_json::to_string(&settings).unwrap();
        assert!(text.contains("hcs"), "序列化应当用小驼峰值：{}", text);
        let parsed: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.engine_preference, EnginePreference::Hcs);
    }

    #[test]
    fn unknown_or_broken_settings_fall_back_to_default() {
        // 损坏的 JSON：按默认处理，不 panic。
        assert!(serde_json::from_str::<Settings>("{ not json").is_err());
        // 未知字段：忽略，仍然能读。
        let parsed: Settings =
            serde_json::from_str(r#"{"enginePreference":"auto","futureField":1}"#).unwrap();
        assert_eq!(parsed.engine_preference, EnginePreference::Auto);
    }
}
