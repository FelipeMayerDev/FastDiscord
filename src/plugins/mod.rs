//! Built-in plugins own their settings, configuration views and transports.
pub mod fockytv;
pub mod golive_bypass;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id {
    GoLiveBypass,
    FockyTv,
}

impl Id {
    pub const ALL: [Self; 2] = [Self::GoLiveBypass, Self::FockyTv];

    pub fn name(self) -> &'static str {
        match self {
            Self::GoLiveBypass => "GoLiveBypass",
            Self::FockyTv => "FockyTV",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            Self::GoLiveBypass => "https://github.com/bezumiya/GoLiveBypass",
            Self::FockyTv => "https://github.com/FelipeMayerDev/fockytv",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub golive_bypass: golive_bypass::Settings,
    pub fockytv: fockytv::Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareBackend {
    Discord,
    FockyTv,
}

impl Settings {
    pub fn backend(&self, bypass_active: bool) -> ShareBackend {
        if self.fockytv.enabled && !bypass_active {
            ShareBackend::FockyTv
        } else {
            ShareBackend::Discord
        }
    }
}

/// Old installs keep their FockyTV preferences when moving to plugin settings.
pub fn migrate_legacy(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if object.contains_key("plugins") {
        return;
    }
    let mut fockytv = serde_json::Map::new();
    for (old, new) in [
        ("fockytv_share", "enabled"),
        ("fockytv_url", "url"),
        ("fockytv_nick", "nick"),
        ("fockytv_fps", "fps"),
        ("fockytv_max_height", "max_height"),
    ] {
        if let Some(value) = object.get(old) {
            fockytv.insert(new.into(), value.clone());
        }
    }
    object.insert("plugins".into(), serde_json::json!({"fockytv": fockytv}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_overrides_fockytv_only_in_the_tunneled_session() {
        let mut settings = Settings::default();
        assert_eq!(settings.backend(false), ShareBackend::FockyTv);
        settings.golive_bypass.enabled = true;
        assert_eq!(settings.backend(false), ShareBackend::FockyTv);
        assert_eq!(settings.backend(true), ShareBackend::Discord);
        settings.fockytv.enabled = false;
        assert_eq!(settings.backend(false), ShareBackend::Discord);
    }

    #[test]
    fn migrate_preserves_legacy_preferences_and_existing_plugin_settings() {
        let mut value = serde_json::json!({"fockytv_share": false, "fockytv_url": "https://tv.example", "fockytv_nick": "alice", "fockytv_fps": 15, "fockytv_max_height": 720});
        migrate_legacy(&mut value);
        let settings: crate::settings::Settings = serde_json::from_value(value).unwrap();
        assert!(!settings.plugins.fockytv.enabled);
        assert_eq!(settings.plugins.fockytv.url, "https://tv.example");
        assert_eq!(settings.plugins.fockytv.nick.as_deref(), Some("alice"));
        assert_eq!(
            (
                settings.plugins.fockytv.fps,
                settings.plugins.fockytv.max_height
            ),
            (15, 720)
        );
        let mut value = serde_json::json!({"plugins":{"fockytv":{"url":"https://new.example"}},"fockytv_url":"https://old.example"});
        migrate_legacy(&mut value);
        assert_eq!(value["plugins"]["fockytv"]["url"], "https://new.example");
    }
}
