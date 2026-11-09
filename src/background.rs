//! Свой фон окна: картинка или анимированный GIF под полупрозрачными панелями.
//!
//! Файл декодируется в фоновом потоке и уменьшается: для GIF — не больше 720 px
//! по длинной стороне и 120 кадров, для картинки — 1600 px. С включённым размытием
//! кадры хранятся совсем маленькими (240 px) и растягиваются видеокартой —
//! это и даёт мягкое размытие почти бесплатно. После загрузки в видеопамять
//! копии кадров в ОЗУ не остаются.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, pos2, Color32, Rect};
use image::AnimationDecoder;

/// Декодированный кадр до загрузки в видеопамять.
struct RawFrame {
    image: egui::ColorImage,
    delay_ms: u32,
}

enum State {
    Empty,
    Loading,
    Raw(Vec<RawFrame>),
    Ready {
        frames: Vec<(egui::TextureHandle, u32)>,
        total_ms: u64,
    },
    Failed(String),
}

pub struct Background {
    path: Option<PathBuf>,
    blur: bool,
    state: Arc<Mutex<State>>,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            path: None,
            blur: false,
            state: Arc::new(Mutex::new(State::Empty)),
        }
    }
}

const MAX_GIF_FRAMES: usize = 120;

fn decode(path: &Path, blur: bool) -> Result<Vec<RawFrame>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("не удалось прочитать файл: {e}"))?;
    let format = image::guess_format(&bytes).map_err(|e| format!("неизвестный формат: {e}"))?;
    let limit_still = if blur { 240 } else { 1600 };
    let limit_anim = if blur { 240 } else { 720 };
    let to_color = |img: image::RgbaImage, max_side: u32| {
        let (w, h) = img.dimensions();
        let scale = (max_side as f32 / w.max(h) as f32).min(1.0);
        let img = if scale < 1.0 {
            image::imageops::resize(
                &img,
                ((w as f32 * scale) as u32).max(1),
                ((h as f32 * scale) as u32).max(1),
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let size = [img.width() as usize, img.height() as usize];
        egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())
    };
    if format == image::ImageFormat::Gif {
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes))
            .map_err(|e| format!("GIF не читается: {e}"))?;
        let mut out = Vec::new();
        for frame in decoder.into_frames().take(MAX_GIF_FRAMES) {
            let frame = frame.map_err(|e| format!("кадр GIF не читается: {e}"))?;
            let (num, den) = frame.delay().numer_denom_ms();
            let delay_ms = if den == 0 { 100 } else { (num / den).max(20) };
            out.push(RawFrame {
                image: to_color(frame.into_buffer(), limit_anim),
                delay_ms,
            });
        }
        if out.is_empty() {
            return Err("в GIF нет кадров".into());
        }
        return Ok(out);
    }
    let img = image::load_from_memory_with_format(&bytes, format)
        .map_err(|e| format!("картинка не читается: {e}"))?
        .to_rgba8();
    Ok(vec![RawFrame {
        image: to_color(img, limit_still),
        delay_ms: 0,
    }])
}

impl Background {
    /// Синхронизирует фон с настройками: при смене файла или размытия — перезагрузка.
    pub fn sync(&mut self, ctx: &egui::Context, path: Option<&str>, blur: bool) {
        let path = path.filter(|p| !p.trim().is_empty()).map(PathBuf::from);
        if path == self.path && blur == self.blur {
            return;
        }
        self.path = path.clone();
        self.blur = blur;
        let state = Arc::new(Mutex::new(State::Empty));
        self.state = state.clone();
        let Some(path) = path else { return };
        if let Ok(mut s) = state.lock() {
            *s = State::Loading;
        }
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("background".into())
            .spawn(move || {
                let result = decode(&path, blur);
                if let Ok(mut s) = state.lock() {
                    *s = match result {
                        Ok(frames) => State::Raw(frames),
                        Err(e) => {
                            crate::store::log(&format!("фон: {e}"));
                            State::Failed(e)
                        }
                    };
                }
                ctx.request_repaint();
            });
    }

    pub fn active(&self) -> bool {
        self.path.is_some()
            && self
                .state
                .lock()
                .map_or(false, |s| !matches!(*s, State::Failed(_) | State::Empty))
    }

    /// Ошибка загрузки (для настроек).
    pub fn error(&self) -> Option<String> {
        match &*self.state.lock().ok()? {
            State::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn loading(&self) -> bool {
        self.state
            .lock()
            .map_or(false, |s| matches!(*s, State::Loading | State::Raw(_)))
    }

    /// Рисует фон на всё окно (как CSS cover) и затемнение поверх.
    pub fn paint(&self, ctx: &egui::Context, dim: f32, animate: bool, base: Color32) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let State::Raw(raw) = &mut *state {
            let raw = std::mem::take(raw);
            let total_ms: u64 = raw.iter().map(|f| f.delay_ms as u64).sum();
            let frames = raw
                .into_iter()
                .enumerate()
                .map(|(i, f)| {
                    let tex = ctx.load_texture(
                        format!("bg-frame-{i}"),
                        f.image,
                        egui::TextureOptions::LINEAR,
                    );
                    (tex, f.delay_ms)
                })
                .collect();
            *state = State::Ready { frames, total_ms };
        }
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(screen, 0.0, base);
        let State::Ready { frames, total_ms } = &*state else {
            return;
        };
        if frames.is_empty() {
            return;
        }
        // текущий кадр GIF по времени
        let (tex, delay) = if frames.len() > 1 && *total_ms > 0 {
            let t = if animate {
                (ctx.input(|i| i.time) * 1000.0) as u64 % total_ms
            } else {
                0
            };
            let mut acc = 0u64;
            let mut pick = (&frames[0].0, frames[0].1);
            for (tex, d) in frames {
                acc += *d as u64;
                if t < acc {
                    pick = (tex, *d);
                    break;
                }
            }
            pick
        } else {
            (&frames[0].0, 0)
        };
        let size = tex.size_vec2();
        let scale = (screen.width() / size.x).max(screen.height() / size.y);
        let img = Rect::from_center_size(screen.center(), size * scale);
        painter.image(
            tex.id(),
            img,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        if dim > 0.0 {
            painter.rect_filled(
                screen,
                0.0,
                Color32::from_black_alpha((dim.clamp(0.0, 0.95) * 255.0) as u8),
            );
        }
        if animate && frames.len() > 1 {
            ctx.request_repaint_after(Duration::from_millis(delay.max(30) as u64));
        }
    }
}
