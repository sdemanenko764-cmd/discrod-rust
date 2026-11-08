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
pub struct Guild {
    pub id: u64,
    pub name: String,
    pub icon: Option<String>,
    /// id роли -> (название, цвет)
    pub roles: HashMap<String, (String, u32)>,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug)]
pub struct Dm {
    pub id: u64,
    pub name: String,
    pub last_message: u64,
    /// (id собеседника, хэш аватарки) — только для личных диалогов 1 на 1.
    pub avatar: Option<(u64, Option<String>)>,
    pub group: bool,
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
struct RawGuild {
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
    mentions: Vec<RawUser>,
    #[serde(default)]
    referenced_message: Option<Box<RawRef>>,
    #[serde(default, deserialize_with = "lenient_vec")]
    reactions: Vec<RawReaction>,
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
    let names: HashMap<String, String> = m
        .mentions
        .iter()
        .map(|u| (u.id.clone(), u.display()))
        .collect();
    let mut content = render_markup(&m.content, &names, true);
    let mut system = false;
    if content.is_empty()
        && m.attachments.is_empty()
        && m.embeds.is_empty()
        && m.sticker_items.is_empty()
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
    for s in &m.sticker_items {
        let ext = match s.format_type {
            1 | 2 => "png",
            4 => "gif",
            _ => {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&format!("[стикер: {}]", s.name));
                continue;
            }
        };
        let url = format!(
            "https://media.discordapp.net/stickers/{}.{ext}?size=160",
            s.id
        );
        attachments.push(Attachment {
            filename: format!("{}.{ext}", s.name),
            size: 0,
            proxy_url: url.clone(),
            url,
            content_type: format!("image/{ext}"),
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
        channels: g.channels.iter().map(convert_channel).collect(),
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
                let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
                gateway_out = Some(out_tx);
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
                                content: render_markup(&content, &HashMap::new(), true),
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

fn percent_encode(s: &str) -> String {
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
            sink.send(Event::Ready {
                user_id: sid(&r.user.id),
                username: r.user.display(),
                avatar: r.user.avatar.clone(),
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
                    content: render_markup(&content, &HashMap::new(), true),
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
