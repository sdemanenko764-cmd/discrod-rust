//! Кэш картинок на стороне интерфейса: текстуры, анимация GIF, вытеснение старого.
//! Картинка запрашивается только когда её место видно на экране,
//! и выгружается, если давно не показывалась.

use std::collections::HashMap;
use std::time::Duration;

use eframe::egui::{self, TextureHandle, TextureId, TextureOptions, Vec2};
use tokio::sync::mpsc::UnboundedSender;

use crate::discord::{Cmd, Frame};

/// Сколько картинок держать одновременно.
const MAX_ENTRIES: usize = 160;

enum Entry {
    Loading,
    Failed,
    Ready {
        frames: Vec<TextureHandle>,
        delays: Vec<u32>,
        total_ms: u32,
        size: Vec2,
    },
}

#[derive(Default)]
pub struct MediaCache {
    entries: HashMap<String, (Entry, f64)>,
}

impl MediaCache {
    /// Текущий кадр картинки. Если её ещё нет — ставит в очередь загрузку.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        cmd: &UnboundedSender<Cmd>,
        key: &str,
        url: &str,
        max_side: u32,
        animate: bool,
    ) -> Option<(TextureId, Vec2)> {
        let now = ctx.input(|i| i.time);
        match self.entries.get_mut(key) {
            Some((entry, used)) => {
                *used = now;
                match entry {
                    Entry::Ready {
                        frames,
                        delays,
                        total_ms,
                        size,
                    } => {
                        let mut index = 0;
                        if frames.len() > 1 && *total_ms > 0 {
                            let mut t = ((now * 1000.0) as u64 % *total_ms as u64) as u32;
                            for (i, d) in delays.iter().enumerate() {
                                if t < *d {
                                    index = i;
                                    break;
                                }
                                t -= d;
                            }
                            ctx.request_repaint_after(Duration::from_millis(
                                delays[index].clamp(20, 200) as u64,
                            ));
                        }
                        Some((frames[index].id(), *size))
                    }
                    _ => None,
                }
            }
            None => {
                self.entries.insert(key.to_string(), (Entry::Loading, now));
                let _ = cmd.send(Cmd::FetchImage {
                    key: key.to_string(),
                    url: url.to_string(),
                    max_side,
                    animate,
                });
                None
            }
        }
    }

    pub fn size_of(&self, key: &str) -> Option<Vec2> {
        match self.entries.get(key) {
            Some((Entry::Ready { size, .. }, _)) => Some(*size),
            _ => None,
        }
    }

    pub fn failed(&self, key: &str) -> bool {
        matches!(self.entries.get(key), Some((Entry::Failed, _)))
    }

    pub fn on_loaded(
        &mut self,
        ctx: &egui::Context,
        key: String,
        result: Result<Vec<Frame>, String>,
    ) {
        let Some((entry, _)) = self.entries.get_mut(&key) else {
            return; // уже выгружена
        };
        *entry = match result {
            Ok(frames) if !frames.is_empty() => {
                let [w, h] = frames[0].image.size;
                let mut delays = Vec::with_capacity(frames.len());
                let mut textures = Vec::with_capacity(frames.len());
                for (i, f) in frames.into_iter().enumerate() {
                    delays.push(f.delay_ms);
                    textures.push(ctx.load_texture(
                        format!("{key}#{i}"),
                        f.image,
                        TextureOptions::LINEAR,
                    ));
                }
                Entry::Ready {
                    total_ms: delays.iter().sum(),
                    frames: textures,
                    delays,
                    size: egui::vec2(w as f32, h as f32),
                }
            }
            _ => Entry::Failed,
        };
    }

    /// Выгружает картинки, которые давно не показывались.
    pub fn evict(&mut self, now: f64) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut by_age: Vec<(String, f64)> = self
            .entries
            .iter()
            .filter(|(_, (_, used))| now - used > 3.0)
            .map(|(k, (_, used))| (k.clone(), *used))
            .collect();
        by_age.sort_by(|a, b| a.1.total_cmp(&b.1));
        let excess = self.entries.len() - MAX_ENTRIES;
        for (key, _) in by_age.into_iter().take(excess + MAX_ENTRIES / 4) {
            self.entries.remove(&key);
        }
    }

    pub fn clear(&mut self) {
        self.entries = HashMap::new();
    }

    /// (картинок, кадров)
    pub fn stats(&self) -> (usize, usize) {
        let frames = self
            .entries
            .values()
            .map(|(e, _)| match e {
                Entry::Ready { frames, .. } => frames.len(),
                _ => 0,
            })
            .sum();
        (self.entries.len(), frames)
    }
}
