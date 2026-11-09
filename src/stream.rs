//! Демонстрация экрана (Go Live): отдельное видеосоединение с голосовым сервером трансляции.
//!
//! Отправка: захват монитора (xcap) → H.264 (OpenH264) → сквозное шифрование DAVE →
//! RTP (FU-A) с расширением playout-delay → транспортное шифрование → UDP.
//! Просмотр: UDP → расшифровка → сборка кадра из RTP → DAVE → декодирование → картинка.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit};
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::unbounded_channel;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use crate::discord::{Event, Sink};
use crate::net::{BoxErr, Proxy};
use crate::voice::{now_ms, parse_endpoint, seed, udp_recv, Dave, Out};

const H264_PT: u8 = 101;
const H264_RTX_PT: u8 = 102;
/// Максимальный размер полезной нагрузки одного RTP-пакета видео.
const MAX_PAYLOAD: usize = 1100;
/// Расширение RTP «playout-delay» (обязательно для видео в Discord), без задержки.
const EXT_HEADER: [u8; 4] = [0xBE, 0xDE, 0x00, 0x01];
const EXT_BODY: [u8; 4] = [0x52, 0x00, 0x00, 0x00];

pub struct StreamParams {
    pub key: String,
    pub rtc_server_id: u64,
    pub user_id: u64,
    pub session_id: String,
    pub token: String,
    pub endpoint: String,
}

#[derive(Clone, Copy, Debug)]
pub struct SendCfg {
    pub monitor: usize,
    /// Захватывать конкретное окно (id окна из `windows()`), а не весь монитор.
    pub window: Option<u32>,
    pub height: u32,
    pub fps: u32,
    /// Качество: 0 — экономное, 1 — сбалансированное, 2 — высокое.
    pub quality: u8,
}

impl SendCfg {
    /// Битрейт видео под разрешение и выбранное качество.
    pub fn bitrate(&self, height: u32) -> u32 {
        let base = match height {
            0..=540 => 1_200_000,
            541..=760 => 2_500_000,
            761..=1100 => 4_500_000,
            _ => 7_000_000,
        };
        let factor = match self.quality {
            0 => 0.6,
            2 => 1.7,
            _ => 1.0,
        };
        let fps_factor = (self.fps.max(5) as f64 / 30.0).clamp(0.5, 1.6);
        (base as f64 * factor * fps_factor) as u32
    }
}

/// Окна, которые можно показать: (id, заголовок, приложение).
pub fn windows() -> Vec<(u32, String, String)> {
    let me = std::process::id();
    let mut out: Vec<(u32, String, String)> = xcap::Window::all()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(true))
        .filter(|w| w.pid().map_or(true, |p| p != me))
        .filter(|w| w.width().unwrap_or(0) >= 120 && w.height().unwrap_or(0) >= 80)
        .filter_map(|w| {
            let title = w.title().ok()?.trim().to_string();
            if title.is_empty() {
                return None;
            }
            Some((w.id().ok()?, title, w.app_name().unwrap_or_default()))
        })
        .collect();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

pub enum Role {
    Send(SendCfg),
    Watch,
}

