//! Голосовое подключение Discord: voice gateway v8, UDP, транспортное шифрование
//! aead_aes256_gcm_rtpsize, кодек Opus и сквозное шифрование DAVE (через крейт davey).

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU16;
use std::time::{Duration, Instant};

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use crate::audio::{self, Audio};
use crate::discord::{Event, Sink, VoiceUi};
use crate::net::{BoxErr, Proxy};

const SILENCE: [u8; 3] = [0xF8, 0xFF, 0xFE];
/// Сколько кадров по 20 мс продолжать передачу после тишины.
const VAD_HANGOVER: u32 = 15;

pub struct VoiceParams {
    pub guild_id: Option<u64>,
    pub channel_id: u64,
    pub user_id: u64,
    pub session_id: String,
    pub token: String,
    pub endpoint: String,
}

/// Настройки звука для звонка.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceOpts {
    pub input: Option<String>,
    pub output: Option<String>,
    /// Порог «говорит» (RMS).
    pub threshold: f32,
    /// Передавать всегда, без детектора речи.
    pub always: bool,
    /// Простое подавление эха: не передавать тихий звук, пока говорят другие.
    pub echo_guard: bool,
    /// Эхоподавление WebRTC AEC3 (вычитает звук собеседников из микрофона).
    pub echo_cancel: bool,
    /// Шумоподавление: 0 — выкл, 1 — стандартное (WebRTC NS), 2 — усиленное (RNNoise).
    pub noise: u8,
    /// Автоматическая регулировка усиления (WebRTC AGC2).
    pub agc: bool,
    /// Ручное усиление микрофона (0..3).
    pub input_gain: f32,
    /// Рация: передавать, только пока зажата клавиша.
    pub ptt: bool,
    /// Общая громкость собеседников (0..2).
    pub output_volume: f32,
    /// Громкость отдельных собеседников (0 = заглушён).
    pub user_volumes: HashMap<u64, f32>,
}

/// Клавиша рации сейчас зажата (обновляет интерфейс / системный опрос клавиатуры).
pub static PTT_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Обработка микрофона: эхоподавление, шумоподавление и автоусиление.
///
/// Цепочка WebRTC (HPF → AEC3 → NS → AGC2) работает кадрами по 10 мс, RNNoise —
/// тоже по 480 сэмплов, поэтому кадр Discord (20 мс) обрабатывается в два шага.
struct Processor {
    pipeline: Option<SendPipeline>,
    rnnoise: Option<Box<nnnoiseless::DenoiseState<'static>>>,
    gain: f32,
    /// Вероятность речи от RNNoise за последний кадр (для детектора речи).
    voice_prob: f32,
    render: [f32; 480],
}

/// Цепочка aec3 внутри держит `Rc`, но целиком принадлежит одному голосовому
/// соединению и никогда не делится между потоками: переносить её вместе
/// с задачей безопасно.
struct SendPipeline(aec3::pipelines::linear::LinearPipeline);
// SAFETY: все Rc/RefCell внутри конвейера — его собственные, наружу ссылки не выдаются,
// а сам конвейер используется только из одной задачи за раз.
unsafe impl Send for SendPipeline {}

impl Processor {
    fn new(o: &VoiceOpts) -> Processor {
        let want_webrtc = o.echo_cancel || o.noise == 1 || o.agc;
        let pipeline = if want_webrtc {
            let format = aec3::nodes::audio::AudioFormat::ten_ms(audio::RATE, 1);
            match aec3::pipelines::linear::builder(format, format)
                .initial_delay_ms(80)
                .enable_noise_suppression(o.noise == 1)
                .enable_gain_controller2(o.agc)
                .enable_high_pass_filter(true)
                .build()
            {
                Ok(p) => Some(SendPipeline(p)),
                Err(e) => {
                    crate::store::log(&format!("обработка звука WebRTC не запустилась: {e}"));
                    None
                }
            }
        } else {
            None
        };
        Processor {
            pipeline,
            rnnoise: (o.noise == 2).then(nnnoiseless::DenoiseState::new),
            gain: o.input_gain.clamp(0.0, 3.0),
            voice_prob: 1.0,
            render: [0.0; 480],
        }
    }

