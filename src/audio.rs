//! Звук для звонков: захват микрофона и вывод в динамики через cpal (WASAPI в Windows).
//!
//! Всё внутри — 48 кГц, как требует Opus/Discord. Если устройство работает на другой
//! частоте, звук пересэмплируется. Потоки cpal живут в отдельном потоке ОС
//! (в Windows они не переносятся между потоками), наружу отдаются только очереди.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const RATE: u32 = 48_000;
/// Сколько звука держать в очереди максимум (иначе копится задержка).
const MAX_CAPTURE: usize = RATE as usize / 2; // 0,5 с моно
const MAX_PLAYBACK: usize = RATE as usize * 2 * 3 / 10; // 0,3 с стерео

/// Простое линейное пересэмплирование, сохраняющее состояние между вызовами.
struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            prev: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let n = input.len() as f64;
        loop {
            let i = self.pos.floor();
            if i + 1.0 >= n {
                break;
            }
            let frac = (self.pos - i) as f32;
            let a = if i < 0.0 {
                self.prev
            } else {
                input[i as usize]
            };
            let b = input[(i + 1.0) as usize];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n;
        self.prev = input[input.len() - 1];
    }
}

pub type Playback = Arc<Mutex<HashMap<u32, VecDeque<f32>>>>;

pub struct Audio {
    /// Микрофон: моно, 48 кГц.
    pub capture: Arc<Mutex<VecDeque<f32>>>,
    /// Собеседники: ssrc -> стерео (L,R,L,R…), 48 кГц.
    pub playback: Playback,
    /// То, что реально ушло в динамики (моно, 48 кГц) — образец для подавления эха.
    pub render: Arc<Mutex<VecDeque<f32>>>,
    pub has_mic: bool,
    stop: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
}

/// Общая громкость вывода (0..2), хранится битами f32.
static OUTPUT_VOLUME: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0

pub fn set_output_volume(v: f32) {
    OUTPUT_VOLUME.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
}

fn output_volume() -> f32 {
    f32::from_bits(OUTPUT_VOLUME.load(Ordering::Relaxed))
}

/// Сколько держать образца для эхоподавления (0,5 с).
const MAX_RENDER: usize = RATE as usize / 2;

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Добавляет звук собеседника в очередь вывода.
pub fn push_playback(playback: &Playback, ssrc: u32, samples: &[f32]) {
    if let Ok(mut map) = playback.lock() {
        let q = map.entry(ssrc).or_default();
        q.extend(samples.iter().copied());
        while q.len() > MAX_PLAYBACK {
            q.pop_front();
        }
    }
}

fn pick_config(
    configs: Vec<cpal::SupportedStreamConfigRange>,
    fallback: cpal::SupportedStreamConfig,
) -> cpal::SupportedStreamConfig {
    let preferred = configs.into_iter().find(|c| {
        c.min_sample_rate().0 <= RATE
            && c.max_sample_rate().0 >= RATE
            && matches!(
                c.sample_format(),
                cpal::SampleFormat::F32 | cpal::SampleFormat::I16
            )
    });
    match preferred {
        Some(c) => c.with_sample_rate(cpal::SampleRate(RATE)),
        None => fallback,
    }
}

fn build_input(
    device: &cpal::Device,
    capture: Arc<Mutex<VecDeque<f32>>>,
    level: Arc<AtomicU32>,
    monitor: Option<Playback>,
) -> Result<cpal::Stream, String> {
    let fallback = device.default_input_config().map_err(|e| e.to_string())?;
    let configs: Vec<_> = device
        .supported_input_configs()
        .map(|c| c.collect())
        .unwrap_or_default();
    let supported = pick_config(configs, fallback);
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels.max(1) as usize;
    let mut resampler = Resampler::new(config.sample_rate.0, RATE);
    let mut mono: Vec<f32> = Vec::new();
    let mut out: Vec<f32> = Vec::new();
    let err = |e| crate::store::log(&format!("микрофон: {e}"));

    let mut handle = move |samples: &mut dyn Iterator<Item = f32>| {
        mono.clear();
        let mut acc = 0.0;
        let mut k = 0;
        for s in samples {
            acc += s;
            k += 1;
            if k == channels {
                mono.push(acc / channels as f32);
                acc = 0.0;
                k = 0;
            }
        }
        out.clear();
        resampler.process(&mono, &mut out);
        if !out.is_empty() {
            let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
            level.store(rms.to_bits(), Ordering::Relaxed);
        }
        if let Some(pb) = &monitor {
            // проверка микрофона: слышите себя
            let stereo: Vec<f32> = out.iter().flat_map(|s| [*s, *s]).collect();
            push_playback(pb, 0, &stereo);
            return;
        }
        if let Ok(mut q) = capture.lock() {
            q.extend(out.iter().copied());
            while q.len() > MAX_CAPTURE {
                q.pop_front();
            }
        }
    };

    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| handle(&mut data.iter().copied()),
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                handle(&mut data.iter().map(|s| *s as f32 / 32768.0))
            },
            err,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                handle(&mut data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0))
            },
            err,
            None,
        ),
        other => return Err(format!("формат микрофона {other:?} не поддерживается")),
    }
    .map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Смешивает всех собеседников в один стерео-поток (n кадров).