/// Мониторы для выбора: (название, ширина, высота).
pub fn monitors() -> Vec<(String, u32, u32)> {
    xcap::Monitor::all()
        .map(|list| {
            list.iter()
                .enumerate()
                .map(|(i, m)| {
                    let name = m
                        .friendly_name()
                        .ok()
                        .filter(|n| !n.trim().is_empty())
                        .or_else(|| m.name().ok())
                        .unwrap_or_else(|| format!("Монитор {}", i + 1));
                    let primary = if m.is_primary().unwrap_or(false) {
                        " (основной)"
                    } else {
                        ""
                    };
                    (
                        format!("{name}{primary}"),
                        m.width().unwrap_or(0),
                        m.height().unwrap_or(0),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn run(p: StreamParams, role: Role, proxy: Option<Proxy>, sink: Sink) {
    let result = session(&p, role, proxy.as_ref(), &sink).await;
    let error = match result {
        Ok(()) => None,
        Err(e) => {
            crate::store::log(&format!("трансляция {}: {e}", p.key));
            Some(e.to_string())
        }
    };
    sink.send(Event::StreamEnded {
        key: p.key.clone(),
        error,
    });
}

// ======================= H.264 и RTP =======================

/// NAL-блоки из потока Annex B (без стартовых кодов).
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut end = if k + 1 < starts.len() {
            starts[k + 1] - 3
        } else {
            data.len()
        };
        // убираем ведущий ноль 4-байтового стартового кода следующего блока
        while end > s && data[end - 1] == 0 && k + 1 < starts.len() {
            end -= 1;
        }
        if end > s {
            out.push(&data[s..end]);
        }
    }
    out
}

/// Приводит поток к 4-байтовым стартовым кодам (так ждут клиенты Discord).
fn normalize_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in split_nals(data) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

/// Делит кадр на RTP-нагрузки: целые NAL-блоки или фрагменты FU-A.
fn packetize(frame: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for nal in split_nals(frame) {
        if nal.len() <= MAX_PAYLOAD {
            out.push(nal.to_vec());
            continue;
        }
        let header = nal[0];
        let indicator = (header & 0xE0) | 28;
        let kind = header & 0x1F;
        let body = &nal[1..];
        let chunks: Vec<&[u8]> = body.chunks(MAX_PAYLOAD - 2).collect();
        let last = chunks.len() - 1;
        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut fu = kind;
            if i == 0 {
                fu |= 0x80;
            }
            if i == last {
                fu |= 0x40;
            }
            let mut p = Vec::with_capacity(chunk.len() + 2);
            p.push(indicator);
            p.push(fu);
            p.extend_from_slice(chunk);
            out.push(p);
        }
    }
    out
}

struct VideoRtp {
    ssrc: u32,
    seq: u16,
    timestamp: u32,
    nonce: u32,
}

impl VideoRtp {
    fn packet(&mut self, cipher: &Aes256Gcm, payload: &[u8], marker: bool) -> Option<Vec<u8>> {
        let mut pkt = Vec::with_capacity(16 + 4 + payload.len() + 16 + 4);
        pkt.push(0x90); // версия 2 + бит расширения
        pkt.push(H264_PT | if marker { 0x80 } else { 0 });
        pkt.extend_from_slice(&self.seq.to_be_bytes());
        pkt.extend_from_slice(&self.timestamp.to_be_bytes());
        pkt.extend_from_slice(&self.ssrc.to_be_bytes());
        pkt.extend_from_slice(&EXT_HEADER);
        let mut plain = Vec::with_capacity(4 + payload.len());
        plain.extend_from_slice(&EXT_BODY);
        plain.extend_from_slice(payload);
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&self.nonce.to_be_bytes());
        let ct = cipher
            .encrypt(
                GenericArray::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &pkt[..16],
                },
            )
            .ok()?;
        pkt.extend_from_slice(&ct);
        pkt.extend_from_slice(&self.nonce.to_be_bytes());
        self.seq = self.seq.wrapping_add(1);
        self.nonce = self.nonce.wrapping_add(1);
        Some(pkt)
    }
}

/// Входящий RTP-пакет после расшифровки.
struct Incoming {
    ssrc: u32,
    seq: u16,
    marker: bool,
    pt: u8,
    payload: Vec<u8>,
}

fn open_rtp(cipher: &Aes256Gcm, pkt: &[u8]) -> Option<Incoming> {
    if pkt.len() < 12 + 4 + 16 || pkt[0] >> 6 != 2 {
        return None;
    }
    let pt = pkt[1] & 0x7f;
    if !(96..=127).contains(&pt) {
        return None; // RTCP и прочее
    }
    let cc = (pkt[0] & 0x0f) as usize;
    let mut header = 12 + cc * 4;
    let mut ext_len = 0;
    if pkt[0] & 0x10 != 0 {
        if pkt.len() < header + 4 {
            return None;
        }
        ext_len = u16::from_be_bytes([pkt[header + 2], pkt[header + 3]]) as usize * 4;
        header += 4;
    }
    if pkt.len() < header + 4 + 16 {
        return None;
    }
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&pkt[pkt.len() - 4..]);
    let plain = cipher
        .decrypt(
            GenericArray::from_slice(&nonce),
            Payload {
                msg: &pkt[header..pkt.len() - 4],
                aad: &pkt[..header],
            },
        )
        .ok()?;
    if plain.len() < ext_len {
        return None;
    }
    Some(Incoming {
        ssrc: u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]),
        seq: u16::from_be_bytes([pkt[2], pkt[3]]),
        marker: pkt[1] & 0x80 != 0,
        pt,
        payload: plain[ext_len..].to_vec(),
    })
}

/// Сборка кадров H.264 из RTP (одиночные NAL, STAP-A, FU-A).
#[derive(Default)]
struct Depacketizer {
    frame: Vec<u8>,
    fu: Vec<u8>,
    last_seq: Option<u16>,
    broken: bool,
    waiting_key: bool,
}