    fn process(&mut self, frame: &mut [f32; 960], a: &Audio, echo_cancel: bool) {
        if (self.gain - 1.0).abs() > 0.01 {
            for s in frame.iter_mut() {
                *s = (*s * self.gain).clamp(-1.0, 1.0);
            }
        }
        if let Some(SendPipeline(p)) = self.pipeline.as_mut() {
            let mut out = [0f32; 480];
            for half in 0..2 {
                // образец того, что звучит из динамиков, — до обработки микрофона
                if echo_cancel {
                    let mut fed = 0;
                    while fed < 4 && a.take_render(&mut self.render) {
                        let _ = p.handle_render_frame(&self.render);
                        fed += 1;
                    }
                }
                let chunk = &mut frame[half * 480..half * 480 + 480];
                if let Ok(true) = p.process_capture_frame(chunk, &mut out) {
                    chunk.copy_from_slice(&out);
                }
            }
        } else if let Ok(mut q) = a.render.lock() {
            q.clear(); // образец не нужен — не копим его
        }
        if let Some(rn) = self.rnnoise.as_mut() {
            let mut inp = [0f32; 480];
            let mut out = [0f32; 480];
            let mut prob = 0f32;
            for half in 0..2 {
                let chunk = &mut frame[half * 480..half * 480 + 480];
                for (d, s) in inp.iter_mut().zip(chunk.iter()) {
                    *d = *s * 32768.0;
                }
                prob = prob.max(rn.process_frame(&mut out, &inp));
                for (d, s) in chunk.iter_mut().zip(out.iter()) {
                    *d = (*s / 32768.0).clamp(-1.0, 1.0);
                }
            }
            self.voice_prob = prob;
        } else {
            self.voice_prob = 1.0;
        }
    }
}

pub enum Ctl {
    Options(VoiceOpts),
    Mute(bool),
    Deaf(bool),
}

// ======================= DAVE =======================

pub(crate) enum Out {
    Json(String),
    Bin(Vec<u8>),
}

pub(crate) fn bin(op: u8, payload: &[u8]) -> Out {
    let mut v = Vec::with_capacity(payload.len() + 1);
    v.push(op);
    v.extend_from_slice(payload);
    Out::Bin(v)
}

fn ready_json(transition_id: u16) -> Out {
    Out::Json(json!({ "op": 23, "d": { "transition_id": transition_id } }).to_string())
}

/// Состояние сквозного шифрования. Порядок обработки — по описанию протокола DAVE
/// и документации davey.
pub(crate) struct Dave {
    pub(crate) session: Option<davey::DaveSession>,
    pub(crate) version: u16,
    pending: HashMap<u16, u16>,
    pub(crate) downgraded: bool,
    user_id: u64,
    channel_id: u64,
}

// Сессия используется только из одной задачи последовательно.
unsafe impl Send for Dave {}

impl Dave {
    pub(crate) fn new(user_id: u64, channel_id: u64) -> Self {
        Self {
            session: None,
            version: 0,
            pending: HashMap::new(),
            downgraded: false,
            user_id,
            channel_id,
        }
    }

