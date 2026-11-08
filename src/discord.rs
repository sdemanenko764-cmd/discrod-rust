//! Связь с Discord: WebSocket gateway (события в реальном времени) и REST API.
//!
//! Всё сетевое живёт в отдельном потоке с однопоточным tokio-рантаймом.
//! Интерфейс шлёт сюда команды (`Cmd`), обратно приходят события (`Event`).
//! Ответы Discord сразу разбираются в компактные структуры: лишние поля
//! (аватары, роли, эмодзи, стикеры, настройки...) не попадают в память вовсе.

use std::{collections::HashMap, sync::mpsc, time::Duration};

use crate::net::Proxy;
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use serde::{de::IgnoredAny, Deserialize};
use serde_json::{json, value::RawValue};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::{protocol::CloseFrame, Message as WsMsg};

const API: &str = "https://discord.com/api/v10";
const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT — только для ботов.
const BOT_INTENTS: u64 = 1 | (1 << 9) | (1 << 12) | (1 << 15);
/// Эпоха Discord для снежинок (id), в миллисекундах.
pub const DISCORD_EPOCH: u64 = 1_420_070_400_000;

// ======================= компактные модели для интерфейса =======================

#[derive(Clone, Debug)]
pub struct Auth {
    pub token: String,
    pub bot: bool,
    /// Что пользователь ввёл в поле «Прокси» (пусто = системный прокси).
    pub proxy: String,
}