impl Depacketizer {
    fn push(&mut self, p: &Incoming) -> Option<Vec<u8>> {
        if let Some(last) = self.last_seq {
            if p.seq != last.wrapping_add(1) {
                self.broken = true; // потеря пакета — кадр испорчен
            }
        }
        self.last_seq = Some(p.seq);
        let data = &p.payload;
        if !data.is_empty() {
            match data[0] & 0x1F {
                1..=23 => {
                    self.frame.extend_from_slice(&[0, 0, 0, 1]);
                    self.frame.extend_from_slice(data);
                }
                24 => {
                    let mut i = 1;
                    while i + 2 <= data.len() {
                        let size = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                        i += 2;
                        if i + size > data.len() {
                            break;
                        }
                        self.frame.extend_from_slice(&[0, 0, 0, 1]);
                        self.frame.extend_from_slice(&data[i..i + size]);
                        i += size;
                    }
                }
                28 if data.len() > 2 => {
                    let fu = data[1];
                    if fu & 0x80 != 0 {
                        self.fu.clear();
                        self.fu.push((data[0] & 0xE0) | (fu & 0x1F));
                    }
                    self.fu.extend_from_slice(&data[2..]);
                    if fu & 0x40 != 0 {
                        self.frame.extend_from_slice(&[0, 0, 0, 1]);
                        self.frame.extend_from_slice(&self.fu);
                        self.fu.clear();
                    }
                }
                _ => {}
            }
        }
        if !p.marker {
            return None;
        }
        let frame = std::mem::take(&mut self.frame);
        let broken = std::mem::replace(&mut self.broken, false);
        if broken || frame.is_empty() {
            self.waiting_key = true;
            return None;
        }
        if self.waiting_key {
            // после потери ждём ключевой кадр, иначе декодер покажет «кашу»
            if !split_nals(&frame)
                .iter()
                .any(|n| matches!(n.first().map(|b| b & 0x1F), Some(5) | Some(7)))
            {
                return None;
            }
            self.waiting_key = false;
        }
        Some(frame)
    }
}

// ======================= захват и кодирование =======================

/// Источник захвата: монитор или окно.
enum Source {
    Monitor(xcap::Monitor),
    Window(xcap::Window),
}

impl Source {
    fn open(cfg: &SendCfg) -> Option<Source> {
        if let Some(id) = cfg.window {
            if let Some(w) = xcap::Window::all()
                .unwrap_or_default()
                .into_iter()
                .find(|w| w.id().ok() == Some(id))
            {
                return Some(Source::Window(w));
            }
        }
        let monitors = xcap::Monitor::all().unwrap_or_default();
        let fallback = monitors.first().cloned();
        monitors
            .into_iter()
            .nth(cfg.monitor)
            .or(fallback)
            .map(Source::Monitor)
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Source::Monitor(m) => (m.width().unwrap_or(1920), m.height().unwrap_or(1080)),
            Source::Window(w) => (w.width().unwrap_or(1280), w.height().unwrap_or(720)),
        }
    }

    fn capture(&self) -> Result<image::RgbaImage, String> {
        match self {
            Source::Monitor(m) => m.capture_image().map_err(|e| e.to_string()),
            Source::Window(w) => {
                if w.is_minimized().unwrap_or(false) {
                    return Err("окно свёрнуто".into());
                }
                w.capture_image().map_err(|e| e.to_string())
            }
        }
    }
}

/// Размер кадра трансляции: высота не больше выбранной, обе стороны чётные.
fn frame_size(src: (u32, u32), height: u32) -> (u32, u32) {
    let (sw, sh) = (src.0.max(2), src.1.max(2));
    let h = height.min(sh) & !1;
    let w = (((sw as f64 * h as f64 / sh as f64).round() as u32).max(2)) & !1;
    (w.max(2), h.max(2))
}

