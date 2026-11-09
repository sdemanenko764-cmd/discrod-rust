//! Настройки, сохраняются в %APPDATA%\Rustcord\settings.json

use std::collections::{HashMap, HashSet};

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
    /// Шрифт интерфейса: встроенный Inter (иначе системный Segoe UI).
    pub font_inter: bool,
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
    /// Эхоподавление WebRTC AEC3.
    pub echo_cancel: bool,
    /// Шумоподавление: 0 — выкл, 1 — стандартное (WebRTC), 2 — усиленное (RNNoise).
    pub noise: u8,
    /// Автоматическая регулировка усиления микрофона.
    pub agc: bool,
    /// Ручное усиление микрофона (0..3).
    pub input_gain: f32,
    /// Рация (Push-to-Talk).
    pub ptt: bool,
    /// Клавиша рации: виртуальный код Windows (VK_*).
    pub ptt_key: u32,
    /// Общая громкость собеседников (0..2).
    pub output_volume: f32,
    /// Громкость отдельных собеседников: id → 0..2 (0 — заглушён).
    pub user_volumes: HashMap<u64, f32>,
    /// Заглушённые серверы и каналы: без уведомлений и меток непрочитанного.
    pub muted: HashSet<u64>,
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
            font_inter: true,
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
            echo_cancel: true,
            noise: 2,
            agc: true,
            input_gain: 1.0,
            ptt: false,
            ptt_key: 0xC0,
            output_volume: 1.0,
            user_volumes: HashMap::new(),
            muted: HashSet::new(),
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
    s.noise = s.noise.min(2);
    s.input_gain = s.input_gain.clamp(0.0, 3.0);
    s.output_volume = s.output_volume.clamp(0.0, 2.0);
    for v in s.user_volumes.values_mut() {
        *v = v.clamp(0.0, 2.0);
    }
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