impl Auth {
    fn header(&self) -> String {
        if self.bot {
            format!("Bot {}", self.token)
        } else {
            self.token.clone()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: u64,
    pub name: String,
    pub kind: u8,
    pub position: i32,
    pub parent: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Guild {
    pub id: u64,
    pub name: String,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug)]
pub struct Dm {
    pub id: u64,
    pub name: String,
    pub last_message: u64,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: u64,
    pub channel_id: u64,
    pub guild_id: Option<u64>,
    pub author_id: u64,
    pub author: String,
    pub content: String,
    pub time: String,
    pub edited: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Online,
    Reconnecting,
}

pub enum Cmd {
    Login(Auth),
    Logout,
    FetchMessages {
        channel_id: u64,
        before: Option<u64>,
    },
    Send {
        channel_id: u64,
        content: String,
    },
}

pub enum Event {
    Status(Status),
    Ready {
        user_id: u64,
        username: String,
        guilds: Vec<Guild>,
        dms: Vec<Dm>,
    },
    GuildCreate(Guild),
    GuildDelete(u64),
    GuildChannel {
        guild_id: u64,
        channel: Channel,
    },
    DmChannel(Dm),
    ChannelDelete {
        guild_id: Option<u64>,
        id: u64,
    },
    MessageCreate(Msg),
    MessageUpdate {
        id: u64,
        channel_id: u64,
        content: String,
    },
    MessageDelete {
        id: u64,
        channel_id: u64,
    },
    History {
        channel_id: u64,
        before: Option<u64>,
        result: Result<Vec<Msg>, String>,
    },
    Error(String),
    Fatal(String),
}

// ======================= сырые структуры Discord (только нужные поля) =======================

#[derive(Deserialize)]
struct RawUser {
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
}

impl RawUser {
    fn display(&self) -> String {
        match &self.global_name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => self.username.clone(),
        }
    }
}

#[derive(Deserialize)]
struct RawChannel {
    #[serde(default)]
    id: String,
    #[serde(rename = "type", default)]
    kind: u8,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    position: Option<i64>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    recipients: Vec<RawUser>,
    #[serde(default)]
    recipient_ids: Vec<String>,
    #[serde(default)]
    last_message_id: Option<String>,
}

#[derive(Deserialize)]
struct RawGuildProps {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawGuild {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    properties: Option<RawGuildProps>,
    #[serde(default, deserialize_with = "lenient_vec")]
    channels: Vec<RawChannel>,
    #[serde(default)]
    unavailable: Option<bool>,
}

#[derive(Deserialize)]
struct RawReady {
    user: RawUser,
    #[serde(default, deserialize_with = "lenient_vec")]
    guilds: Vec<RawGuild>,
    #[serde(default, deserialize_with = "lenient_vec")]
    private_channels: Vec<RawChannel>,
    #[serde(default, deserialize_with = "lenient_vec")]
    users: Vec<RawUser>,
}

/// Список, в котором неподходящие элементы молча пропускаются, а не ломают весь разбор.
/// Discord иногда присылает поля другого вида — из-за одного странного канала
/// не должен отваливаться вход целиком.
fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let raw: Option<Vec<serde_json::Value>> = Option::deserialize(d)?;
    Ok(raw
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

#[derive(Deserialize)]
struct RawAttachment {
    #[serde(default)]
    filename: String,
}

#[derive(Deserialize)]
struct RawSticker {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawMessage {
    id: String,
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
    author: RawUser,
    #[serde(default)]
    content: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    edited_timestamp: Option<String>,
    #[serde(default)]
    attachments: Vec<RawAttachment>,
    #[serde(default)]
    embeds: Vec<IgnoredAny>,
    #[serde(default)]
    sticker_items: Vec<RawSticker>,
    #[serde(rename = "type", default)]
    kind: u8,
}

#[derive(Deserialize)]
struct RawMessageUpdate {
    id: String,
    channel_id: String,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct RawIds {
    id: String,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    unavailable: Option<bool>,
}

#[derive(Deserialize)]
struct Payload<'a> {
    op: u8,
    #[serde(default, borrow)]
    d: Option<&'a RawValue>,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

#[derive(Deserialize)]
struct Hello {
    heartbeat_interval: u64,
}

// ======================= преобразования =======================

fn sid(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

fn fmt_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d.%m.%Y %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn system_text(kind: u8) -> &'static str {
    match kind {
        1 => "добавил участника в группу",
        2 => "удалил участника из группы",
        3 => "начал звонок",
        4 => "изменил название",
        6 => "закрепил сообщение",
        7 => "присоединился к серверу",
        8 => "забустил сервер",
        _ => "[системное сообщение]",
    }
}

fn convert_msg(m: RawMessage) -> Msg {
    let mut content = m.content;
    for a in &m.attachments {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str("[файл: ");
        content.push_str(&a.filename);
        content.push(']');
    }
    for s in &m.sticker_items {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str("[стикер: ");
        content.push_str(&s.name);
        content.push(']');
    }
    if content.is_empty() && !m.embeds.is_empty() {
        content.push_str("[встроенное содержимое]");
    }
    if content.is_empty() {
        content.push_str(system_text(m.kind));
    }
    content.shrink_to_fit();
    Msg {
        id: sid(&m.id),
        channel_id: sid(&m.channel_id),
        guild_id: m.guild_id.as_deref().map(sid),
        author_id: sid(&m.author.id),
        author: m.author.display(),
        content,
        time: fmt_time(&m.timestamp),
        edited: m.edited_timestamp.is_some(),
    }
}

fn convert_channel(c: &RawChannel) -> Channel {
    Channel {
        id: sid(&c.id),
        name: c.name.clone().unwrap_or_default(),
        kind: c.kind,
        position: c
            .position
            .unwrap_or(0)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        parent: c.parent_id.as_deref().map(sid),
    }
}

fn convert_guild(g: RawGuild) -> Guild {
    let name = g
        .name
        .or_else(|| g.properties.and_then(|p| p.name))
        .unwrap_or_else(|| "Без названия".to_string());
    Guild {
        id: sid(&g.id),
        name,
        channels: g.channels.iter().map(convert_channel).collect(),
    }
}

fn convert_dm(c: &RawChannel, users: &HashMap<String, String>) -> Dm {
    let name = match &c.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ => {
            let mut names: Vec<String> = c.recipients.iter().map(RawUser::display).collect();
            if names.is_empty() {
                names = c
                    .recipient_ids
                    .iter()
                    .filter_map(|id| users.get(id).cloned())
                    .collect();
            }
            if names.is_empty() {
                "Неизвестный".to_string()
            } else {
                names.join(", ")
            }
        }
    };
    Dm {
        id: sid(&c.id),
        name,
        last_message: c.last_message_id.as_deref().map(sid).unwrap_or(0),
    }
}

// ======================= фоновый поток =======================

#[derive(Clone)]
struct Sink {
    tx: mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

/// Запускает сетевой поток. Возвращает канал команд и канал событий.
pub fn spawn(ctx: egui::Context) -> (UnboundedSender<Cmd>, mpsc::Receiver<Event>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let sink = Sink { tx: ev_tx, ctx };
    std::thread::Builder::new()
        .name("discord-net".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("не удалось запустить tokio");
            rt.block_on(backend(cmd_rx, sink));
        })
        .expect("не удалось создать сетевой поток");
    (cmd_tx, ev_rx)
}

fn http_client(proxy: Option<&Proxy>) -> reqwest::Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .user_agent(concat!("Rustcord/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15));
    let builder = match proxy {
        Some(p) => builder.proxy(reqwest::Proxy::all(p.reqwest_url())?),
        None => builder.no_proxy(),
    };
    builder.build()
}

async fn backend(mut rx: UnboundedReceiver<Cmd>, sink: Sink) {
    let mut http = reqwest::Client::new();
    let mut auth: Option<Auth> = None;
    let mut gateway: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Login(a) => {
                if let Some(h) = gateway.take() {
                    h.abort();
                }
                let proxy = crate::net::resolve(&a.proxy);
                let route = proxy
                    .as_ref()
                    .map(Proxy::describe)
                    .unwrap_or_else(|| "напрямую".into());
                crate::store::log(&format!("вход: соединение {route}"));
                http = match http_client(proxy.as_ref()) {
                    Ok(c) => c,
                    Err(e) => {
                        sink.send(Event::Fatal(format!("Неверные настройки прокси: {e}")));
                        continue;
                    }
                };
                gateway = Some(tokio::spawn(gateway_loop(a.clone(), proxy, sink.clone())));
                auth = Some(a);
            }
            Cmd::Logout => {
                if let Some(h) = gateway.take() {
                    h.abort();
                }
                auth = None;
            }
            Cmd::FetchMessages { channel_id, before } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        let result = fetch_messages(&http, &a, channel_id, before).await;
                        sink.send(Event::History {
                            channel_id,
                            before,
                            result,
                        });
                    });
                }
            }
            Cmd::Send {
                channel_id,
                content,
            } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        match send_message(&http, &a, channel_id, &content).await {
                            Ok(m) => sink.send(Event::MessageCreate(m)),
                            Err(e) => sink.send(Event::Error(e)),
                        }
                    });
                }
            }
        }
    }
}

