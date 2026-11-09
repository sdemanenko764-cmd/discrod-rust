//! Связь с Discord: WebSocket gateway (события в реальном времени) и REST API.
//!
//! Всё сетевое живёт в отдельном потоке с однопоточным tokio-рантаймом.
//! Интерфейс шлёт сюда команды (`Cmd`), обратно приходят события (`Event`).
//! Ответы Discord сразу разбираются в компактные структуры: лишние поля
//! не попадают в память вовсе. Картинки скачиваются и уменьшаются здесь же,
//! в фоне, чтобы окно не подвисало.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};

use crate::net::Proxy;
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::{protocol::CloseFrame, Message as WsMsg};

const API: &str = "https://discord.com/api/v10";
const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT — только для ботов.
const BOT_INTENTS: u64 = 1 | (1 << 9) | (1 << 12) | (1 << 15);
/// Эпоха Discord для снежинок (id), в миллисекундах.
pub const DISCORD_EPOCH: u64 = 1_420_070_400_000;
/// Лимит Discord на файл без подписки.
pub const MAX_UPLOAD: u64 = 10 * 1024 * 1024;
const MAX_IMAGE_DOWNLOAD: u64 = 16 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 90;

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
pub struct VoiceMember {
    pub user_id: u64,
    pub channel_id: u64,
    pub muted: bool,
    pub deafened: bool,
    pub streaming: bool,
    pub name: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Clone, Debug)]
pub enum VoiceUi {
    Connecting {
        guild_id: Option<u64>,
        channel_id: u64,
    },
    Connected {
        ping_ms: u32,
        e2ee: bool,
    },
    Disconnected(Option<String>),
}

/// Внутренние сигналы для голосового менеджера (из основного gateway).
pub(crate) enum VoiceSignal {
    State {
        session_id: String,
        channel_id: Option<u64>,
    },
    Server {
        token: String,
        endpoint: String,
    },
    StreamCreate {
        key: String,
        rtc_server_id: u64,
    },
    StreamServer {
        key: String,
        token: String,
        endpoint: String,
    },
    StreamDelete {
        key: String,
        reason: Option<String>,
    },
}

static ME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static VOICE_SIGNAL: std::sync::OnceLock<UnboundedSender<VoiceSignal>> = std::sync::OnceLock::new();

fn voice_signal(sig: VoiceSignal) {
    if let Some(tx) = VOICE_SIGNAL.get() {
        let _ = tx.send(sig);
    }
}

#[derive(Clone, Debug)]
pub struct Guild {
    pub voice: Vec<VoiceMember>,
    pub id: u64,
    pub name: String,
    pub icon: Option<String>,
    /// id роли -> (название, цвет)
    pub roles: HashMap<String, (String, u32)>,
    pub channels: Vec<Channel>,
    /// Свои эмодзи сервера.
    pub emojis: Vec<Emoji>,
}

/// Друг, заявка или заблокированный (тип: 1 — друг, 2 — блок, 3 — входящая, 4 — исходящая).
#[derive(Clone, Debug)]
pub struct Friend {
    pub id: u64,
    pub kind: u8,
    pub name: String,
    pub username: String,
    pub avatar: Option<String>,
}

/// Что запросили через универсальный REST-запрос — по этому интерфейс понимает ответ.
#[derive(Clone, Debug)]
pub enum ApiTag {
    Profile(u64),
    Pins(u64),
    Search,
    Threads(u64),
    OpenDm,
    Invite,
    /// Показать этот текст, когда запрос удался.
    Done(String),
    Silent,
}

#[derive(Clone, Debug)]
pub struct Dm {
    pub id: u64,
    pub name: String,
    pub last_message: u64,
    /// (id собеседника, хэш аватарки) — только для личных диалогов 1 на 1.
    pub avatar: Option<(u64, Option<String>)>,
    pub group: bool,
    /// Собеседники: (id, имя, хэш аватарки).
    pub recipients: Vec<(u64, String, Option<String>)>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub filename: String,
    pub size: u64,
    pub url: String,
    pub proxy_url: String,
    pub content_type: String,
    pub width: u32,
    pub height: u32,
}

impl Attachment {
    fn ext(&self) -> String {
        self.filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default()
    }

    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
            || matches!(self.ext().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
    }

    pub fn is_gif(&self) -> bool {
        self.content_type == "image/gif" || self.ext() == "gif"
    }

    pub fn is_video(&self) -> bool {
        self.content_type.starts_with("video/")
            || matches!(self.ext().as_str(), "mp4" | "webm" | "mov" | "mkv")
    }
}

#[derive(Clone, Debug)]
pub struct Media {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct Embed {
    pub kind: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub provider: Option<String>,
    pub color: Option<u32>,
    /// Картинка/превью для показа.
    pub media: Option<Media>,
    /// Прямая ссылка на анимированный GIF, если удалось её получить.
    pub gif_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub author: String,
    pub content: String,
}

/// Эмодзи: обычный юникод (id = None) или кастомный эмодзи сервера.
#[derive(Clone, Debug, PartialEq)]
pub struct Emoji {
    pub id: Option<u64>,
    pub name: String,
    pub animated: bool,
}

impl Emoji {
    pub fn unicode(s: &str) -> Self {
        Self {
            id: None,
            name: s.to_string(),
            animated: false,
        }
    }

    /// Как эмодзи записывается в адресе запроса реакции.
    fn api_key(&self) -> String {
        match self.id {
            Some(id) => format!("{}:{id}", self.name),
            None => self.name.clone(),
        }
    }

