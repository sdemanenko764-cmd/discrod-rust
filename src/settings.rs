//! Настройки, сохраняются в %APPDATA%\Rustcord\settings.json

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Theme {
    AdwaitaDark,
    AdwaitaLight,
    Catppuccin,
    Nord,
    Dark,
    Midnight,
    Light,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub accent: usize,
    pub zoom: f32,
    pub compact: bool,
    pub load_images: bool,
    pub animate_gifs: bool,
    pub load_avatars: bool,
    pub show_ram: bool,
    pub cached_channels: usize,
    pub messages_per_channel: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::AdwaitaDark,
            accent: 1,
            zoom: 1.0,
            compact: false,
            load_images: true,
            animate_gifs: true,
            load_avatars: true,
            show_ram: true,
            cached_channels: 6,
            messages_per_channel: 200,
        }
    }
}

pub fn load() -> Settings {
    let mut s: Settings = crate::store::config_path("settings.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    s.zoom = s.zoom.clamp(0.75, 1.6);
    s.cached_channels = s.cached_channels.clamp(2, 20);
    s.messages_per_channel = s.messages_per_channel.clamp(50, 500);
    s
}

pub fn save(s: &Settings) {
    if let Some(p) = crate::store::config_path("settings.json") {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(s) {
            let _ = std::fs::write(p, text);
        }
    }
}