    pub(crate) fn reinit(&mut self, out: &mut Vec<Out>) {
        if let Some(v) = NonZeroU16::new(self.version) {
            match &mut self.session {
                Some(s) => {
                    if let Err(e) = s.reinit(v, self.user_id, self.channel_id, None) {
                        crate::store::log(&format!("DAVE reinit: {e}"));
                    }
                }
                None => match davey::DaveSession::new(v, self.user_id, self.channel_id, None) {
                    Ok(s) => self.session = Some(s),
                    Err(e) => {
                        crate::store::log(&format!("DAVE init: {e}"));
                        return;
                    }
                },
            }
            if let Some(s) = &mut self.session {
                if self.downgraded {
                    s.set_passthrough_mode(true, None);
                }
                match s.create_key_package() {
                    Ok(kp) => out.push(bin(26, &kp)),
                    Err(e) => crate::store::log(&format!("DAVE key package: {e}")),
                }
            }
        } else if let Some(s) = &mut self.session {
            let _ = s.reset();
            s.set_passthrough_mode(true, None);
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.version != 0
            && !self.downgraded
            && self.session.as_ref().map_or(false, |s| s.is_ready())
    }

    fn execute(&mut self, tid: u16) {
        let Some(v) = self.pending.remove(&tid) else {
            return;
        };
        if tid != 0 {
            if let Some(s) = &mut self.session {
                if let Err(e) = s.execute_transition(tid) {
                    crate::store::log(&format!("DAVE execute: {e}"));
                }
            }
        }
        let old = self.version;
        self.version = v;
        if old != v && v == 0 {
            self.downgraded = true;
        } else if tid > 0 && self.downgraded {
            self.downgraded = false;
            if let Some(s) = &mut self.session {
                s.set_passthrough_mode(false, Some(10));
            }
        }
    }

    pub(crate) fn on_json(&mut self, op: u64, d: &Value, out: &mut Vec<Out>) {
        let tid = d["transition_id"].as_u64().unwrap_or(0) as u16;
        match op {
            21 => {
                let pv = d["protocol_version"].as_u64().unwrap_or(0) as u16;
                self.pending.insert(tid, pv);
                if let Some(s) = &mut self.session {
                    if let Err(e) = s.prepare_transition(tid, pv) {
                        crate::store::log(&format!("DAVE prepare: {e}"));
                    }
                }
                if tid == 0 {
                    self.execute(0);
                } else {
                    if pv == 0 {
                        if let Some(s) = &mut self.session {
                            s.set_passthrough_mode(true, None);
                        }
                    }
                    out.push(ready_json(tid));
                }
            }
            22 => self.execute(tid),
            24 => {
                if d["epoch"].as_u64() == Some(1) {
                    self.version = d["protocol_version"].as_u64().unwrap_or(0) as u16;
                    self.reinit(out);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn on_binary(&mut self, op: u8, payload: &[u8], users: &[u64], out: &mut Vec<Out>) {
        let version = self.version;
        let mut failed_transition: Option<u16> = None;
        {
            let Some(s) = self.session.as_mut() else {
                return;
            };
            match op {
                25 => {
                    if let Err(e) = s.set_external_sender(payload) {
                        crate::store::log(&format!("DAVE external sender: {e}"));
                    }
                }
                27 if !payload.is_empty() => {
                    let kind = if payload[0] == 0 {
                        davey::ProposalsOperationType::APPEND
                    } else {
                        davey::ProposalsOperationType::REVOKE
                    };
                    match s.process_proposals(kind, &payload[1..], Some(users)) {
                        Ok(Some(cw)) => {
                            let mut b = cw.commit;
                            if let Some(w) = cw.welcome {
                                b.extend_from_slice(&w);
                            }
                            out.push(bin(28, &b));
                        }
                        Ok(None) => {}
                        Err(e) => crate::store::log(&format!("DAVE proposals: {e}")),
                    }
                }
                29 | 30 if payload.len() >= 2 => {
                    let tid = u16::from_be_bytes([payload[0], payload[1]]);
                    let ok = if op == 29 {
                        match s.process_commit(&payload[2..]) {
                            Ok(true) => true,
                            Ok(false) => return,
                            Err(e) => {
                                crate::store::log(&format!("DAVE commit: {e}"));
                                false
                            }
                        }
                    } else {
                        match s.process_welcome(&payload[2..], Some(users)) {
                            Ok(()) => true,
                            Err(e) => {
                                crate::store::log(&format!("DAVE welcome: {e}"));
                                false
                            }
                        }
                    };
                    if ok {
                        let pv = s.protocol_version().get();
                        if let Err(e) = s.prepare_transition(tid, pv) {
                            crate::store::log(&format!("DAVE prepare: {e}"));
                        }
                        if tid != 0 {
                            self.pending.insert(tid, version);
                            out.push(ready_json(tid));
                        }
                    } else {
                        failed_transition = Some(tid);
                    }
                }
                _ => {}
            }
        }
        if let Some(tid) = failed_transition {
            out.push(Out::Json(
                json!({ "op": 31, "d": { "transition_id": tid } }).to_string(),
            ));
            self.reinit(out);
        }
    }
}

// ======================= Opus =======================

struct Codec {
    encoder: opus::Encoder,
    decoders: HashMap<u32, opus::Decoder>,
}

// Кодеки используются только из одной задачи последовательно.
unsafe impl Send for Codec {}

impl Codec {
    fn new() -> Result<Self, String> {
        let mut encoder =
            opus::Encoder::new(audio::RATE, opus::Channels::Stereo, opus::Application::Voip)
                .map_err(|e| e.to_string())?;
        let _ = encoder.set_bitrate(opus::Bitrate::Bits(64_000));
        Ok(Self {
            encoder,
            decoders: HashMap::new(),
        })
    }
}

// ======================= RTP =======================

struct Rtp {
    ssrc: u32,
    seq: u16,
    timestamp: u32,
    nonce: u32,
}

impl Rtp {
    fn packet(&mut self, cipher: &Aes256Gcm, payload: &[u8]) -> Option<Vec<u8>> {
        let mut pkt = Vec::with_capacity(12 + payload.len() + 16 + 4);
        pkt.extend_from_slice(&[0x80, 0x78]);
        pkt.extend_from_slice(&self.seq.to_be_bytes());
        pkt.extend_from_slice(&self.timestamp.to_be_bytes());
        pkt.extend_from_slice(&self.ssrc.to_be_bytes());
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&self.nonce.to_be_bytes());
        let ct = cipher
            .encrypt(
                GenericArray::from_slice(&nonce),
                Payload {
                    msg: payload,
                    aad: &pkt[..12],
                },
            )
            .ok()?;
        pkt.extend_from_slice(&ct);
        pkt.extend_from_slice(&self.nonce.to_be_bytes());
        self.seq = self.seq.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(960);
        self.nonce = self.nonce.wrapping_add(1);
        Some(pkt)
    }
}

/// Расшифровывает входящий RTP-пакет → (ssrc, opus-кадр).
fn open_rtp(cipher: &Aes256Gcm, pkt: &[u8]) -> Option<(u32, Vec<u8>)> {
    if pkt.len() < 12 + 4 + 16 || pkt[0] >> 6 != 2 || (pkt[1] & 0x7f) != 0x78 {
        return None;
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
    let ssrc = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]);
    Some((ssrc, plain[ext_len..].to_vec()))
}

// ======================= подключение =======================

pub(crate) fn parse_endpoint(endpoint: &str) -> (String, u16) {
    let e = endpoint.trim_start_matches("wss://").trim_end_matches('/');
    match e.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(443)),
        None => (e.to_string(), 443),
    }
}

pub(crate) async fn udp_recv(udp: Option<&UdpSocket>, buf: &mut [u8]) -> std::io::Result<usize> {
    match udp {
        Some(u) => u.recv(buf).await,
        None => std::future::pending().await,
    }
}

/// Случайное начальное значение для RTP (без лишних зависимостей).
pub(crate) fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9);
    nanos ^ (nanos >> 29) ^ 0xA076_1D64_78BD_642F
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub async fn run(
    p: VoiceParams,
    proxy: Option<Proxy>,
    sink: Sink,
    ctl: UnboundedReceiver<Ctl>,
    mute: bool,
    deaf: bool,
    opts: VoiceOpts,
) {
    let result = session(&p, proxy.as_ref(), &sink, ctl, mute, deaf, opts).await;
    if let Some(user) = Some(p.user_id) {
        sink.send(Event::Speaking {
            user_id: user,
            speaking: false,
        });
    }
    match result {
        Ok(()) => sink.send(Event::Voice(VoiceUi::Disconnected(None))),
        Err(e) => {
            crate::store::log(&format!("голос: {e}"));
            sink.send(Event::Voice(VoiceUi::Disconnected(Some(e.to_string()))));
        }
    }
}