    pub fn image_url(&self) -> String {
        match self.id {
            Some(id) => custom_emoji_url(id, self.animated),
            None => crate::emoji::twemoji_url(&self.name),
        }
    }
}

pub fn custom_emoji_url(id: u64, animated: bool) -> String {
    let ext = if animated { "gif" } else { "png" };
    format!("https://cdn.discordapp.com/emojis/{id}.{ext}?size=48")
}

#[derive(Clone, Debug)]
pub struct Reaction {
    pub emoji: Emoji,
    pub count: u32,
    pub me: bool,
}

/// Строка списка участников: заголовок группы или участник.
#[derive(Clone, Debug)]
pub enum ListItem {
    Group {
        id: String,
        count: u64,
    },
    Member {
        id: u64,
        name: String,
        avatar: Option<String>,
        status: String,
    },
}

pub enum ListOp {
    Sync { start: usize, items: Vec<ListItem> },
    Insert { index: usize, item: ListItem },
    Update { index: usize, item: ListItem },
    Delete { index: usize },
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: u64,
    pub channel_id: u64,
    pub guild_id: Option<u64>,
    pub author_id: u64,
    pub author: String,
    pub author_avatar: Option<String>,
    pub content: String,
    pub time: String,
    pub edited: bool,
    pub system: bool,
    pub attachments: Vec<Attachment>,
    pub embeds: Vec<Embed>,
    pub reply: Option<Reply>,
    pub reactions: Vec<Reaction>,
    /// Упомянутые пользователи: (id, имя).
    pub mentions: Vec<(u64, String)>,
    pub mention_everyone: bool,
    pub mention_roles: Vec<u64>,
    pub pinned: bool,
    /// Ветка, созданная из сообщения: (id, название, сообщений).
    pub thread: Option<(u64, String, u32)>,
}

/// Один кадр картинки (у статичной — один, у GIF — много).
pub struct Frame {
    pub image: egui::ColorImage,
    pub delay_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Online,
    Reconnecting,
}

pub enum Cmd {
    /// Универсальный REST-запрос; ответ придёт как `Event::Api`.
    Api {
        method: &'static str,
        path: String,
        body: Option<serde_json::Value>,
        tag: ApiTag,
    },
    /// Сменить свой статус: online / idle / dnd / invisible и свой текст.
    SetPresence {
        status: String,
        custom: String,
    },
    JoinVoice {
        guild_id: Option<u64>,
        channel_id: u64,
    },
    LeaveVoice,
    SetMute(bool),
    SetDeaf(bool),
    RingCall(u64),
    DeclineCall(u64),
    VoiceOptions(crate::voice::VoiceOpts),
    StartStream(crate::stream::SendCfg),
    StopStream,
    WatchStream(String),
    StopWatching,
    Login(Auth),
    Logout,
    FetchMessages {
        channel_id: u64,
        before: Option<u64>,
    },
    Send {
        channel_id: u64,
        content: String,
        reply_to: Option<u64>,
    },
    Edit {
        channel_id: u64,
        message_id: u64,
        content: String,
    },
    Delete {
        channel_id: u64,
        message_id: u64,
    },
    React {
        channel_id: u64,
        message_id: u64,
        emoji: Emoji,
        add: bool,
    },
    Typing {
        channel_id: u64,
    },
    SubscribeMembers {
        guild_id: u64,
        channel_id: u64,
    },
    Upload {
        channel_id: u64,
        path: PathBuf,
        content: String,
    },
    FetchImage {
        key: String,
        url: String,
        max_side: u32,
        animate: bool,
    },
    Download {
        url: String,
        filename: String,
    },
}

pub enum Event {
    Status(Status),
    Ready {
        user_id: u64,
        username: String,
        avatar: Option<String>,
        guilds: Vec<Guild>,
        dms: Vec<Dm>,
        presences: Vec<(u64, String)>,
        friends: Vec<Friend>,
        /// статус и свой текст из настроек аккаунта
        status: Option<String>,
        custom_status: Option<String>,
    },
    Presence {
        user_id: u64,
        status: String,
    },
    Relationship {
        friend: Option<Friend>,
        removed: Option<u64>,
    },
    Api {
        tag: ApiTag,
        result: Result<String, String>,
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
    Image {
        key: String,
        result: Result<Vec<Frame>, String>,
    },
    Reaction {
        channel_id: u64,
        message_id: u64,
        user_id: u64,
        emoji: Emoji,
        add: bool,
    },
    ReactionsCleared {
        channel_id: u64,
        message_id: u64,
    },
    Typing {
        channel_id: u64,
        user_id: u64,
        name: Option<String>,
    },
    MemberList {
        guild_id: u64,
        groups: Vec<(String, u64)>,
        ops: Vec<ListOp>,
    },
    Voice(VoiceUi),
    MicLevel(f32),
    Speaking {
        user_id: u64,
        speaking: bool,
    },
    VoiceState {
        guild_id: Option<u64>,
        member: VoiceMember,
        left: bool,
    },
    CallRing {
        channel_id: u64,
        ringing: bool,
    },
    CallEnded {
        channel_id: u64,
    },
    /// Наша трансляция пошла в эфир (отправлен первый кадр).
    StreamLive {
        key: String,
    },
    StreamFrame {
        key: String,
        image: egui::ColorImage,
    },
    StreamEnded {
        key: String,
        error: Option<String>,
    },
    Info(String),
    Error(String),
    Fatal(String),
}

// ======================= ссылки на CDN =======================

pub fn avatar_url(user_id: u64, hash: &str) -> String {
    format!("https://cdn.discordapp.com/avatars/{user_id}/{hash}.png?size=64")
}

pub fn guild_icon_url(guild_id: u64, hash: &str) -> String {
    format!("https://cdn.discordapp.com/icons/{guild_id}/{hash}.png?size=96")
}

/// Ссылка на уменьшенную копию через медиапрокси Discord (экономит трафик и память).
pub fn resized_url(url: &str, width: u32, height: u32) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}width={width}&height={height}")
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
    #[serde(default)]
    avatar: Option<String>,
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
    #[serde(default)]
    message_count: Option<u32>,
}

#[derive(Deserialize)]
struct RawGuildProps {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
}

#[derive(Deserialize)]
struct RawRole {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: u32,
}

#[derive(Deserialize)]
struct RawVoiceState {
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    self_mute: bool,
    #[serde(default)]
    self_deaf: bool,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    deaf: bool,
    #[serde(default)]
    self_stream: bool,
    #[serde(default)]
    member: Option<RawTypingMember>,
}

impl RawVoiceState {
    fn member(&self) -> Option<VoiceMember> {
        let channel_id = self.channel_id.as_deref().map(sid)?;
        let (name, avatar) = match &self.member {
            Some(m) => (
                m.nick
                    .clone()
                    .filter(|n| !n.is_empty())
                    .or_else(|| m.user.as_ref().map(|u| u.display())),
                m.user.as_ref().and_then(|u| u.avatar.clone()),
            ),
            None => (None, None),
        };
        Some(VoiceMember {
            user_id: sid(&self.user_id),
            channel_id,
            muted: self.self_mute || self.mute,
            deafened: self.self_deaf || self.deaf,
            streaming: self.self_stream,
            name,
            avatar,
        })
    }
}

#[derive(Deserialize)]
struct RawStream {
    #[serde(default)]
    stream_key: String,
    #[serde(default)]
    rtc_server_id: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct RawVoiceServer {
    #[serde(default)]
    token: String,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Deserialize)]
struct RawCall {
    channel_id: String,
    #[serde(default)]
    ringing: Vec<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    voice_states: Vec<RawVoiceState>,
}

#[derive(Deserialize)]
struct RawGuild {
    #[serde(default, deserialize_with = "lenient_vec")]
    voice_states: Vec<RawVoiceState>,
    #[serde(default, deserialize_with = "lenient_vec")]
    roles: Vec<RawRole>,
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    properties: Option<RawGuildProps>,
    #[serde(default, deserialize_with = "lenient_vec")]
    channels: Vec<RawChannel>,
    #[serde(default, deserialize_with = "lenient_vec")]
    threads: Vec<RawChannel>,
    #[serde(default, deserialize_with = "lenient_vec")]
    emojis: Vec<RawEmoji>,
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
    #[serde(default, deserialize_with = "lenient_vec")]
    relationships: Vec<RawRelationship>,
    #[serde(default, deserialize_with = "lenient_vec")]
    presences: Vec<RawPresenceUpdate>,
    #[serde(default)]
    user_settings: Option<RawUserSettings>,
}

#[derive(Deserialize)]
struct RawRelationship {
    #[serde(default)]
    id: String,
    #[serde(rename = "type", default)]
    kind: u8,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    nickname: Option<String>,
}

impl RawRelationship {
    fn convert(self, users: &HashMap<String, (String, Option<String>)>) -> Friend {
        let (name, username, avatar) = match &self.user {
            Some(u) => (u.display(), u.username.clone(), u.avatar.clone()),
            None => match users.get(&self.id) {
                Some((n, a)) => (n.clone(), n.clone(), a.clone()),
                None => ("Неизвестный".into(), String::new(), None),
            },
        };
        Friend {
            id: sid(&self.id),
            kind: self.kind,
            name: self.nickname.filter(|n| !n.is_empty()).unwrap_or(name),
            username,
            avatar,
        }
    }
}

#[derive(Deserialize)]
struct RawIdOnly {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
struct RawPresenceUpdate {
    #[serde(default)]
    user: Option<RawIdOnly>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    status: String,
}

impl RawPresenceUpdate {
    fn convert(&self) -> Option<(u64, String)> {
        let id = self
            .user
            .as_ref()
            .map(|u| u.id.clone())
            .or_else(|| self.user_id.clone())?;
        Some((sid(&id), self.status.clone()))
    }
}

#[derive(Deserialize)]
struct RawCustomStatus {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct RawUserSettings {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    custom_status: Option<RawCustomStatus>,
}

#[derive(Deserialize)]
struct RawThreadList {
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    threads: Vec<RawChannel>,
}

/// Список, в котором неподходящие элементы молча пропускаются, а не ломают весь разбор.
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
    #[serde(default)]
    size: u64,
    #[serde(default)]
    url: String,
    #[serde(default)]
    proxy_url: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Deserialize)]
struct RawSticker {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    format_type: u8,
}

#[derive(Deserialize)]
struct RawMedia {
    #[serde(default)]
    url: String,
    #[serde(default)]
    proxy_url: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Deserialize)]