fn mix(playback: &Playback, frames: usize, out: &mut Vec<f32>) {
    out.clear();
    out.resize(frames * 2, 0.0);
    if let Ok(mut map) = playback.lock() {
        for q in map.values_mut() {
            let take = q.len().min(frames * 2);
            for (i, s) in q.drain(..take).enumerate() {
                out[i] += s;
            }
        }
        map.retain(|_, q| !q.is_empty() || q.capacity() < 4096);
    }
    let vol = output_volume();
    for s in out.iter_mut() {
        *s = (*s * vol).clamp(-1.0, 1.0);
    }
}

fn build_output(
    device: &cpal::Device,
    playback: Playback,
    render: Option<Arc<Mutex<VecDeque<f32>>>>,
) -> Result<cpal::Stream, String> {
    let fallback = device.default_output_config().map_err(|e| e.to_string())?;
    let configs: Vec<_> = device
        .supported_output_configs()
        .map(|c| c.collect())
        .unwrap_or_default();
    let supported = pick_config(configs, fallback);
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels.max(1) as usize;
    let mut rs_l = Resampler::new(RATE, config.sample_rate.0);
    let mut rs_r = Resampler::new(RATE, config.sample_rate.0);
    let mut fifo_l: VecDeque<f32> = VecDeque::new();
    let mut fifo_r: VecDeque<f32> = VecDeque::new();
    let mut mixed: Vec<f32> = Vec::new();
    let mut tmp_l: Vec<f32> = Vec::new();
    let mut tmp_r: Vec<f32> = Vec::new();
    let mut src_l: Vec<f32> = Vec::new();
    let mut src_r: Vec<f32> = Vec::new();
    let err = |e| crate::store::log(&format!("динамики: {e}"));

    // Заполняет буфер устройства: frames кадров по channels каналов.
    let mut fill = move |frames: usize, write: &mut dyn FnMut(usize, f32)| {
        while fifo_l.len() < frames {
            mix(&playback, 480, &mut mixed); // 10 мс
            if let Some(r) = &render {
                if let Ok(mut q) = r.lock() {
                    q.extend(mixed.chunks_exact(2).map(|p| (p[0] + p[1]) * 0.5));
                    while q.len() > MAX_RENDER {
                        q.pop_front();
                    }
                }
            }
            src_l.clear();
            src_r.clear();
            for pair in mixed.chunks_exact(2) {
                src_l.push(pair[0]);
                src_r.push(pair[1]);
            }
            tmp_l.clear();
            tmp_r.clear();
            rs_l.process(&src_l, &mut tmp_l);
            rs_r.process(&src_r, &mut tmp_r);
            fifo_l.extend(tmp_l.iter().copied());
            fifo_r.extend(tmp_r.iter().copied());
        }
        for f in 0..frames {
            let l = fifo_l.pop_front().unwrap_or(0.0);
            let r = fifo_r.pop_front().unwrap_or(0.0);
            for c in 0..channels {
                let v = match (channels, c) {
                    (1, _) => (l + r) * 0.5,
                    (_, 0) => l,
                    (_, 1) => r,
                    _ => 0.0,
                };
                write(f * channels + c, v);
            }
        }
    };

    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(frames, &mut |i, v| data[i] = v);
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(frames, &mut |i, v| data[i] = (v * 32767.0) as i16);
            },
            err,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(frames, &mut |i, v| {
                    data[i] = ((v * 32767.0) + 32768.0) as u16
                });
            },
            err,
            None,
        ),
        other => return Err(format!("формат динамиков {other:?} не поддерживается")),
    }
    .map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Названия микрофонов и устройств вывода.
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let inputs = host
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    let outputs = host
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    (inputs, outputs)
}

fn find_input(host: &cpal::Host, name: Option<&str>) -> Option<cpal::Device> {
    if let Some(n) = name {
        if let Ok(mut it) = host.input_devices() {
            if let Some(d) = it.find(|d| d.name().map(|x| x == n).unwrap_or(false)) {
                return Some(d);
            }
        }
    }
    host.default_input_device()
}

fn find_output(host: &cpal::Host, name: Option<&str>) -> Option<cpal::Device> {
    if let Some(n) = name {
        if let Ok(mut it) = host.output_devices() {
            if let Some(d) = it.find(|d| d.name().map(|x| x == n).unwrap_or(false)) {
                return Some(d);
            }
        }
    }
    host.default_output_device()
}