async fn session(
    p: &VoiceParams,
    proxy: Option<&Proxy>,
    sink: &Sink,
    mut ctl: UnboundedReceiver<Ctl>,
    mut mute: bool,
    mut deaf: bool,
    mut opts: VoiceOpts,
) -> Result<(), BoxErr> {
    let (host, port) = parse_endpoint(&p.endpoint);
    crate::store::log(&format!("голос: подключаюсь к {host}:{port}"));
    let stream = crate::net::connect(proxy, &host, port).await?;
    let url = format!("wss://{host}/?v=8");
    let (ws, _) = tokio_tungstenite::client_async_tls(url, stream).await?;
    let (mut write, mut read) = ws.split();

    let server_id = p.guild_id.unwrap_or(p.channel_id).to_string();
    let mut seq_ack: i64 = -1;
    let mut heartbeat: Option<tokio::time::Interval> = None;
    let mut hb_sent: Option<Instant> = None;
    let mut ssrc: Option<u32> = None;
    let mut udp: Option<UdpSocket> = None;
    let mut cipher: Option<Aes256Gcm> = None;
    let mut rtp: Option<Rtp> = None;
    let mut dave = Dave::new(p.user_id, p.channel_id);
    let mut users: HashSet<u64> = HashSet::new();
    let mut ssrc_users: HashMap<u32, u64> = HashMap::new();
    let mut audio_io: Option<Audio> = None;
    let mut codec = Codec::new()?;
    let mut processor = Processor::new(&opts);
    audio::set_output_volume(opts.output_volume);
    let mut speaking = false;
    let mut hangover = 0u32;
    let mut last_remote = Instant::now() - Duration::from_secs(10);
    let mut ticks: u64 = 0;
    let (mut sent, mut received, mut dropped) = (0u64, 0u64, 0u64);
    let (mut decrypt_fail, mut unknown_ssrc, mut played) = (0u64, 0u64, 0u64);
    let mut frame = [0f32; 960];
    let mut stereo = vec![0f32; 1920];
    let mut opus_out = vec![0u8; 4000];
    let mut pcm = vec![0f32; 5760 * 2];
    let mut rbuf = vec![0u8; 4096];
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let identify = json!({
        "op": 0,
        "d": {
            "server_id": server_id,
            "user_id": p.user_id.to_string(),
            "session_id": p.session_id,
            "token": p.token,
            "max_dave_protocol_version": davey::DAVE_PROTOCOL_VERSION,
        }
    });
    write.send(WsMsg::Text(identify.to_string())).await?;

    loop {
        let mut out: Vec<Out> = Vec::new();
        tokio::select! {
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err("голосовой сервер закрыл соединение".into()),
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
                                let my_ssrc = d["ssrc"].as_u64().unwrap_or(0) as u32;
                                let ip = d["ip"].as_str().unwrap_or("").to_string();
                                let port = d["port"].as_u64().unwrap_or(0) as u16;
                                let modes: Vec<String> = d["modes"].as_array().map(|a| {
                                    a.iter().filter_map(|m| m.as_str().map(str::to_string)).collect()
                                }).unwrap_or_default();
                                if !modes.iter().any(|m| m == "aead_aes256_gcm_rtpsize") {
                                    return Err("сервер не поддерживает нужный режим шифрования".into());
                                }
                                let socket = UdpSocket::bind("0.0.0.0:0").await?;
                                socket.connect((ip.as_str(), port)).await?;
                                // IP discovery: узнаём свой внешний адрес и порт
                                let mut disc = [0u8; 74];
                                disc[0..2].copy_from_slice(&1u16.to_be_bytes());
                                disc[2..4].copy_from_slice(&70u16.to_be_bytes());
                                disc[4..8].copy_from_slice(&my_ssrc.to_be_bytes());
                                socket.send(&disc).await?;
                                let mut resp = [0u8; 74];
                                let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut resp))
                                    .await
                                    .map_err(|_| "UDP не отвечает — звонкам нужен VPN в режиме «весь трафик» (TUN) или zapret с поддержкой UDP")??;
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
                                        "data": { "address": my_ip, "port": my_port, "mode": "aead_aes256_gcm_rtpsize" }
                                    }
                                }).to_string()));
                                ssrc = Some(my_ssrc);
                                udp = Some(socket);
                            }
                            4 => {
                                let key: Vec<u8> = d["secret_key"].as_array().map(|a| {
                                    a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect()
                                }).unwrap_or_default();
                                cipher = Some(Aes256Gcm::new_from_slice(&key).map_err(|_| "неверный ключ шифрования")?);
                                rtp = Some(Rtp {
                                    ssrc: ssrc.unwrap_or(0),
                                    seq: (seed() & 0xFFFF) as u16,
                                    timestamp: (seed() >> 16) as u32,
                                    nonce: 0,
                                });
                                dave.version = d["dave_protocol_version"].as_u64().unwrap_or(0) as u16;
                                dave.downgraded = dave.version == 0;
                                dave.reinit(&mut out);
                                match Audio::start(opts.input.clone(), opts.output.clone(), false) {
                                    Ok(a) => {
                                        if !a.has_mic {
                                            sink.send(Event::Error("Микрофон не найден — вас не слышно, но вы слышите других".into()));
                                        }
                                        audio_io = Some(a);
                                    }
                                    Err(e) => sink.send(Event::Error(format!("Звук не запустился: {e}"))),
                                }
                                sink.send(Event::Voice(VoiceUi::Connected { ping_ms: 0, e2ee: dave.version > 0 }));
                            }
                            6 => {
                                if let Some(t) = hb_sent.take() {
                                    sink.send(Event::Voice(VoiceUi::Connected {
                                        ping_ms: t.elapsed().as_millis() as u32,
                                        e2ee: dave.active(),
                                    }));
                                }
                            }
                            5 => {
                                let uid: u64 = d["user_id"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                                let s = d["ssrc"].as_u64().unwrap_or(0) as u32;
                                if uid != 0 && s != 0 {
                                    ssrc_users.insert(s, uid);
                                    users.insert(uid);
                                    sink.send(Event::Speaking { user_id: uid, speaking: d["speaking"].as_u64().unwrap_or(0) != 0 });
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
                                    ssrc_users.retain(|_, u| *u != uid);
                                    sink.send(Event::Speaking { user_id: uid, speaking: false });
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
                            4006 => Err("сессия голосового канала недействительна".into()),
                            other => Err(format!("голосовой сервер закрыл соединение (код {other})").into()),
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
                hb_sent = Some(Instant::now());
                out.push(Out::Json(json!({ "op": 3, "d": { "t": now_ms(), "seq_ack": seq_ack } }).to_string()));
            }
            _ = tick.tick(), if cipher.is_some() => {
                let Some(a) = &audio_io else { continue };
                ticks += 1;
                if ticks % 5 == 0 {
                    let lvl = if mute || deaf { 0.0 } else { a.level() };
                    sink.send(Event::MicLevel(lvl));
                }
                if ticks % 500 == 0 {
                    crate::store::log(&format!(
                        "голос: отправлено {sent}, получено {received}, воспроизведено {played}, ошибок расшифровки {decrypt_fail}, чужих ssrc {unknown_ssrc}, отброшено {dropped}; DAVE v{} активно={} шифрование={}; микрофон={}; рация={} эхо={} шум={}",
                        dave.version,
                        dave.active(),
                        dave.session.as_ref().map_or(false, |s| s.can_encrypt()),
                        a.has_mic,
                        opts.ptt,
                        opts.echo_cancel,
                        opts.noise
                    ));
                }
                if mute || deaf || !a.has_mic {
                    a.clear_capture();
                } else {
                    while a.take_frame(&mut frame) {
                        // Решение «говорит / молчит» — по исходному звуку микрофона (с учётом
                        // ручного усиления): шумо- и эхоподавление не должны глушить передачу.
                        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / 960.0).sqrt()
                            * opts.input_gain.max(0.01);
                        processor.process(&mut frame, a, opts.echo_cancel);
                        let echo = opts.echo_guard
                            && !opts.echo_cancel
                            && last_remote.elapsed() < Duration::from_millis(250)
                            && rms < opts.threshold * 3.0;
                        let ptt_down = PTT_DOWN.load(std::sync::atomic::Ordering::Relaxed);
                        let transmit = if opts.ptt {
                            ptt_down
                        } else {
                            opts.always || (rms > opts.threshold && !echo)
                        };
                        if transmit {
                            hangover = if opts.ptt { 3 } else { VAD_HANGOVER };
                        } else {
                            hangover = hangover.saturating_sub(1);
                        }
                        if hangover > 0 {
                            if !speaking {
                                speaking = true;
                                out.push(Out::Json(json!({ "op": 5, "d": { "speaking": 1, "delay": 0, "ssrc": ssrc.unwrap_or(0) } }).to_string()));
                                sink.send(Event::Speaking { user_id: p.user_id, speaking: true });
                            }
                            for (i, s) in frame.iter().enumerate() {
                                stereo[i * 2] = *s;
                                stereo[i * 2 + 1] = *s;
                            }
                            if let Ok(n) = codec.encoder.encode_float(&stereo, &mut opus_out) {
                                let packet = opus_out[..n].to_vec();
                                if send_audio(&mut dave, rtp.as_mut(), cipher.as_ref(), udp.as_ref(), &packet).await {
                                    sent += 1;
                                } else {
                                    dropped += 1;
                                }
                            }
                        }
                        // В очереди могло накопиться несколько кадров — отправляем не больше одного за тик,
                        // остальное догоняется на следующих тиках.
                        break;
                    }
                }
                if speaking && (hangover == 0 || mute || deaf) {
                    speaking = false;
                    for _ in 0..5 {
                        send_audio(&mut dave, rtp.as_mut(), cipher.as_ref(), udp.as_ref(), &SILENCE).await;
                    }
                    out.push(Out::Json(json!({ "op": 5, "d": { "speaking": 0, "delay": 0, "ssrc": ssrc.unwrap_or(0) } }).to_string()));
                    sink.send(Event::Speaking { user_id: p.user_id, speaking: false });
                }
            }
            res = udp_recv(udp.as_ref(), &mut rbuf) => {
                let n = res?;
                if let (Some(c), Some(a)) = (cipher.as_ref(), audio_io.as_ref()) {
                    if let Some((from, payload)) = open_rtp(c, &rbuf[..n]) {
                        received += 1;
                        let user = ssrc_users.get(&from).copied();
                        let mut data = payload;
                        if data.as_slice() != SILENCE.as_slice() {
                            if let (Some(s), Some(uid)) = (dave.session.as_mut(), user) {
                                let can = s.is_ready() && (dave.version != 0 && !dave.downgraded || s.can_passthrough(uid));
                                if can {
                                    match s.decrypt(uid, davey::MediaType::AUDIO, &data) {
                                        Ok(plain) => data = plain,
                                        Err(_) => {
                                            decrypt_fail += 1;
                                            continue;
                                        }
                                    }
                                }
                            } else if dave.active() && user.is_none() {
                                unknown_ssrc += 1;
                                continue; // не знаем, чей это звук — расшифровать нельзя
                            }
                        }
                        if !deaf {
                            let dec = match codec.decoders.entry(from) {
                                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    match opus::Decoder::new(audio::RATE, opus::Channels::Stereo) {
                                        Ok(d) => v.insert(d),
                                        Err(_) => continue,
                                    }
                                }
                            };
                            if let Ok(samples) = dec.decode_float(&data, &mut pcm, false) {
                                let chunk = &pcm[..samples * 2];
                                if !chunk.is_empty() {
                                    let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
                                    if rms > 0.01 {
                                        last_remote = Instant::now();
                                    }
                                }
                                let vol = user
                                    .and_then(|u| opts.user_volumes.get(&u).copied())
                                    .unwrap_or(1.0);
                                if vol <= 0.001 {
                                    continue; // собеседник заглушён у нас
                                }
                                played += 1;
                                if (vol - 1.0).abs() > 0.01 {
                                    let scaled: Vec<f32> = chunk.iter().map(|s| s * vol).collect();
                                    audio::push_playback(&a.playback, from, &scaled);
                                } else {
                                    audio::push_playback(&a.playback, from, chunk);
                                }
                            }
                        }
                    }
                }
            }
            c = ctl.recv() => {
                match c {
                    Some(Ctl::Options(o)) => {
                        let devices_changed = o.input != opts.input || o.output != opts.output;
                        let dsp_changed = o.echo_cancel != opts.echo_cancel
                            || o.noise != opts.noise
                            || o.agc != opts.agc
                            || (o.input_gain - opts.input_gain).abs() > 0.001;
                        opts = o;
                        audio::set_output_volume(opts.output_volume);
                        if dsp_changed {
                            processor = Processor::new(&opts);
                        }
                        if devices_changed && audio_io.is_some() {
                            audio_io = None; // старые потоки останавливаются
                            match Audio::start(opts.input.clone(), opts.output.clone(), false) {
                                Ok(a) => audio_io = Some(a),
                                Err(e) => sink.send(Event::Error(format!("Звук не запустился: {e}"))),
                            }
                        }
                    }
                    Some(Ctl::Mute(m)) => mute = m,
                    Some(Ctl::Deaf(d)) => {
                        deaf = d;
                        if deaf {
                            if let Some(a) = &audio_io {
                                if let Ok(mut map) = a.playback.lock() {
                                    map.clear();
                                }
                            }
                        }
                    }
                    None => return Ok(()),
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

async fn send_audio(
    dave: &mut Dave,
    rtp: Option<&mut Rtp>,
    cipher: Option<&Aes256Gcm>,
    udp: Option<&UdpSocket>,
    packet: &[u8],
) -> bool {
    let (Some(rtp), Some(cipher), Some(udp)) = (rtp, cipher, udp) else {
        return false;
    };
    let mut frame: Vec<u8> = packet.to_vec();
    if dave.active() && packet != SILENCE.as_slice() {
        let Some(s) = dave.session.as_mut() else {
            return false;
        };
        if !s.can_encrypt() {
            return false; // ключа ещё нет — не отправляем, чтобы не утёк незашифрованный звук
        }
        match s.encrypt_opus(packet) {
            Ok(enc) => frame = enc.into_owned(),
            Err(_) => return false,
        }
    }
    match rtp.packet(cipher, &frame) {
        Some(pkt) => udp.send(&pkt).await.is_ok(),
        None => false,
    }
}