struct RawProvider {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawEmbed {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    color: Option<u32>,
    #[serde(default)]
    provider: Option<RawProvider>,
    #[serde(default)]
    thumbnail: Option<RawMedia>,
    #[serde(default)]
    image: Option<RawMedia>,
    #[serde(default)]
    video: Option<RawMedia>,
}

#[derive(Deserialize)]
struct RawEmoji {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    animated: bool,
}

impl RawEmoji {
    fn convert(&self) -> Emoji {
        Emoji {
            id: self.id.as_deref().map(sid).filter(|i| *i != 0),
            name: self.name.clone().unwrap_or_default(),
            animated: self.animated,
        }
    }
}

#[derive(Deserialize)]
struct RawReaction {
    #[serde(default)]
    count: u32,
    #[serde(default)]
    me: bool,
    emoji: RawEmoji,
}

#[derive(Deserialize)]
struct RawReactionEvent {
    #[serde(default)]
    user_id: String,
    channel_id: String,
    message_id: String,
    emoji: RawEmoji,
}

#[derive(Deserialize)]
struct RawReactionClear {
    channel_id: String,
    message_id: String,
}

#[derive(Deserialize)]
struct RawTypingMember {
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    nick: Option<String>,
}

#[derive(Deserialize)]
struct RawTyping {
    channel_id: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    member: Option<RawTypingMember>,
}

#[derive(Deserialize)]
struct RawPresence {
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct RawListMember {
    user: RawUser,
    #[serde(default)]
    nick: Option<String>,
    #[serde(default)]
    presence: Option<RawPresence>,
}

#[derive(Deserialize)]
struct RawGroup {
    #[serde(default)]
    id: String,
    #[serde(default)]
    count: u64,
}

#[derive(Deserialize)]
struct RawListItem {
    #[serde(default)]
    group: Option<RawGroup>,
    #[serde(default)]
    member: Option<RawListMember>,
}

impl RawListItem {
    fn convert(self) -> Option<ListItem> {
        if let Some(g) = self.group {
            return Some(ListItem::Group {
                id: g.id,
                count: g.count,
            });
        }
        let m = self.member?;
        let name = m
            .nick
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| m.user.display());
        Some(ListItem::Member {
            id: sid(&m.user.id),
            name,
            avatar: m.user.avatar,
            status: m
                .presence
                .map(|p| p.status)
                .unwrap_or_else(|| "offline".into()),
        })
    }
}

#[derive(Deserialize)]
struct RawListOp {
    #[serde(default)]
    op: String,
    #[serde(default)]
    range: Option<Vec<usize>>,
    #[serde(default, deserialize_with = "lenient_vec")]
    items: Vec<RawListItem>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    item: Option<RawListItem>,
}

#[derive(Deserialize)]
struct RawMemberList {
    guild_id: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    groups: Vec<RawGroup>,
    #[serde(default, deserialize_with = "lenient_vec")]
    ops: Vec<RawListOp>,
}

#[derive(Deserialize)]
struct RawRef {
    #[serde(default)]
    author: Option<RawUser>,
    #[serde(default)]
    content: String,
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
    #[serde(default, deserialize_with = "lenient_vec")]
    attachments: Vec<RawAttachment>,
    #[serde(default, deserialize_with = "lenient_vec")]
    embeds: Vec<RawEmbed>,
    #[serde(default, deserialize_with = "lenient_vec")]
    sticker_items: Vec<RawSticker>,
    #[serde(default, deserialize_with = "lenient_vec")]
    stickers: Vec<RawSticker>,
    #[serde(default, deserialize_with = "lenient_vec")]
    mentions: Vec<RawUser>,
    #[serde(default)]
    referenced_message: Option<Box<RawRef>>,
    #[serde(default, deserialize_with = "lenient_vec")]
    reactions: Vec<RawReaction>,
    #[serde(rename = "type", default)]
    kind: u8,
    #[serde(default)]
    mention_everyone: bool,
    #[serde(default)]
    mention_roles: Vec<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    thread: Option<RawChannel>,
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

fn system_text(kind: u8) -> Option<&'static str> {
    Some(match kind {
        1 => "добавил(а) участника в группу",
        2 => "удалил(а) участника из группы",
        3 => "начал(а) звонок",
        4 => "изменил(а) название",
        5 => "изменил(а) значок",
        6 => "закрепил(а) сообщение",
        7 => "присоединился(-ась) к серверу",
        8..=11 => "забустил(а) сервер",
        _ => return None,
    })
}

/// `<@123>` → `@имя`, `<:smile:123>` → `:smile:`, `<t:1700000000:f>` → дата.
fn render_markup(content: &str, names: &HashMap<String, String>, keep_emoji: bool) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find('>') else {
            out.push_str(after);
            rest = "";
            break;
        };
        let inner = &after[1..end];
        let replaced =
            if let Some(id) = inner.strip_prefix("@!").or_else(|| inner.strip_prefix('@')) {
                if let Some(role) = id.strip_prefix('&') {
                    (role.chars().all(|c| c.is_ascii_digit())).then(|| "@роль".to_string())
                } else if id.chars().all(|c| c.is_ascii_digit()) {
                    Some(format!(
                        "@{}",
                        names.get(id).map(String::as_str).unwrap_or("пользователь")
                    ))
                } else {
                    None
                }
            } else if let Some(id) = inner.strip_prefix('#') {
                id.chars()
                    .all(|c| c.is_ascii_digit())
                    .then(|| "#канал".to_string())
            } else if (inner.starts_with(':') || inner.starts_with("a:")) && !keep_emoji {
                let body = inner.trim_start_matches("a:").trim_start_matches(':');
                body.split_once(':').map(|(name, _)| format!(":{name}:"))
            } else if let Some(ts) = inner.strip_prefix("t:") {
                let secs = ts.split(':').next().and_then(|s| s.parse::<i64>().ok());
                secs.and_then(|s| chrono::DateTime::from_timestamp(s, 0))
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%d.%m.%Y %H:%M")
                            .to_string()
                    })
            } else {
                None
            };
        match replaced {
            Some(r) => out.push_str(&r),
            None => out.push_str(&after[..=end]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Превью Tenor (`...AAAAe/name.png`) → анимированный GIF (`...AAAAC/name.gif`).
fn gif_from_preview(url: &str) -> Option<String> {
    if url.contains("media.tenor.com") {
        let (base, file) = url.split('?').next()?.rsplit_once('/')?;
        let (prefix, id) = base.rsplit_once('/')?;
        if id.len() < 6 {
            return None;
        }
        let slug = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
        return Some(format!("{prefix}/{}C/{slug}.gif", &id[..id.len() - 1]));
    }
    if url.contains("giphy.com") && url.contains("_s.gif") {
        return Some(url.replace("_s.gif", ".gif"));
    }
    None
}

fn convert_media(m: &RawMedia) -> Option<Media> {
    let url = m
        .proxy_url
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| m.url.clone());
    (!url.is_empty()).then(|| Media {
        url,
        width: m.width.unwrap_or(0),
        height: m.height.unwrap_or(0),
    })
}

fn convert_embed(e: RawEmbed) -> Embed {
    let kind = e.kind.unwrap_or_else(|| "rich".into());
    let media = e
        .image
        .as_ref()
        .and_then(convert_media)
        .or_else(|| e.thumbnail.as_ref().and_then(convert_media));
    let gif_url = if kind == "gifv" {
        e.video
            .as_ref()
            .map(|v| v.url.clone())
            .filter(|u| u.ends_with(".gif"))
            .or_else(|| e.thumbnail.as_ref().and_then(|t| gif_from_preview(&t.url)))
    } else {
        None
    };
    Embed {
        kind,
        url: e.url,
        title: e.title,
        description: e.description,
        provider: e.provider.and_then(|p| p.name),
        color: e.color,
        media,
        gif_url,
    }
}

fn convert_msg(m: RawMessage) -> Msg {
    let mentions: Vec<(u64, String)> = m
        .mentions
        .iter()
        .map(|u| (sid(&u.id), u.display()))
        .collect();
    // Разметку (упоминания, время, **жирный**…) рисует интерфейс — текст храним как есть.
    let mut content = m.content.clone();
    let mut system = false;
    if content.is_empty()
        && m.attachments.is_empty()
        && m.embeds.is_empty()
        && m.sticker_items.is_empty()
        && m.stickers.is_empty()
    {
        if let Some(t) = system_text(m.kind) {
            content.push_str(t);
            system = true;
        }
    }
    content.shrink_to_fit();

    let mut attachments: Vec<Attachment> = m
        .attachments
        .into_iter()
        .map(|a| Attachment {
            proxy_url: a
                .proxy_url
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| a.url.clone()),
            filename: a.filename,
            size: a.size,
            url: a.url,
            content_type: a.content_type.unwrap_or_default(),
            width: a.width.unwrap_or(0),
            height: a.height.unwrap_or(0),
        })
        .collect();
    // Стикеры показываем как картинки (PNG/APNG/GIF); Lottie-стикеры — подписью.
    // Стикеры: PNG/APNG/GIF показываем картинкой, Lottie (анимированные стикеры
    // Discord) — через статичное превью медиапрокси или карточкой.
    let stickers = if m.sticker_items.is_empty() {
        &m.stickers
    } else {
        &m.sticker_items
    };
    for s in stickers {
        let (ext, kind) = match s.format_type {
            1 | 2 => ("png", "sticker/png"),
            4 => ("gif", "sticker/gif"),
            _ => ("png", "sticker/lottie"),
        };
        let url = if kind == "sticker/lottie" {
            format!(
                "https://media.discordapp.net/stickers/{}.png?size=160&passthrough=false",
                s.id
            )
        } else {
            format!(
                "https://media.discordapp.net/stickers/{}.{ext}?size=160",
                s.id
            )
        };
        attachments.push(Attachment {
            filename: s.name.clone(),
            size: 0,
            proxy_url: url.clone(),
            url,
            content_type: kind.to_string(),
            width: 160,
            height: 160,
        });
    }

    let reply = m.referenced_message.map(|r| Reply {
        author: r.author.as_ref().map(RawUser::display).unwrap_or_default(),
        content: render_markup(&r.content, &HashMap::new(), false)
            .chars()
            .take(120)
            .collect::<String>()
            .replace('\n', " "),
    });

    Msg {
        id: sid(&m.id),
        channel_id: sid(&m.channel_id),
        guild_id: m.guild_id.as_deref().map(sid),
        author_id: sid(&m.author.id),
        author: m.author.display(),
        author_avatar: m.author.avatar.clone(),
        content,
        time: fmt_time(&m.timestamp),
        edited: m.edited_timestamp.is_some(),
        system,
        attachments,
        embeds: m.embeds.into_iter().map(convert_embed).collect(),
        reply,
        reactions: m
            .reactions
            .iter()
            .map(|r| Reaction {
                emoji: r.emoji.convert(),
                count: r.count,
                me: r.me,
            })
            .collect(),
        mentions,
        mention_everyone: m.mention_everyone,
        mention_roles: m.mention_roles.iter().map(|r| sid(r)).collect(),
        pinned: m.pinned,
        thread: m.thread.as_ref().map(|t| {
            (
                sid(&t.id),
                t.name.clone().unwrap_or_default(),
                t.message_count.unwrap_or(0),
            )
        }),
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
    let (prop_name, prop_icon) = match g.properties {
        Some(p) => (p.name, p.icon),
        None => (None, None),
    };
    Guild {
        id: sid(&g.id),
        name: g
            .name
            .or(prop_name)
            .unwrap_or_else(|| "Без названия".to_string()),
        icon: g.icon.or(prop_icon),
        roles: g
            .roles
            .iter()
            .map(|r| (r.id.clone(), (r.name.clone(), r.color)))
            .collect(),
        voice: g
            .voice_states
            .iter()
            .filter_map(RawVoiceState::member)
            .collect(),
        channels: g
            .channels
            .iter()
            .chain(g.threads.iter())
            .map(convert_channel)
            .collect(),
        emojis: g
            .emojis
            .iter()
            .map(RawEmoji::convert)
            .filter(|e| e.id.is_some())
            .collect(),
    }
}

/// users: id -> (имя, хэш аватарки)
fn convert_dm(c: &RawChannel, users: &HashMap<String, (String, Option<String>)>) -> Dm {
    let mut people: Vec<(u64, String, Option<String>)> = c
        .recipients
        .iter()
        .map(|u| (sid(&u.id), u.display(), u.avatar.clone()))
        .collect();
    if people.is_empty() {
        people = c
            .recipient_ids
            .iter()
            .filter_map(|id| users.get(id).map(|(n, a)| (sid(id), n.clone(), a.clone())))
            .collect();
    }
    let group = c.kind == 3;
    let name = match &c.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ if people.is_empty() => "Неизвестный".to_string(),
        _ => people
            .iter()
            .map(|(_, n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    };
    let avatar = if !group && people.len() == 1 {
        Some((people[0].0, people[0].2.clone()))
    } else {
        None
    };
    Dm {
        id: sid(&c.id),
        name,
        last_message: c.last_message_id.as_deref().map(sid).unwrap_or(0),
        avatar,
        group,
        recipients: people,
    }
}

// ======================= фоновый поток =======================

#[derive(Clone)]
pub(crate) struct Sink {
    tx: mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    pub(crate) fn send(&self, e: Event) {
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
                .max_blocking_threads(2)
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
    let mut gateway_out: Option<UnboundedSender<String>> = None;
    // Не больше 4 одновременных загрузок картинок.
    let images = Arc::new(tokio::sync::Semaphore::new(4));
    let mut proxy_now: Option<Proxy> = None;
    let mut voice = VoiceManager::default();
    let (sig_tx, mut sig_rx) = tokio::sync::mpsc::unbounded_channel::<VoiceSignal>();
    let _ = VOICE_SIGNAL.set(sig_tx);

    loop {
        let cmd = tokio::select! {
            c = rx.recv() => match c {
                Some(c) => c,
                None => break,
            },
            Some(sig) = sig_rx.recv() => {
                voice.on_signal(sig, &sink, proxy_now.clone());
                continue;
            }
        };
        match cmd {
            Cmd::Api {
                method,
                path,
                body,
                tag,
            } => {
                let Some(a) = auth.clone() else { continue };
                let (http, sink) = (http.clone(), sink.clone());
                tokio::spawn(async move {
                    let result = api_call(&http, &a, method, &path, body).await;
                    sink.send(Event::Api { tag, result });
                });
            }
            Cmd::SetPresence { status, custom } => {
                let activities = if custom.trim().is_empty() {
                    json!([])
                } else {
                    json!([{ "type": 4, "name": "Custom Status", "state": custom.trim() }])
                };
                gw_send(
                    &gateway_out,
                    json!({
                        "op": 3,
                        "d": { "status": status, "since": 0, "activities": activities, "afk": false }
                    }),
                );
                // запоминаем статус в аккаунте, чтобы он сохранился после перезапуска
                if let Some(a) = auth.clone().filter(|a| !a.bot) {
                    let http = http.clone();
                    tokio::spawn(async move {
                        let custom_status = if custom.trim().is_empty() {
                            serde_json::Value::Null
                        } else {
                            json!({ "text": custom.trim() })
                        };
                        let _ = api_call(
                            &http,
                            &a,
                            "PATCH",
                            "/users/@me/settings",
                            Some(json!({ "status": status, "custom_status": custom_status })),
                        )
                        .await;
                    });
                }
            }
            Cmd::JoinVoice {
                guild_id,
                channel_id,
            } => {
                voice.join(guild_id, channel_id, &gateway_out, &sink);
            }
            Cmd::LeaveVoice => voice.leave(&gateway_out, &sink),
            Cmd::SetMute(m) => voice.set_flags(Some(m), None, &gateway_out),
            Cmd::SetDeaf(d) => voice.set_flags(None, Some(d), &gateway_out),
            Cmd::VoiceOptions(o) => voice.set_options(o),
            Cmd::StartStream(cfg) => {
                voice.start_stream(cfg, &gateway_out, &sink, proxy_now.clone())
            }
            Cmd::StopStream => voice.stop_stream(&gateway_out, &sink),
            Cmd::WatchStream(key) => voice.watch(key, &gateway_out, &sink),
            Cmd::StopWatching => voice.stop_watching(&gateway_out, &sink),
            Cmd::RingCall(channel_id) | Cmd::DeclineCall(channel_id) if auth.is_some() => {
                let ring = matches!(cmd, Cmd::RingCall(_));
                let a = auth.clone().unwrap();
                let (http, sink) = (http.clone(), sink.clone());
                tokio::spawn(async move {
                    let me = ME.load(std::sync::atomic::Ordering::Relaxed);
                    let (path, body) = if ring {
                        ("ring", json!({ "recipients": null }))
                    } else {
                        ("stop-ringing", json!({ "recipients": [me.to_string()] }))
                    };
                    let res = http
                        .post(format!("{API}/channels/{channel_id}/call/{path}"))
                        .header("Authorization", a.header())
                        .json(&body)
                        .send()
                        .await;
                    if let Ok(r) = res {
                        if !r.status().is_success() && ring {
                            sink.send(Event::Error(format!(
                                "Не удалось позвонить: {}",
                                http_error(r.status())
                            )));
                        }
                    }
                });
            }
            Cmd::RingCall(_) | Cmd::DeclineCall(_) => {}
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
                let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
                gateway_out = Some(out_tx);
                proxy_now = proxy.clone();
                gateway = Some(tokio::spawn(gateway_loop(
                    a.clone(),
                    proxy,
                    sink.clone(),
                    out_rx,
                )));
                auth = Some(a);
            }
            Cmd::Logout => {
                if let Some(h) = gateway.take() {
                    h.abort();
                }
                voice.leave(&gateway_out, &sink);
                gateway_out = None;
                auth = None;
            }
            Cmd::SubscribeMembers {
                guild_id,
                channel_id,
            } => {
                // Список участников (как в официальном клиенте) — только для аккаунтов.
                if let (Some(a), Some(out)) = (&auth, &gateway_out) {
                    if !a.bot {
                        let mut channels = serde_json::Map::new();
                        channels.insert(channel_id.to_string(), json!([[0, 99], [100, 199]]));
                        let packet = json!({
                            "op": 14,
                            "d": {
                                "guild_id": guild_id.to_string(),
                                "typing": true,
                                "activities": false,
                                "threads": false,
                                "channels": channels
                            }
                        });
                        let _ = out.send(packet.to_string());
                    }
                }
            }
            Cmd::Edit {
                channel_id,
                message_id,
                content,
            } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        let url = format!("{API}/channels/{channel_id}/messages/{message_id}");
                        let res = http
                            .patch(url)
                            .header("Authorization", a.header())
                            .json(&json!({ "content": content }))
                            .send()
                            .await;
                        match res {
                            Ok(r) if r.status().is_success() => sink.send(Event::MessageUpdate {
                                id: message_id,
                                channel_id,
                                content,
                            }),
                            Ok(r) => sink.send(Event::Error(format!(
                                "Не изменено: {}",
                                http_error(r.status())
                            ))),
                            Err(e) => {
                                sink.send(Event::Error(format!("Не изменено, нет связи: {e}")))
                            }
                        }
                    });
                }
            }
            Cmd::Delete {
                channel_id,
                message_id,
            } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        let url = format!("{API}/channels/{channel_id}/messages/{message_id}");
                        let res = http
                            .delete(url)
                            .header("Authorization", a.header())
                            .send()
                            .await;
                        match res {
                            Ok(r) if r.status().is_success() => sink.send(Event::MessageDelete {
                                id: message_id,
                                channel_id,
                            }),
                            Ok(r) => sink.send(Event::Error(format!(
                                "Не удалено: {}",
                                http_error(r.status())
                            ))),
                            Err(e) => {
                                sink.send(Event::Error(format!("Не удалено, нет связи: {e}")))
                            }
                        }
                    });
                }
            }
            Cmd::React {
                channel_id,
                message_id,
                emoji,
                add,
            } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        let url = format!(
                            "{API}/channels/{channel_id}/messages/{message_id}/reactions/{}/@me",
                            percent_encode(&emoji.api_key())
                        );
                        let req = if add {
                            http.put(url).body("")
                        } else {
                            http.delete(url)
                        };
                        match req.header("Authorization", a.header()).send().await {
                            Ok(r) if r.status().is_success() => {}
                            Ok(r) => sink.send(Event::Error(format!(
                                "Реакция не поставлена: {}",
                                http_error(r.status())
                            ))),
                            Err(e) => {
                                sink.send(Event::Error(format!("Реакция не поставлена: {e}")))
                            }
                        }
                    });
                }
            }
            Cmd::Typing { channel_id } => {
                if let Some(a) = auth.clone() {
                    let http = http.clone();
                    tokio::spawn(async move {
                        let _ = http
                            .post(format!("{API}/channels/{channel_id}/typing"))
                            .header("Authorization", a.header())
                            .body("")
                            .send()
                            .await;
                    });
                }
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
                reply_to,
            } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        match send_message(&http, &a, channel_id, &content, reply_to).await {
                            Ok(m) => sink.send(Event::MessageCreate(m)),
                            Err(e) => sink.send(Event::Error(e)),
                        }
                    });
                }
            }
            Cmd::Upload {
                channel_id,
                path,
                content,
            } => {
                if let Some(a) = auth.clone() {
                    let (http, sink) = (http.clone(), sink.clone());
                    tokio::spawn(async move {
                        sink.send(Event::Info("Отправляю файл…".into()));
                        match upload_file(&http, &a, channel_id, &path, &content).await {
                            Ok(m) => sink.send(Event::MessageCreate(m)),
                            Err(e) => sink.send(Event::Error(e)),
                        }
                    });
                }
            }
            Cmd::FetchImage {
                key,
                url,
                max_side,
                animate,
            } => {
                let (http, sink, sem) = (http.clone(), sink.clone(), images.clone());
                tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await;
                    let result = fetch_image(&http, &url, max_side, animate).await;
                    sink.send(Event::Image { key, result });
                });
            }
            Cmd::Download { url, filename } => {
                let (http, sink) = (http.clone(), sink.clone());
                tokio::spawn(async move {
                    match download(&http, &url, &filename).await {
                        Ok(path) => {
                            sink.send(Event::Info(format!("Сохранено: {}", path.display())))
                        }
                        Err(e) => sink.send(Event::Error(format!("Не скачалось: {e}"))),
                    }
                });
            }
        }
    }
}