impl Audio {
    /// Запускает микрофон и динамики. Без микрофона звонок работает только на приём.
    /// monitor = проверка микрофона: звук с микрофона сразу идёт в наушники.
    pub fn start(
        input: Option<String>,
        output: Option<String>,
        monitor: bool,
    ) -> Result<Audio, String> {
        let capture = Arc::new(Mutex::new(VecDeque::new()));
        let playback: Playback = Arc::new(Mutex::new(HashMap::new()));
        let render = Arc::new(Mutex::new(VecDeque::new()));
        let render_tap = render.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0));
        let (tx, rx) = std::sync::mpsc::channel::<Result<bool, String>>();

        let (cap, pb, st, lv) = (
            capture.clone(),
            playback.clone(),
            stop.clone(),
            level.clone(),
        );
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let host = cpal::default_host();
                let monitor_pb = monitor.then(|| pb.clone());
                let output = match find_output(&host, output.as_deref()) {
                    Some(d) => d,
                    None => {
                        let _ = tx.send(Err("не найдено устройство вывода звука".into()));
                        return;
                    }
                };
                let out_stream = match build_output(&output, pb, Some(render_tap)) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(format!("динамики: {e}")));
                        return;
                    }
                };
                let _ = out_stream.play();
                let in_stream =
                    find_input(&host, input.as_deref()).and_then(|d| {
                        match build_input(&d, cap, lv, monitor_pb) {
                            Ok(s) => Some(s),
                            Err(e) => {
                                crate::store::log(&format!("микрофон недоступен: {e}"));
                                None
                            }
                        }
                    });
                if let Some(s) = &in_stream {
                    let _ = s.play();
                }
                let _ = tx.send(Ok(in_stream.is_some()));
                while !st.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                drop(in_stream);
                drop(out_stream);
            })
            .map_err(|e| e.to_string())?;

        let has_mic = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "звук не запустился".to_string())??;
        Ok(Audio {
            capture,
            playback,
            render,
            has_mic,
            stop,
            level,
        })
    }

    /// Текущая громкость микрофона (RMS, 0..1).
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// Забирает ровно 960 сэмплов (20 мс) с микрофона, если они накопились.
    pub fn take_frame(&self, out: &mut [f32; 960]) -> bool {
        let Ok(mut q) = self.capture.lock() else {
            return false;
        };
        if q.len() < 960 {
            return false;
        }
        for (i, s) in q.drain(..960).enumerate() {
            out[i] = s;
        }
        true
    }

    /// Забирает 480 сэмплов (10 мс) образца вывода для эхоподавления.
    pub fn take_render(&self, out: &mut [f32; 480]) -> bool {
        let Ok(mut q) = self.render.lock() else {
            return false;
        };
        if q.len() < 480 {
            return false;
        }
        for (i, s) in q.drain(..480).enumerate() {
            out[i] = s;
        }
        true
    }

    pub fn clear_capture(&self) {
        if let Ok(mut q) = self.capture.lock() {
            q.clear();
        }
    }
}

/// Проверка динамиков: короткая мелодия из трёх нот на выбранном устройстве.
pub fn play_test_sound(output: Option<String>) -> Result<(), String> {
    std::thread::Builder::new()
        .name("speaker-test".into())
        .spawn(move || {
            let host = cpal::default_host();
            let Some(device) = find_output(&host, output.as_deref()) else {
                crate::store::log("проверка динамиков: устройство не найдено");
                return;
            };
            let playback: Playback = Arc::new(Mutex::new(HashMap::new()));
            let stream = match build_output(&device, playback.clone(), None) {
                Ok(s) => s,
                Err(e) => {
                    crate::store::log(&format!("проверка динамиков: {e}"));
                    return;
                }
            };
            let _ = stream.play();
            // до-ми-соль с мягкой атакой и затуханием
            let mut samples: Vec<f32> = Vec::new();
            for (k, freq) in [523.25f32, 659.25, 783.99].iter().enumerate() {
                let len = (RATE as f32 * if k == 2 { 0.55 } else { 0.28 }) as usize;
                for i in 0..len {
                    let t = i as f32 / RATE as f32;
                    let env = (t * 60.0).min(1.0) * (-t * 5.0).exp();
                    let v = (t * freq * std::f32::consts::TAU).sin() * 0.28 * env
                        + (t * freq * 2.0 * std::f32::consts::TAU).sin() * 0.06 * env;
                    samples.push(v);
                    samples.push(v);
                }
            }
            if let Ok(mut map) = playback.lock() {
                map.insert(0, samples.into_iter().collect());
            }
            std::thread::sleep(Duration::from_millis(1500));
            drop(stream);
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}
