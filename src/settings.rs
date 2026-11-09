//! Настройки, сохраняются в %APPDATA%\Rustcord\settings.json

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Theme {
    Aurora,
    Sunset,
    Ocean,
    Neon,
    AdwaitaDark,
    AdwaitaLight,
    Catppuccin,
    Nord,
    Dark,
    Midnight,
    Light,
    Classic,
    TokyoNight,
    Dracula,
    RosePine,
    Gruvbox,
    Sakura,
    Mint,
    Lavender,
    Forest,
    Cosmos,
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
    pub animate_background: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub mic_threshold: f32,
    pub always_transmit: bool,
    pub echo_guard: bool,
    pub stream_monitor: usize,
    pub stream_height: u32,
    pub stream_fps: u32,
    /// Качество трансляции: 0 — экономно, 1 — баланс, 2 — чётко.
    pub stream_quality: u8,
    pub notifications: bool,
    pub status: String,
    pub custom_status: String,
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
            animate_background: true,
            input_device: None,
            output_device: None,
            mic_threshold: 0.006,
            always_transmit: false,
            echo_guard: true,
            stream_monitor: 0,
            stream_height: 720,
            stream_fps: 30,
            stream_quality: 1,
            notifications: true,
            status: "online".into(),
            custom_status: String::new(),
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
    s.mic_threshold = s.mic_threshold.clamp(0.001, 0.08);
    s.stream_height = s.stream_height.clamp(360, 1440);
    s.stream_fps = s.stream_fps.clamp(5, 60);
    s.stream_quality = s.stream_quality.min(2);
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
