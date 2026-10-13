//! FockyTV plugin: https://github.com/FelipeMayerDev/fockytv.
mod backend;
pub mod ui;
pub use backend::*;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub url: String,
    pub nick: Option<String>,
    pub fps: u32,
    pub max_height: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            url: "https://tv.huestavo.com".into(),
            nick: None,
            fps: 60,
            max_height: 0,
        }
    }
}