// ======================= REST =======================

/// Универсальный запрос к REST API: тело ответа строкой или понятная ошибка.
async fn api_call(
    http: &reqwest::Client,
    auth: &Auth,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<String, String> {
    let url = format!("{API}{path}");
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut req = http.request(m, url).header("Authorization", auth.header());
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req
        .send()
        .await
        .map_err(|e| format!("ошибка соединения: {e}"))?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if status.is_success() {
        if status.as_u16() == 202 {
            return Err("Discord ещё готовит результаты — попробуйте через пару секунд".into());
        }
        return Ok(text);
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if v.get("captcha_key").is_some() || v.get("captcha_sitekey").is_some() {
        return Err(
            "Discord требует капчу для этого действия — сделайте его один раз в официальном клиенте или браузере"
                .into(),
        );
    }
    match v.get("message").and_then(|m| m.as_str()) {
        Some(msg) => Err(format!("{} ({msg})", http_error(status))),
        None => Err(http_error(status)),
    }
}

/// Сообщения из ответа REST (массив).
pub fn parse_messages(body: &str) -> Vec<Msg> {
    #[derive(Deserialize)]
    struct Wrap(#[serde(deserialize_with = "lenient_vec")] Vec<RawMessage>);
    serde_json::from_str::<Wrap>(body)
        .map(|w| w.0.into_iter().map(convert_msg).collect())
        .unwrap_or_default()
}

/// Результаты поиска: (всего найдено, сообщения).
pub fn parse_search(body: &str) -> (u64, Vec<Msg>) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let total = v["total_results"].as_u64().unwrap_or(0);
    let mut out = Vec::new();
    if let Some(groups) = v["messages"].as_array() {
        for g in groups {
            let hit = g
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find(|m| m["hit"].as_bool() == Some(true))
                        .or(a.first())
                })
                .cloned();
            if let Some(m) = hit.and_then(|m| serde_json::from_value::<RawMessage>(m).ok()) {
                out.push(convert_msg(m));
            }
        }
    }
    (total, out)
}