// ======================= REST =======================

fn http_error(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 => "Токен недействителен".into(),
        403 => "Нет доступа к этому каналу".into(),
        404 => "Канал не найден".into(),
        429 => "Слишком много запросов — подождите немного".into(),
        code => format!("Ошибка Discord: HTTP {code}"),
    }
}

async fn fetch_messages(
    http: &reqwest::Client,
    auth: &Auth,
    channel_id: u64,
    before: Option<u64>,
) -> Result<Vec<Msg>, String> {
    let mut url = format!("{API}/channels/{channel_id}/messages?limit=50");
    if let Some(b) = before {
        url.push_str(&format!("&before={b}"));
    }
    let resp = http
        .get(&url)
        .header("Authorization", auth.header())
        .send()
        .await
        .map_err(|e| format!("Нет связи с Discord: {e}"))?;
    if !resp.status().is_success() {
        return Err(http_error(resp.status()));
    }
    let raw: Vec<RawMessage> = resp
        .json()
        .await
        .map_err(|e| format!("Неожиданный ответ Discord: {e}"))?;
    let mut msgs: Vec<Msg> = raw.into_iter().map(convert_msg).collect();
    msgs.reverse(); // Discord отдаёт от новых к старым
    Ok(msgs)
}

async fn send_message(
    http: &reqwest::Client,
    auth: &Auth,
    channel_id: u64,
    content: &str,
) -> Result<Msg, String> {
    let resp = http
        .post(format!("{API}/channels/{channel_id}/messages"))
        .header("Authorization", auth.header())
        .json(&json!({ "content": content }))
        .send()
        .await
        .map_err(|e| format!("Не отправлено, нет связи: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Не отправлено: {}", http_error(resp.status())));
    }
    let raw: RawMessage = resp
        .json()
        .await
        .map_err(|e| format!("Неожиданный ответ Discord: {e}"))?;
    Ok(convert_msg(raw))
}

// ======================= Gateway =======================

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

enum End {
    Reconnect(String),
    Fatal(String),
}

/// После стольких неудачных попыток подряд при ПЕРВОМ входе — показываем ошибку,
/// а не крутим «Подключение…» бесконечно.
const MAX_LOGIN_ATTEMPTS: u32 = 3;