/// Поток захвата экрана: снимает монитор или окно, кодирует в H.264 и отдаёт кадры.
/// Заодно шлёт в интерфейс уменьшенное превью — чтобы стример видел, что показывает.
fn spawn_capture(
    key: String,
    cfg: SendCfg,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    sink: Sink,
) -> Result<(u32, u32), String> {
    let is_window = cfg.window.is_some();
    let not_found = move || {
        if is_window {
            "Окно для трансляции не найдено — возможно, оно закрыто".to_string()
        } else {
            "Монитор для трансляции не найден".to_string()
        }
    };
    // Проверяем доступ к захвату заранее, чтобы сразу показать понятную ошибку.
    // (Дескрипторы окон нельзя передавать между потоками — в потоке захвата откроем заново.)
    let (w, h) = {
        let source = Source::open(&cfg).ok_or_else(not_found)?;
        source
            .capture()
            .map_err(|e| format!("Не удалось захватить экран: {e}"))?;
        frame_size(source.size(), cfg.height)
    };

    std::thread::Builder::new()
        .name("screen-capture".into())
        .spawn(move || {
            let Some(source) = Source::open(&cfg) else {
                sink.send(Event::Error(not_found()));
                return;
            };
            use openh264::encoder::{
                BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
            };
            use openh264::formats::{RgbaSliceU8, YUVBuffer};
            let config = EncoderConfig::new()
                .usage_type(UsageType::ScreenContentRealTime)
                .bitrate(BitRate::from_bps(cfg.bitrate(h)))
                .max_frame_rate(FrameRate::from_hz(cfg.fps as f32))
                .rate_control_mode(RateControlMode::Bitrate)
                .skip_frames(true);
            let mut encoder =
                match Encoder::with_api_config(openh264::OpenH264API::from_source(), config) {
                    Ok(e) => e,
                    Err(e) => {
                        sink.send(Event::Error(format!("Видеокодек не запустился: {e}")));
                        return;
                    }
                };
            let frame_time = Duration::from_millis(1000 / cfg.fps.max(1) as u64);
            let preview_every = Duration::from_millis(125);
            let mut last_preview = Instant::now() - preview_every;
            let mut n: u64 = 0;
            let mut failures = 0u32;
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                match source.capture() {
                    Ok(img) => {
                        failures = 0;
                        let img = if img.width() != w || img.height() != h {
                            image::imageops::resize(
                                &img,
                                w,
                                h,
                                image::imageops::FilterType::Triangle,
                            )
                        } else {
                            img
                        };
                        if last_preview.elapsed() >= preview_every {
                            last_preview = Instant::now();
                            let pw = 480u32.min(w);
                            let ph = ((h as f64 * pw as f64 / w as f64) as u32).max(2);
                            let small = image::imageops::resize(
                                &img,
                                pw,
                                ph,
                                image::imageops::FilterType::Nearest,
                            );
                            sink.send(Event::StreamFrame {
                                key: key.clone(),
                                image: egui::ColorImage::from_rgba_unmultiplied(
                                    [pw as usize, ph as usize],
                                    small.as_raw(),
                                ),
                            });
                        }
                        let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(
                            img.as_raw(),
                            (w as usize, h as usize),
                        ));
                        if n % (cfg.fps as u64 * 2) == 0 || FORCE_KEY.swap(false, Ordering::Relaxed)
                        {
                            encoder.force_intra_frame(); // ключевой кадр — для новых зрителей
                        }
                        match encoder.encode(&yuv) {
                            Ok(bits) => {
                                let data = bits.to_vec();
                                if !data.is_empty() && tx.send(normalize_annexb(&data)).is_err() {
                                    break;
                                }
                            }
                            Err(e) => crate::store::log(&format!("кодирование кадра: {e}")),
                        }
                        n += 1;
                    }
                    Err(e) => {
                        failures += 1;
                        if failures == 1 || failures % 20 == 0 {
                            crate::store::log(&format!("захват экрана: {e}"));
                        }
                        if failures == 10 {
                            sink.send(Event::Error(format!("Захват экрана не удаётся: {e}")));
                        }
                        std::thread::sleep(Duration::from_millis(300));
                    }
                }
                if let Some(rest) = frame_time.checked_sub(started.elapsed()) {
                    std::thread::sleep(rest);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok((w, h))
}

/// Зритель попросил ключевой кадр (RTCP PLI) — следующий кадр будет ключевым.
static FORCE_KEY: AtomicBool = AtomicBool::new(false);

/// Зашифрованный RTCP-пакет (режим rtpsize: первые 8 байт — открытые, в AAD).
fn rtcp_packet(cipher: &Aes256Gcm, header8: &[u8; 8], body: &[u8], nonce: u32) -> Option<Vec<u8>> {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&nonce.to_be_bytes());
    let ct = cipher
        .encrypt(
            GenericArray::from_slice(&n),
            Payload {
                msg: body,
                aad: header8,
            },
        )
        .ok()?;
    let mut pkt = Vec::with_capacity(8 + ct.len() + 4);
    pkt.extend_from_slice(header8);
    pkt.extend_from_slice(&ct);
    pkt.extend_from_slice(&nonce.to_be_bytes());
    Some(pkt)
}

/// RTCP PLI: «пришли ключевой кадр» для видеопотока `media_ssrc`.
fn pli(cipher: &Aes256Gcm, sender_ssrc: u32, media_ssrc: u32, nonce: u32) -> Option<Vec<u8>> {
    let mut h = [0u8; 8];
    h[0] = 0x81; // V=2, FMT=1 (PLI)
    h[1] = 206; // PSFB
    h[2..4].copy_from_slice(&2u16.to_be_bytes());
    h[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    rtcp_packet(cipher, &h, &media_ssrc.to_be_bytes(), nonce)
}

/// Пустой RTCP Receiver Report — держит UDP-«дырку» в NAT открытой.
fn receiver_report(cipher: &Aes256Gcm, sender_ssrc: u32, nonce: u32) -> Option<Vec<u8>> {
    let mut h = [0u8; 8];
    h[0] = 0x80;
    h[1] = 201;
    h[2..4].copy_from_slice(&1u16.to_be_bytes());
    h[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    rtcp_packet(cipher, &h, &[], nonce)
}

/// Автор трансляции из ключа: «guild:<сервер>:<канал>:<id>» или «call:<канал>:<id>».
fn key_owner(key: &str) -> Option<u64> {
    key.rsplit(':').next()?.parse().ok()
}

/// Поток декодирования: H.264 → RGBA → интерфейс.
fn spawn_decoder(key: String, sink: Sink) -> std::sync::mpsc::Sender<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("stream-decoder".into())
        .spawn(move || {
            use openh264::formats::YUVSource;
            let mut decoder = match openh264::decoder::Decoder::new() {
                Ok(d) => d,
                Err(e) => {
                    sink.send(Event::Error(format!("Видеодекодер не запустился: {e}")));
                    return;
                }
            };
            let mut rgba: Vec<u8> = Vec::new();
            let mut errors = 0u32;
            while let Ok(first) = rx.recv() {
                // все кадры нужно декодировать по порядку (P-кадры ссылаются на предыдущие),
                // но в картинку переводим только самый свежий
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let last = batch.len() - 1;
                for (i, frame) in batch.iter().enumerate() {
                    match decoder.decode(frame) {
                        Ok(Some(yuv)) if i == last => {
                            let (w, h) = yuv.dimensions();
                            rgba.resize(w * h * 4, 0);
                            yuv.write_rgba8(&mut rgba);
                            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                            sink.send(Event::StreamFrame {
                                key: key.clone(),
                                image,
                            });
                        }
                        Ok(_) => {}
                        Err(e) => {
                            errors += 1;
                            if errors % 50 == 1 {
                                crate::store::log(&format!("декодирование кадра: {e}"));
                            }
                        }
                    }
                }
            }
        })
        .ok();
    tx
}

// ======================= соединение =======================

async fn session(
    p: &StreamParams,
    role: Role,
    proxy: Option<&Proxy>,
    sink: &Sink,
) -> Result<(), BoxErr> {
    let (host, port) = parse_endpoint(&p.endpoint);
    crate::store::log(&format!(
        "трансляция {}: подключаюсь к {host}:{port}",
        p.key
    ));
    let stream = crate::net::connect(proxy, &host, port).await?;
    let (ws, _) = tokio_tungstenite::client_async_tls(format!("wss://{host}/?v=8"), stream).await?;
    let (mut write, mut read) = ws.split();

    let sending = matches!(role, Role::Send(_));
    let cfg = match &role {
        Role::Send(c) => Some(*c),
        Role::Watch => None,
    };
    let streams = if sending {
        json!([{ "type": "screen", "rid": "100", "quality": 100 }])
    } else {
        json!([])
    };
    let identify = json!({
        "op": 0,
        "d": {
            "server_id": p.rtc_server_id.to_string(),
            "user_id": p.user_id.to_string(),
            "session_id": p.session_id,
            "token": p.token,
            "video": true,
            "streams": streams,
            "max_dave_protocol_version": davey::DAVE_PROTOCOL_VERSION,
        }
    });
    write.send(WsMsg::Text(identify.to_string())).await?;

    let mut seq_ack: i64 = -1;
    let mut heartbeat: Option<tokio::time::Interval> = None;
    let mut udp: Option<UdpSocket> = None;
    let mut cipher: Option<Aes256Gcm> = None;
    let mut audio_ssrc = 0u32;
    let mut video_ssrc = 0u32;
    let mut rtx_ssrc = 0u32;
    // у трансляций своя MLS-группа: id сервера трансляции минус один
    let mut dave = Dave::new(p.user_id, p.rtc_server_id.saturating_sub(1));
    let mut users: HashSet<u64> = HashSet::new();
    let mut ssrc_owner: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let mut rtp: Option<VideoRtp> = None;
    let mut fps = 30u32;
    let (frame_tx, mut frame_rx) = unbounded_channel::<Vec<u8>>();
    let mut frame_tx = Some(frame_tx);
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_guard = StopOnDrop(stop.clone());
    let mut decoder_tx: Option<std::sync::mpsc::Sender<Vec<u8>>> = None;
    let mut depack: std::collections::HashMap<u32, Depacketizer> = std::collections::HashMap::new();
    let mut rbuf = vec![0u8; 4096];
    let mut live_sent = false;
    let mut sent_frames = 0u64;
    // Зритель: служебные RTCP-пакеты (PLI и keepalive) и статистика для лога.
    let owner_hint = key_owner(&p.key);
    let mut rtx_ssrcs: HashSet<u32> = HashSet::new();
    let mut rtcp_nonce: u32 = 0;
    let mut rtcp_tick = tokio::time::interval(Duration::from_millis(1000));
    rtcp_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rr = Instant::now();
    let mut stats = (0u64, 0u64, 0u64, 0u64); // пакеты, кадры, ошибки DAVE, показано
    let mut stats_logged = Instant::now();

    loop {
        let mut out: Vec<Out> = Vec::new();
        tokio::select! {
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err("сервер трансляции закрыл соединение".into()),
                };
                match msg {
                    WsMsg::Text(text) => {
                        let v: Value = serde_json::from_str(&text)?;
                        if let Some(s) = v["seq"].as_i64() {
                            seq_ack = s;
                        }
                        let op = v["op"].as_u64().unwrap_or(999);
                        let d = &v["d"];
                        match op {
                            8 => {
                                let ms = d["heartbeat_interval"].as_f64().unwrap_or(13750.0).max(1000.0);
                                let period = Duration::from_millis(ms as u64);
                                heartbeat = Some(tokio::time::interval_at(tokio::time::Instant::now() + period, period));
                            }
                            2 => {
                                audio_ssrc = d["ssrc"].as_u64().unwrap_or(0) as u32;
                                if let Some(s) = d["streams"].as_array().and_then(|a| a.first()) {
                                    video_ssrc = s["ssrc"].as_u64().unwrap_or(0) as u32;
                                    rtx_ssrc = s["rtx_ssrc"].as_u64().unwrap_or(0) as u32;
                                }
                                let ip = d["ip"].as_str().unwrap_or("").to_string();
                                let port = d["port"].as_u64().unwrap_or(0) as u16;
                                let socket = UdpSocket::bind("0.0.0.0:0").await?;
                                socket.connect((ip.as_str(), port)).await?;
                                let mut disc = [0u8; 74];
                                disc[0..2].copy_from_slice(&1u16.to_be_bytes());
                                disc[2..4].copy_from_slice(&70u16.to_be_bytes());
                                disc[4..8].copy_from_slice(&audio_ssrc.to_be_bytes());
                                socket.send(&disc).await?;
                                let mut resp = [0u8; 74];
                                let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut resp))
                                    .await
                                    .map_err(|_| "UDP не отвечает — нужен VPN в режиме «весь трафик» (TUN)")??;
                                if n < 74 {
                                    return Err("неожиданный ответ IP discovery".into());
                                }
                                let end = resp[8..72].iter().position(|b| *b == 0).unwrap_or(64);
                                let my_ip = String::from_utf8_lossy(&resp[8..8 + end]).to_string();
                                let my_port = u16::from_be_bytes([resp[72], resp[73]]);
                                out.push(Out::Json(json!({
                                    "op": 1,
                                    "d": {
                                        "protocol": "udp",
                                        "data": { "address": my_ip, "port": my_port, "mode": "aead_aes256_gcm_rtpsize" },
                                        "codecs": [
                                            { "name": "opus", "type": "audio", "priority": 1000, "payload_type": 120 },
                                            { "name": "H264", "type": "video", "priority": 1000, "payload_type": H264_PT,
                                              "rtx_payload_type": H264_RTX_PT, "encode": sending, "decode": true }
                                        ]
                                    }
                                }).to_string()));
                                udp = Some(socket);
                            }
                            4 => {
                                let key: Vec<u8> = d["secret_key"].as_array().map(|a| {
                                    a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect()
                                }).unwrap_or_default();
                                cipher = Some(Aes256Gcm::new_from_slice(&key).map_err(|_| "неверный ключ шифрования")?);
                                dave.version = d["dave_protocol_version"].as_u64().unwrap_or(0) as u16;
                                dave.downgraded = dave.version == 0;
                                dave.reinit(&mut out);
                                if let Some(c) = cfg {
                                    fps = c.fps.max(1);
                                    let (tx, rx2) = unbounded_channel::<Vec<u8>>();
                                    frame_rx = rx2;
                                    let (w, h) = spawn_capture(p.key.clone(), c, tx, stop.clone(), sink.clone())?;
                                    frame_tx = None;
                                    rtp = Some(VideoRtp { ssrc: video_ssrc, seq: (seed() & 0xFFFF) as u16, timestamp: (seed() >> 20) as u32, nonce: 0 });
                                    let bitrate = c.bitrate(h);
                                    out.push(Out::Json(json!({
                                        "op": 12,
                                        "d": {
                                            "audio_ssrc": audio_ssrc,
                                            "video_ssrc": video_ssrc,
                                            "rtx_ssrc": rtx_ssrc,
                                            "streams": [{
                                                "type": "video", "rid": "100", "ssrc": video_ssrc, "rtx_ssrc": rtx_ssrc,
                                                "active": true, "quality": 100, "max_bitrate": bitrate, "max_framerate": fps,
                                                "max_resolution": { "type": "fixed", "width": w, "height": h }
                                            }]
                                        }
                                    }).to_string()));
                                    out.push(Out::Json(json!({ "op": 5, "d": { "speaking": 2, "delay": 0, "ssrc": audio_ssrc } }).to_string()));
                                } else {
                                    decoder_tx = Some(spawn_decoder(p.key.clone(), sink.clone()));
                                    // Просим сервер присылать видео в лучшем качестве.
                                    out.push(Out::Json(json!({ "op": 15, "d": { "any": 100 } }).to_string()));
                                }
                            }
                            12 => {
                                // кто какой видеопоток отправляет
                                let uid: u64 = d["user_id"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                                let vs = d["video_ssrc"].as_u64().unwrap_or(0) as u32;
                                if uid != 0 {
                                    users.insert(uid);
                                    if vs != 0 {
                                        ssrc_owner.insert(vs, uid);
                                    }
                                    // В новых версиях протокола ssrc видео приходит в списке потоков.
                                    if let Some(list) = d["streams"].as_array() {
                                        for st in list {
                                            let ss = st["ssrc"].as_u64().unwrap_or(0) as u32;
                                            if ss != 0 {
                                                ssrc_owner.insert(ss, uid);
                                            }
                                            let rtx = st["rtx_ssrc"].as_u64().unwrap_or(0) as u32;
                                            if rtx != 0 {
                                                rtx_ssrcs.insert(rtx);
                                            }
                                        }
                                    }
                                }
                            }
                            5 => {
                                if let Some(uid) = d["user_id"].as_str().and_then(|s| s.parse::<u64>().ok()) {
                                    users.insert(uid);
                                }
                            }
                            11 => {
                                if let Some(ids) = d["user_ids"].as_array() {
                                    for id in ids.iter().filter_map(|x| x.as_str()).filter_map(|s| s.parse::<u64>().ok()) {
                                        users.insert(id);
                                    }
                                }
                            }
                            13 => {
                                if let Some(uid) = d["user_id"].as_str().and_then(|s| s.parse::<u64>().ok()) {
                                    users.remove(&uid);
                                }
                            }
                            21 | 22 | 24 => dave.on_json(op, d, &mut out),
                            _ => {}
                        }
                    }
                    WsMsg::Binary(data) => {
                        if data.len() >= 3 {
                            seq_ack = u16::from_be_bytes([data[0], data[1]]) as i64;
                            let mut known: Vec<u64> = users.iter().copied().collect();
                            known.push(p.user_id);
                            dave.on_binary(data[2], &data[3..], &known, &mut out);
                        }
                    }
                    WsMsg::Close(frame) => {
                        let code = frame.map(|f| u16::from(f.code)).unwrap_or(1000);
                        return match code {
                            1000 | 4014 => Ok(()),
                            other => Err(format!("сервер трансляции закрыл соединение (код {other})").into()),
                        };
                    }
                    _ => {}
                }
            }
            _ = async {
                match heartbeat.as_mut() {
                    Some(h) => { h.tick().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                out.push(Out::Json(json!({ "op": 3, "d": { "t": now_ms(), "seq_ack": seq_ack } }).to_string()));
            }
            frame = frame_rx.recv(), if frame_tx.is_none() => {
                let Some(frame) = frame else {
                    return Err("захват экрана остановился".into());
                };
                if let (Some(c), Some(u), Some(r)) = (cipher.as_ref(), udp.as_ref(), rtp.as_mut()) {
                    let mut data = frame;
                    let mut ok = true;
                    if dave.active() {
                        match dave.session.as_mut() {
                            Some(s) if s.can_encrypt() => {
                                match s.encrypt(davey::MediaType::VIDEO, davey::Codec::H264, &data) {
                                    Ok(enc) => data = enc.into_owned(),
                                    Err(_) => ok = false,
                                }
                            }
                            _ => ok = false,
                        }
                    }
                    if ok {
                        let payloads = packetize(&data);
                        let last = payloads.len().saturating_sub(1);
                        for (i, pl) in payloads.iter().enumerate() {
                            if let Some(pkt) = r.packet(c, pl, i == last) {
                                let _ = u.send(&pkt).await;
                            }
                        }
                        sent_frames += 1;
                        if !live_sent {
                            live_sent = true;
                            sink.send(Event::StreamLive { key: p.key.clone() });
                        }
                        if sent_frames % 300 == 0 {
                            crate::store::log(&format!("трансляция: отправлено кадров {sent_frames}, DAVE активно={}", dave.active()));
                        }
                    }
                    r.timestamp = r.timestamp.wrapping_add(90_000 / fps);
                }
            }
            _ = rtcp_tick.tick(), if !sending && udp.is_some() && cipher.is_some() => {
                let (Some(u), Some(c)) = (udp.as_ref(), cipher.as_ref()) else { continue };
                // Пока нет ключевого кадра — просим его у стримера раз в секунду.
                let mut want: Vec<u32> = depack
                    .iter()
                    .filter(|(_, d)| d.waiting_key)
                    .map(|(ssrc, _)| *ssrc)
                    .collect();
                if depack.is_empty() {
                    want.extend(ssrc_owner.keys().copied());
                }
                for media in want {
                    rtcp_nonce = rtcp_nonce.wrapping_add(1);
                    if let Some(pkt) = pli(c, audio_ssrc, media, rtcp_nonce) {
                        let _ = u.send(&pkt).await;
                    }
                }
                if last_rr.elapsed() >= Duration::from_secs(5) {
                    last_rr = Instant::now();
                    rtcp_nonce = rtcp_nonce.wrapping_add(1);
                    if let Some(pkt) = receiver_report(c, audio_ssrc, rtcp_nonce) {
                        let _ = u.send(&pkt).await;
                    }
                }
                if stats_logged.elapsed() >= Duration::from_secs(10) {
                    stats_logged = Instant::now();
                    crate::store::log(&format!(
                        "трансляция (просмотр): пакетов {}, кадров {}, ошибок DAVE {}, в декодер {}, DAVE={}",
                        stats.0, stats.1, stats.2, stats.3, dave.active()
                    ));
                }
            }
            res = udp_recv(udp.as_ref(), &mut rbuf) => {
                let n = res?;
                if sending {
                    // Сервер передаёт просьбы зрителей о ключевом кадре (RTCP PLI / FIR).
                    if n >= 8 && rbuf[0] >> 6 == 2 && rbuf[1] == 206 {
                        let fmt = rbuf[0] & 0x1F;
                        if fmt == 1 || fmt == 4 {
                            FORCE_KEY.store(true, Ordering::Relaxed);
                        }
                    }
                    continue;
                }
                let Some(c) = cipher.as_ref() else { continue };
                let Some(pkt) = open_rtp(c, &rbuf[..n]) else { continue };
                stats.0 += 1;
                // Звук трансляции (opus) и повторы (RTX) пропускаем, видео — любой другой тип.
                if pkt.pt == 120 || pkt.pt == H264_RTX_PT || rtx_ssrcs.contains(&pkt.ssrc) {
                    continue;
                }
                if pkt.pt != H264_PT && stats.0 < 5 {
                    crate::store::log(&format!("трансляция: видео с типом {} (ждали {H264_PT})", pkt.pt));
                }
                let Some(frame) = depack
                    .entry(pkt.ssrc)
                    .or_insert_with(|| Depacketizer { waiting_key: true, ..Default::default() })
                    .push(&pkt) else { continue };
                stats.1 += 1;
                let mut data = frame;
                // Автор потока: из сигнализации, а если её не было — из ключа трансляции.
                let owner = ssrc_owner.get(&pkt.ssrc).copied().or(owner_hint);
                if let Some(owner) = owner {
                    if let Some(s) = dave.session.as_mut() {
                        let can = s.is_ready() && (dave.version != 0 && !dave.downgraded || s.can_passthrough(owner));
                        if can {
                            match s.decrypt(owner, davey::MediaType::VIDEO, &data) {
                                Ok(plain) => data = plain,
                                Err(_) => {
                                    stats.2 += 1;
                                    if let Some(d) = depack.get_mut(&pkt.ssrc) {
                                        d.waiting_key = true;
                                    }
                                    continue;
                                }
                            }
                        } else if dave.active() {
                            stats.2 += 1;
                            continue;
                        }
                    }
                } else if dave.active() {
                    continue;
                }
                stats.3 += 1;
                if let Some(tx) = &decoder_tx {
                    let _ = tx.send(data);
                }
            }
        }
        for o in out {
            match o {
                Out::Json(t) => write.send(WsMsg::Text(t)).await?,
                Out::Bin(b) => write.send(WsMsg::Binary(b)).await?,
            }
        }
    }
}

/// Останавливает поток захвата, когда соединение завершается.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