/// Ветки из ответа поиска веток: (id сервера, ветка, сообщений).
pub fn parse_threads(body: &str) -> Vec<(u64, Channel, u32)> {
    #[derive(Deserialize)]
    struct Wrap {
        #[serde(default, deserialize_with = "lenient_vec")]
        threads: Vec<RawChannel>,
    }
    serde_json::from_str::<Wrap>(body)
        .map(|w| {
            w.threads
                .iter()
                .map(|t| {
                    (
                        t.guild_id.as_deref().map(sid).unwrap_or(0),
                        convert_channel(t),
                        t.message_count.unwrap_or(0),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Личный диалог из ответа REST (POST /users/@me/channels).
pub fn parse_dm(body: &str) -> Option<Dm> {
    let c: RawChannel = serde_json::from_str(body).ok()?;
    Some(convert_dm(&c, &HashMap::new()))
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn http_error(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 => "Токен недействителен".into(),
        403 => "Нет доступа к этому каналу".into(),
        404 => "Канал не найден".into(),
        413 => "Файл слишком большой для Discord".into(),
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
    reply_to: Option<u64>,
) -> Result<Msg, String> {
    let mut body = json!({ "content": content });
    if let Some(id) = reply_to {
        body["message_reference"] =
            json!({ "message_id": id.to_string(), "fail_if_not_exists": false });
    }
    let resp = http
        .post(format!("{API}/channels/{channel_id}/messages"))
        .header("Authorization", auth.header())
        .json(&body)
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

async fn upload_file(
    http: &reqwest::Client,
    auth: &Auth,
    channel_id: u64,
    path: &std::path::Path,
    content: &str,
) -> Result<Msg, String> {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > MAX_UPLOAD {
        return Err(format!(
            "Файл «{filename}» больше 10 МБ — Discord без подписки такие не принимает"
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Не удалось прочитать файл: {e}"))?;
    let payload = json!({
        "content": content,
        "attachments": [{ "id": 0, "filename": filename }]
    });
    let form = reqwest::multipart::Form::new()
        .text("payload_json", payload.to_string())
        .part(
            "files[0]",
            reqwest::multipart::Part::bytes(bytes).file_name(filename.clone()),
        );
    let resp = http
        .post(format!("{API}/channels/{channel_id}/messages"))
        .header("Authorization", auth.header())
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Файл не отправлен, нет связи: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Файл не отправлен: {}", http_error(resp.status())));
    }
    let raw: RawMessage = resp
        .json()
        .await
        .map_err(|e| format!("Неожиданный ответ Discord: {e}"))?;
    Ok(convert_msg(raw))
}

fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join("Downloads");
    if dir.is_dir() {
        dir
    } else {
        home
    }
}

/// Имя файла без символов, запрещённых в Windows.
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim().is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

async fn download(http: &reqwest::Client, url: &str, filename: &str) -> Result<PathBuf, String> {
    let resp = http.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let dir = downloads_dir();
    let name = safe_filename(filename);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    let mut path = dir.join(&name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| e.to_string())?;
    Ok(path)
}

async fn fetch_image(
    http: &reqwest::Client,
    url: &str,
    max_side: u32,
    animate: bool,
) -> Result<Vec<Frame>, String> {
    let resp = http.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    if resp.content_length().unwrap_or(0) > MAX_IMAGE_DOWNLOAD {
        return Err("слишком большая картинка".into());
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || decode_image(&bytes, max_side, animate))
        .await
        .map_err(|e| e.to_string())?
}

fn to_color_image(img: image::DynamicImage, max_side: u32) -> egui::ColorImage {
    let img = if img.width() > max_side || img.height() > max_side {
        img.resize(max_side, max_side, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    )
}

/// Декодирует и сразу уменьшает: в памяти хранится только то, что видно на экране.
fn decode_image(bytes: &[u8], max_side: u32, animate: bool) -> Result<Vec<Frame>, String> {
    use image::AnimationDecoder;
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    if animate && format == image::ImageFormat::Gif {
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
            .map_err(|e| e.to_string())?;
        let mut frames = Vec::new();
        for frame in decoder.into_frames().take(MAX_GIF_FRAMES) {
            let Ok(frame) = frame else { break };
            let (num, den) = frame.delay().numer_denom_ms();
            let delay_ms = if den == 0 { 100 } else { (num / den).max(20) };
            let img = image::DynamicImage::ImageRgba8(frame.into_buffer());
            frames.push(Frame {
                image: to_color_image(img, max_side),
                delay_ms,
            });
        }
        if !frames.is_empty() {
            return Ok(frames);
        }
    }
    let img = image::load_from_memory_with_format(bytes, format).map_err(|e| e.to_string())?;
    Ok(vec![Frame {
        image: to_color_image(img, max_side),
        delay_ms: 0,
    }])
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

async fn gateway_loop(
    auth: Auth,
    proxy: Option<Proxy>,
    sink: Sink,
    mut out_rx: UnboundedReceiver<String>,
) {
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
        let reason = match session(&auth, proxy.as_ref(), &sink, &mut got_ready, &mut out_rx).await
        {
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
    out_rx: &mut UnboundedReceiver<String>,
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
            Some(packet) = out_rx.recv(), if *got_ready => {
                write.send(WsMsg::Text(packet)).await?;
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
            ME.store(sid(&r.user.id), std::sync::atomic::Ordering::Relaxed);
            let users: HashMap<String, (String, Option<String>)> = r
                .users
                .iter()
                .map(|u| (u.id.clone(), (u.display(), u.avatar.clone())))
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
            let presences = r
                .presences
                .iter()
                .filter_map(RawPresenceUpdate::convert)
                .collect();
            let friends = r
                .relationships
                .into_iter()
                .map(|x| x.convert(&users))
                .collect();
            let (status, custom_status) = match r.user_settings {
                Some(us) => (us.status, us.custom_status.and_then(|c| c.text)),
                None => (None, None),
            };
            sink.send(Event::Ready {
                user_id: sid(&r.user.id),
                username: r.user.display(),
                avatar: r.user.avatar.clone(),
                guilds,
                dms,
                presences,
                friends,
                status,
                custom_status,
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
        "THREAD_CREATE" | "THREAD_UPDATE" => {
            let c: RawChannel = serde_json::from_str(d)?;
            if let Some(g) = c.guild_id.as_deref() {
                sink.send(Event::GuildChannel {
                    guild_id: sid(g),
                    channel: convert_channel(&c),
                });
            }
        }
        "THREAD_LIST_SYNC" => {
            let l: RawThreadList = serde_json::from_str(d)?;
            if let Some(g) = l.guild_id.as_deref().map(sid) {
                for c in &l.threads {
                    sink.send(Event::GuildChannel {
                        guild_id: g,
                        channel: convert_channel(c),
                    });
                }
            }
        }
        "PRESENCE_UPDATE" => {
            let p: RawPresenceUpdate = serde_json::from_str(d)?;
            if let Some((user_id, status)) = p.convert() {
                sink.send(Event::Presence { user_id, status });
            }
        }
        "RELATIONSHIP_ADD" => {
            let r: RawRelationship = serde_json::from_str(d)?;
            sink.send(Event::Relationship {
                friend: Some(r.convert(&HashMap::new())),
                removed: None,
            });
        }
        "RELATIONSHIP_REMOVE" => {
            let r: RawRelationship = serde_json::from_str(d)?;
            sink.send(Event::Relationship {
                friend: None,
                removed: Some(sid(&r.id)),
            });
        }
        "CHANNEL_DELETE" | "THREAD_DELETE" => {
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
        "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_REMOVE" => {
            let r: RawReactionEvent = serde_json::from_str(d)?;
            sink.send(Event::Reaction {
                channel_id: sid(&r.channel_id),
                message_id: sid(&r.message_id),
                user_id: sid(&r.user_id),
                emoji: r.emoji.convert(),
                add: t == "MESSAGE_REACTION_ADD",
            });
        }
        "MESSAGE_REACTION_REMOVE_ALL" | "MESSAGE_REACTION_REMOVE_EMOJI" => {
            let r: RawReactionClear = serde_json::from_str(d)?;
            sink.send(Event::ReactionsCleared {
                channel_id: sid(&r.channel_id),
                message_id: sid(&r.message_id),
            });
        }
        "TYPING_START" => {
            let r: RawTyping = serde_json::from_str(d)?;
            let name = r.member.and_then(|m| {
                m.nick
                    .filter(|n| !n.is_empty())
                    .or_else(|| m.user.map(|u| u.display()))
            });
            sink.send(Event::Typing {
                channel_id: sid(&r.channel_id),
                user_id: sid(&r.user_id),
                name,
            });
        }
        "GUILD_MEMBER_LIST_UPDATE" => {
            let r: RawMemberList = serde_json::from_str(d)?;
            let mut ops = Vec::new();
            for op in r.ops {
                match op.op.as_str() {
                    "SYNC" => ops.push(ListOp::Sync {
                        start: op
                            .range
                            .as_ref()
                            .and_then(|r| r.first().copied())
                            .unwrap_or(0),
                        items: op
                            .items
                            .into_iter()
                            .filter_map(RawListItem::convert)
                            .collect(),
                    }),
                    "INSERT" | "UPDATE" => {
                        let insert = op.op == "INSERT";
                        if let (Some(index), Some(item)) =
                            (op.index, op.item.and_then(RawListItem::convert))
                        {
                            ops.push(if insert {
                                ListOp::Insert { index, item }
                            } else {
                                ListOp::Update { index, item }
                            });
                        }
                    }
                    "DELETE" => {
                        if let Some(index) = op.index {
                            ops.push(ListOp::Delete { index });
                        }
                    }
                    _ => {}
                }
            }
            sink.send(Event::MemberList {
                guild_id: sid(&r.guild_id),
                groups: r.groups.into_iter().map(|g| (g.id, g.count)).collect(),
                ops,
            });
        }
        "VOICE_STATE_UPDATE" => {
            let v: RawVoiceState = serde_json::from_str(d)?;
            let user_id = sid(&v.user_id);
            if user_id == ME.load(std::sync::atomic::Ordering::Relaxed) {
                voice_signal(VoiceSignal::State {
                    session_id: v.session_id.clone(),
                    channel_id: v.channel_id.as_deref().map(sid),
                });
            }
            let guild_id = v.guild_id.as_deref().map(sid);
            let member = v.member().unwrap_or(VoiceMember {
                user_id,
                channel_id: 0,
                muted: false,
                deafened: false,
                streaming: false,
                name: None,
                avatar: None,
            });
            sink.send(Event::VoiceState {
                guild_id,
                left: v.channel_id.is_none(),
                member,
            });
        }
        "VOICE_SERVER_UPDATE" => {
            let v: RawVoiceServer = serde_json::from_str(d)?;
            if let Some(endpoint) = v.endpoint.filter(|e| !e.is_empty()) {
                voice_signal(VoiceSignal::Server {
                    token: v.token,
                    endpoint,
                });
            }
        }
        "CALL_CREATE" | "CALL_UPDATE" => {
            let c: RawCall = serde_json::from_str(d)?;
            let me = ME.load(std::sync::atomic::Ordering::Relaxed);
            // кто уже сидит в звонке
            for v in &c.voice_states {
                if let Some(member) = v.member() {
                    sink.send(Event::VoiceState {
                        guild_id: None,
                        member,
                        left: false,
                    });
                }
            }
            sink.send(Event::CallRing {
                channel_id: sid(&c.channel_id),
                ringing: c.ringing.iter().any(|id| sid(id) == me),
            });
        }
        "STREAM_CREATE" => {
            let v: RawStream = serde_json::from_str(d)?;
            if let Some(rtc) = v.rtc_server_id.as_deref().map(sid) {
                voice_signal(VoiceSignal::StreamCreate {
                    key: v.stream_key,
                    rtc_server_id: rtc,
                });
            }
        }
        "STREAM_SERVER_UPDATE" => {
            let v: RawStream = serde_json::from_str(d)?;
            if let Some(endpoint) = v.endpoint.filter(|e| !e.is_empty()) {
                voice_signal(VoiceSignal::StreamServer {
                    key: v.stream_key,
                    token: v.token.unwrap_or_default(),
                    endpoint,
                });
            }
        }
        "STREAM_DELETE" => {
            let v: RawStream = serde_json::from_str(d)?;
            voice_signal(VoiceSignal::StreamDelete {
                key: v.stream_key,
                reason: v.reason,
            });
        }
        "CALL_DELETE" => {
            let c: RawCall = serde_json::from_str(d)?;
            sink.send(Event::CallEnded {
                channel_id: sid(&c.channel_id),
            });
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

// ======================= голосовой менеджер =======================

#[derive(Default)]
struct VoiceManager {
    target: Option<(Option<u64>, u64)>,
    session_id: Option<String>,
    server: Option<(String, String)>,
    conn: Option<(
        tokio::task::JoinHandle<()>,
        UnboundedSender<crate::voice::Ctl>,
    )>,
    mute: bool,
    deaf: bool,
    opts: Option<crate::voice::VoiceOpts>,
    /// Наша трансляция.
    outgoing: Option<StreamSlot>,
    /// Трансляция, которую мы смотрим.
    watching: Option<StreamSlot>,
}

struct StreamSlot {
    key: String,
    send: Option<crate::stream::SendCfg>,
    rtc_server_id: Option<u64>,
    server: Option<(String, String)>,
    conn: Option<tokio::task::JoinHandle<()>>,
}

impl StreamSlot {
    fn new(key: String, send: Option<crate::stream::SendCfg>) -> Self {
        Self {
            key,
            send,
            rtc_server_id: None,
            server: None,
            conn: None,
        }
    }

    fn stop(&mut self) {
        if let Some(h) = self.conn.take() {
            h.abort();
        }
    }
}

fn gw_send(out: &Option<UnboundedSender<String>>, v: serde_json::Value) {
    if let Some(out) = out {
        let _ = out.send(v.to_string());
    }
}

impl VoiceManager {
    fn set_options(&mut self, o: crate::voice::VoiceOpts) {
        if let Some((_, ctl)) = &self.conn {
            let _ = ctl.send(crate::voice::Ctl::Options(o.clone()));
        }
        self.opts = Some(o);
    }

    fn send_state(
        &self,
        out: &Option<UnboundedSender<String>>,
        guild: Option<u64>,
        channel: Option<u64>,
    ) {
        if let Some(out) = out {
            let packet = json!({
                "op": 4,
                "d": {
                    "guild_id": guild.map(|g| g.to_string()),
                    "channel_id": channel.map(|c| c.to_string()),
                    "self_mute": self.mute,
                    "self_deaf": self.deaf,
                    "self_video": false
                }
            });
            let _ = out.send(packet.to_string());
        }
    }

    fn stop_conn(&mut self) {
        if let Some((h, _)) = self.conn.take() {
            h.abort();
        }
    }

    fn join(
        &mut self,
        guild: Option<u64>,
        channel: u64,
        out: &Option<UnboundedSender<String>>,
        sink: &Sink,
    ) {
        self.stop_conn();
        self.target = Some((guild, channel));
        self.session_id = None;
        self.server = None;
        self.send_state(out, guild, Some(channel));
        sink.send(Event::Voice(VoiceUi::Connecting {
            guild_id: guild,
            channel_id: channel,
        }));
    }

    fn leave(&mut self, out: &Option<UnboundedSender<String>>, sink: &Sink) {
        self.stop_watching(out, sink);
        self.stop_stream(out, sink);
        self.stop_conn();
        if let Some((guild, _)) = self.target.take() {
            self.send_state(out, guild, None);
            sink.send(Event::Voice(VoiceUi::Disconnected(None)));
        }
        self.session_id = None;
        self.server = None;
    }

    fn set_flags(
        &mut self,
        mute: Option<bool>,
        deaf: Option<bool>,
        out: &Option<UnboundedSender<String>>,
    ) {
        if let Some(m) = mute {
            self.mute = m;
        }
        if let Some(d) = deaf {
            self.deaf = d;
        }
        if let Some((_, ctl)) = &self.conn {
            let _ = ctl.send(crate::voice::Ctl::Mute(self.mute));
            let _ = ctl.send(crate::voice::Ctl::Deaf(self.deaf));
        }
        if let Some((guild, channel)) = self.target {
            self.send_state(out, guild, Some(channel));
        }
    }

    fn stream_key(&self) -> Option<String> {
        let (guild, channel) = self.target?;
        let me = ME.load(std::sync::atomic::Ordering::Relaxed);
        Some(match guild {
            Some(g) => format!("guild:{g}:{channel}:{me}"),
            None => format!("call:{channel}:{me}"),
        })
    }

    fn start_stream(
        &mut self,
        cfg: crate::stream::SendCfg,
        out: &Option<UnboundedSender<String>>,
        sink: &Sink,
        proxy: Option<Proxy>,
    ) {
        let (Some((guild, channel)), Some(key)) = (self.target, self.stream_key()) else {
            sink.send(Event::Error(
                "Сначала зайдите в голосовой канал или звонок".into(),
            ));
            return;
        };
        if let Some(slot) = &mut self.outgoing {
            if slot.key == key {
                // трансляция уже идёт — перезапускаем только захват с новыми настройками
                slot.send = Some(cfg);
                slot.stop();
                self.spawn_streams(sink, proxy);
                return;
            }
        }
        self.stop_stream(out, sink);
        gw_send(
            out,
            json!({
                "op": 18,
                "d": {
                    "type": if guild.is_some() { "guild" } else { "call" },
                    "guild_id": guild.map(|g| g.to_string()),
                    "channel_id": channel.to_string(),
                    "preferred_region": null
                }
            }),
        );
        gw_send(
            out,
            json!({ "op": 22, "d": { "stream_key": key, "paused": false } }),
        );
        self.outgoing = Some(StreamSlot::new(key, Some(cfg)));
    }

    fn stop_stream(&mut self, out: &Option<UnboundedSender<String>>, sink: &Sink) {
        if let Some(mut slot) = self.outgoing.take() {
            slot.stop();
            gw_send(out, json!({ "op": 19, "d": { "stream_key": slot.key } }));
            sink.send(Event::StreamEnded {
                key: slot.key,
                error: None,
            });
        }
    }

    fn watch(&mut self, key: String, out: &Option<UnboundedSender<String>>, sink: &Sink) {
        if self.target.is_none() {
            sink.send(Event::Error(
                "Чтобы смотреть трансляцию, зайдите в этот голосовой канал".into(),
            ));
            return;
        }
        if self.watching.as_ref().map_or(false, |w| w.key == key) {
            return;
        }
        self.stop_watching(out, sink);
        gw_send(out, json!({ "op": 20, "d": { "stream_key": key } }));
        self.watching = Some(StreamSlot::new(key, None));
    }

    fn stop_watching(&mut self, out: &Option<UnboundedSender<String>>, sink: &Sink) {
        if let Some(mut slot) = self.watching.take() {
            slot.stop();
            gw_send(out, json!({ "op": 19, "d": { "stream_key": slot.key } }));
            sink.send(Event::StreamEnded {
                key: slot.key,
                error: None,
            });
        }
    }

    fn spawn_streams(&mut self, sink: &Sink, proxy: Option<Proxy>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let me = ME.load(std::sync::atomic::Ordering::Relaxed);
        for slot in [self.outgoing.as_mut(), self.watching.as_mut()]
            .into_iter()
            .flatten()
        {
            if slot.conn.is_some() {
                continue;
            }
            let (Some(rtc), Some((token, endpoint))) = (slot.rtc_server_id, slot.server.clone())
            else {
                continue;
            };
            let params = crate::stream::StreamParams {
                key: slot.key.clone(),
                rtc_server_id: rtc,
                user_id: me,
                session_id: session_id.clone(),
                token,
                endpoint,
            };
            let role = match slot.send {
                Some(cfg) => crate::stream::Role::Send(cfg),
                None => crate::stream::Role::Watch,
            };
            slot.conn = Some(tokio::spawn(crate::stream::run(
                params,
                role,
                proxy.clone(),
                sink.clone(),
            )));
        }
    }

    fn on_stream_signal(&mut self, sig: VoiceSignal, sink: &Sink, proxy: Option<Proxy>) {
        let key = match &sig {
            VoiceSignal::StreamCreate { key, .. }
            | VoiceSignal::StreamServer { key, .. }
            | VoiceSignal::StreamDelete { key, .. } => key.clone(),
            _ => return,
        };
        let is_out = self.outgoing.as_ref().map_or(false, |s| s.key == key);
        let is_watch = self.watching.as_ref().map_or(false, |s| s.key == key);
        if !is_out && !is_watch {
            return;
        }
        let slot = if is_out {
            self.outgoing.as_mut()
        } else {
            self.watching.as_mut()
        };
        let Some(slot) = slot else { return };
        match sig {
            VoiceSignal::StreamCreate { rtc_server_id, .. } => {
                if slot.rtc_server_id != Some(rtc_server_id) {
                    slot.stop();
                }
                slot.rtc_server_id = Some(rtc_server_id);
            }
            VoiceSignal::StreamServer {
                token, endpoint, ..
            } => {
                slot.stop();
                slot.server = Some((token, endpoint));
            }
            VoiceSignal::StreamDelete { reason, .. } => {
                slot.stop();
                if is_out {
                    self.outgoing = None;
                } else {
                    self.watching = None;
                }
                let error = match reason.as_deref() {
                    None | Some("user_requested") | Some("stream_ended") => None,
                    Some("unauthorized") => {
                        Some("нет прав на трансляцию в этом канале".to_string())
                    }
                    Some("stream_full") => Some("трансляция заполнена".to_string()),
                    Some(other) => Some(other.to_string()),
                };
                sink.send(Event::StreamEnded { key, error });
                return;
            }
            _ => {}
        }
        self.spawn_streams(sink, proxy);
    }

    fn on_signal(&mut self, sig: VoiceSignal, sink: &Sink, proxy: Option<Proxy>) {
        if matches!(
            sig,
            VoiceSignal::StreamCreate { .. }
                | VoiceSignal::StreamServer { .. }
                | VoiceSignal::StreamDelete { .. }
        ) {
            self.on_stream_signal(sig, sink, proxy);
            return;
        }
        let Some((guild, channel)) = self.target else {
            return;
        };
        match sig {
            VoiceSignal::State {
                session_id,
                channel_id,
            } => {
                if channel_id.is_none() {
                    // нас отключили от канала (или мы вышли с другого устройства)
                    for slot in [self.outgoing.take(), self.watching.take()]
                        .into_iter()
                        .flatten()
                    {
                        let mut slot = slot;
                        slot.stop();
                        sink.send(Event::StreamEnded {
                            key: slot.key,
                            error: None,
                        });
                    }
                    self.stop_conn();
                    self.target = None;
                    sink.send(Event::Voice(VoiceUi::Disconnected(None)));
                    return;
                }
                self.session_id = Some(session_id);
            }
            VoiceSignal::Server { token, endpoint } => {
                // смена голосового сервера — переподключаемся
                self.stop_conn();
                self.server = Some((token, endpoint));
            }
            _ => {}
        }
        if self.conn.is_none() {
            if let (Some(session_id), Some((token, endpoint))) = (&self.session_id, &self.server) {
                let params = crate::voice::VoiceParams {
                    guild_id: guild,
                    channel_id: channel,
                    user_id: ME.load(std::sync::atomic::Ordering::Relaxed),
                    session_id: session_id.clone(),
                    token: token.clone(),
                    endpoint: endpoint.clone(),
                };
                let (ctl_tx, ctl_rx) = tokio::sync::mpsc::unbounded_channel();
                let handle = tokio::spawn(crate::voice::run(
                    params,
                    proxy.clone(),
                    sink.clone(),
                    ctl_rx,
                    self.mute,
                    self.deaf,
                    self.opts.clone().unwrap_or(crate::voice::VoiceOpts {
                        input: None,
                        output: None,
                        threshold: 0.006,
                        always: false,
                        echo_guard: true,
                    }),
                ));
                self.conn = Some((handle, ctl_tx));
            }
        }
        self.spawn_streams(sink, proxy);
    }
}