async fn gateway_loop(auth: Auth, proxy: Option<Proxy>, sink: Sink) {
    let mut delay: u64 = 1;
    let mut ever_ready = false;
    let mut failures: u32 = 0;
    loop {
        sink.send(Event::Status(if ever_ready || failures > 0 {
            Status::Reconnecting
        } else {
            Status::Connecting
        }));
        let mut got_ready = false;
        let reason = match session(&auth, proxy.as_ref(), &sink, &mut got_ready).await {
            Ok(End::Fatal(msg)) => {
                crate::store::log(&format!("ФАТАЛЬНО: {msg}"));
                sink.send(Event::Fatal(msg));
                return;
            }
            Ok(End::Reconnect(r)) => r,
            Err(e) => format!("ошибка соединения: {e}"),
        };
        crate::store::log(&format!("gateway: {reason}"));
        if got_ready {
            ever_ready = true;
            failures = 0;
            delay = 1;
        } else {
            failures += 1;
        }
        if !ever_ready {
            if failures >= MAX_LOGIN_ATTEMPTS {
                let hint = if proxy.is_none() {
                    "\n\nПохоже, соединение с Discord блокируется. Включите VPN в режиме «весь трафик» \
                     (TUN), либо укажите в поле «Прокси» локальный прокси вашего VPN-клиента, \
                     например socks5://127.0.0.1:10808."
                } else {
                    "\n\nПроверьте, что VPN-клиент запущен и адрес прокси указан верно."
                };
                sink.send(Event::Fatal(format!(
                    "Не удалось войти: {reason}.{hint}\n\nЛог: {}",
                    crate::store::log_path_display()
                )));
                return;
            }
            sink.send(Event::Error(format!("Попытка {failures}: {reason}")));
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

fn identify(auth: &Auth) -> String {
    let mut d = json!({
        "token": auth.token,
        "properties": {
            "os": std::env::consts::OS,
            "browser": "Rustcord",
            "device": "Rustcord"
        }
    });
    if auth.bot {
        d["intents"] = json!(BOT_INTENTS);
    }
    json!({ "op": 2, "d": d }).to_string()
}

fn heartbeat(seq: Option<u64>) -> String {
    json!({ "op": 1, "d": seq }).to_string()
}

fn close_end(frame: Option<CloseFrame<'_>>) -> End {
    let (code, text) = frame
        .map(|f| (u16::from(f.code), f.reason.to_string()))
        .unwrap_or((1000, String::new()));
    crate::store::log(&format!("gateway закрыт: код {code} {text}"));
    match code {
        4004 => End::Fatal(
            "Discord не принял токен (код 4004).\n\
             • Токен бота — поставьте галочку «Это токен бота».\n\
             • Токен аккаунта — галочку уберите.\n\
             • Если меняли пароль или сбрасывали токен — нужен новый токен."
                .into(),
        ),
        4013 => End::Fatal("Discord отклонил набор intents бота (код 4013).".into()),
        4014 => End::Fatal(
            "У бота не включён Message Content Intent (код 4014). \
             Включите его: Discord Developer Portal → ваше приложение → Bot."
                .into(),
        ),
        4010..=4012 => End::Fatal(format!("Discord закрыл соединение (код {code}).")),
        _ if text.is_empty() => End::Reconnect(format!("Discord закрыл соединение (код {code})")),
        _ => End::Reconnect(format!("Discord закрыл соединение (код {code}: {text})")),
    }
}

async fn session(
    auth: &Auth,
    proxy: Option<&Proxy>,
    sink: &Sink,
    got_ready: &mut bool,
) -> Result<End, BoxErr> {
    let stream = crate::net::connect(proxy, "gateway.discord.gg", 443).await?;
    let (ws, _) = tokio_tungstenite::client_async_tls(GATEWAY, stream).await?;
    let (mut write, mut read) = ws.split();

    // 1. Ждём Hello с интервалом heartbeat.
    let interval_ms = loop {
        match read.next().await {
            Some(Ok(WsMsg::Text(text))) => {
                let p: Payload = serde_json::from_str(&text)?;
                if p.op == 10 {
                    let hello: Hello = serde_json::from_str(p.d.map(|d| d.get()).unwrap_or("{}"))?;
                    break hello.heartbeat_interval;
                }
            }
            Some(Ok(WsMsg::Close(f))) => return Ok(close_end(f)),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
            None => {
                return Ok(End::Reconnect(
                    "соединение оборвалось до приветствия".into(),
                ))
            }
        }
    };

    // 2. Представляемся.
    write.send(WsMsg::Text(identify(auth))).await?;

    let period = Duration::from_millis(interval_ms.max(1000));
    let mut hb = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let mut seq: Option<u64> = None;
    let mut acked = true;
    let ready_timeout = tokio::time::sleep(Duration::from_secs(40));
    tokio::pin!(ready_timeout);

    // 3. Основной цикл: heartbeat + события.
    loop {
        tokio::select! {
            _ = &mut ready_timeout, if !*got_ready => {
                return Ok(End::Reconnect("Discord не ответил на вход за 40 секунд".into()));
            }
            _ = hb.tick() => {
                if !acked {
                    return Ok(End::Reconnect("сервер перестал отвечать на heartbeat".into()));
                }
                acked = false;
                write.send(WsMsg::Text(heartbeat(seq))).await?;
            }
            msg = read.next() => match msg {
                Some(Ok(WsMsg::Text(text))) => {
                    match serde_json::from_str::<Payload>(&text) {
                        Ok(p) => {
                            if let Some(s) = p.s {
                                seq = Some(s);
                            }
                            match p.op {
                                0 => {
                                    let t = p.t.as_deref().unwrap_or("");
                                    let d = p.d.map(|d| d.get()).unwrap_or("null");
                                    match handle_dispatch(t, d, sink) {
                                        Ok(()) if t == "READY" => {
                                            *got_ready = true;
                                            crate::store::log("вход выполнен (READY)");
                                        }
                                        Ok(()) => {}
                                        Err(e) if t == "READY" => {
                                            return Ok(End::Fatal(format!(
                                                "Discord ответил на вход, но ответ не удалось разобрать: {e}"
                                            )));
                                        }
                                        Err(e) => crate::store::log(&format!("не удалось разобрать {t}: {e}")),
                                    }
                                }
                                1 => write.send(WsMsg::Text(heartbeat(seq))).await?,
                                7 => return Ok(End::Reconnect("Discord попросил переподключиться".into())),
                                9 => {
                                    tokio::time::sleep(Duration::from_secs(3)).await;
                                    return Ok(End::Reconnect("Discord отклонил сессию (invalid session)".into()));
                                }
                                11 => acked = true,
                                _ => {}
                            }
                        }
                        Err(e) => crate::store::log(&format!("gateway: плохой пакет: {e}")),
                    }
                }
                Some(Ok(WsMsg::Close(f))) => return Ok(close_end(f)),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(End::Reconnect("соединение оборвалось".into())),
            }
        }
    }
}

/// Разбирает событие и отправляет в интерфейс только то, что нужно.
/// Неизвестные события пропускаются без разбора — это экономит и CPU, и память.
fn handle_dispatch(t: &str, d: &str, sink: &Sink) -> serde_json::Result<()> {
    match t {
        "READY" => {
            let r: RawReady = serde_json::from_str(d)?;
            let users: HashMap<String, String> = r
                .users
                .iter()
                .map(|u| (u.id.clone(), u.display()))
                .collect();
            let mut dms: Vec<Dm> = r
                .private_channels
                .iter()
                .filter(|c| c.kind == 1 || c.kind == 3)
                .map(|c| convert_dm(c, &users))
                .collect();
            dms.sort_by(|a, b| b.last_message.cmp(&a.last_message));
            let guilds: Vec<Guild> = r
                .guilds
                .into_iter()
                .filter(|g| g.unavailable != Some(true) || !g.channels.is_empty())
                .map(convert_guild)
                .collect();
            sink.send(Event::Ready {
                user_id: sid(&r.user.id),
                username: r.user.display(),
                guilds,
                dms,
            });
        }
        "GUILD_CREATE" => {
            let g: RawGuild = serde_json::from_str(d)?;
            if g.unavailable != Some(true) {
                sink.send(Event::GuildCreate(convert_guild(g)));
            }
        }
        "GUILD_DELETE" => {
            let x: RawIds = serde_json::from_str(d)?;
            if x.unavailable != Some(true) {
                sink.send(Event::GuildDelete(sid(&x.id)));
            }
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
            let c: RawChannel = serde_json::from_str(d)?;
            match c.guild_id.as_deref() {
                Some(g) => sink.send(Event::GuildChannel {
                    guild_id: sid(g),
                    channel: convert_channel(&c),
                }),
                None if c.kind == 1 || c.kind == 3 => {
                    sink.send(Event::DmChannel(convert_dm(&c, &HashMap::new())))
                }
                None => {}
            }
        }
        "CHANNEL_DELETE" => {
            let c: RawChannel = serde_json::from_str(d)?;
            sink.send(Event::ChannelDelete {
                guild_id: c.guild_id.as_deref().map(sid),
                id: sid(&c.id),
            });
        }
        "MESSAGE_CREATE" => {
            let m: RawMessage = serde_json::from_str(d)?;
            sink.send(Event::MessageCreate(convert_msg(m)));
        }
        "MESSAGE_UPDATE" => {
            let m: RawMessageUpdate = serde_json::from_str(d)?;
            if let Some(content) = m.content {
                sink.send(Event::MessageUpdate {
                    id: sid(&m.id),
                    channel_id: sid(&m.channel_id),
                    content,
                });
            }
        }
        "MESSAGE_DELETE" => {
            let x: RawIds = serde_json::from_str(d)?;
            sink.send(Event::MessageDelete {
                id: sid(&x.id),
                channel_id: x.channel_id.as_deref().map(sid).unwrap_or(0),
            });
        }
        _ => {}
    }
    Ok(())
}
