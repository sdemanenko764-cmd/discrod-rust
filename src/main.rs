//! Rustcord — лёгкий нативный клиент Discord на Rust.
//!
//! Почему он ест мало памяти:
//! * нет встроенного браузера (официальный клиент — Electron: Chromium + Node.js);
//!   интерфейс рисуется egui напрямую через OpenGL;
//! * из ответов Discord берутся только нужные поля;
//! * сообщения хранятся лишь для нескольких последних каналов, с лимитом на канал;
//! * картинки уменьшаются до размера на экране, грузятся только когда видны
//!   и выгружаются, когда давно не показывались;
//! * системные шрифты подключаются через mmap, а не копируются в память;
//! * в простое окно не перерисовывается.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod discord;
mod emoji;
mod extra;
mod fonts;
mod markdown;
mod media;
mod net;
mod settings;
mod store;
mod stream;
mod theme;
mod voice;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, load::SizedTexture, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Layout,
    Margin, Rect, RichText, Rounding, Sense, Stroke, TextureId, Vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use discord::{
    Attachment, Auth, Channel, Cmd, Dm, Embed, Emoji, Event, Guild, ListItem, ListOp, Msg,
    Reaction, Status, VoiceMember, VoiceUi, DISCORD_EPOCH,
};
use eframe::egui::epaint::Shadow;
use media::MediaCache;
use settings::{Settings, Theme};
use theme::{Palette, ACCENTS};

const MAX_MESSAGE_LEN: usize = 2000;

const NAME_COLORS: [Color32; 10] = [
    Color32::from_rgb(242, 133, 78),
    Color32::from_rgb(88, 199, 132),
    Color32::from_rgb(110, 160, 255),
    Color32::from_rgb(230, 110, 180),
    Color32::from_rgb(236, 190, 70),
    Color32::from_rgb(70, 200, 210),
    Color32::from_rgb(170, 130, 245),
    Color32::from_rgb(250, 110, 110),
    Color32::from_rgb(140, 200, 80),
    Color32::from_rgb(255, 160, 120),
];

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustcord")
            .with_inner_size([1220.0, 780.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rustcord",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

// ======================= состояние =======================

#[derive(PartialEq)]
enum Screen {
    Login,
    Main,
}

struct ChannelCache {
    msgs: VecDeque<Msg>,
    loading: bool,
    has_more: bool,
    error: Option<String>,
}

#[derive(Clone, PartialEq)]
enum VoiceStatus {
    Connecting,
    Connected,
    Failed(String),
}

#[derive(Clone)]
struct VoiceConn {
    guild_id: Option<u64>,
    channel_id: u64,
    status: VoiceStatus,
    ping: u32,
    e2ee: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum PickerTarget {
    Composer,
    React { channel_id: u64, message_id: u64 },
}

#[derive(Default)]
struct MemberList {
    items: Vec<Option<ListItem>>,
    groups: HashMap<String, u64>,
}

enum Action {
    ToggleMembers,
    React {
        channel_id: u64,
        message_id: u64,
        emoji: Emoji,
        add: bool,
    },
    Reply(u64, String),
    StartEdit(u64, u64, String),
    EditLast(u64),
    CancelCompose,
    AskDelete(u64, u64, String),
    CopyText(String),
    Report(String),
    OpenPicker(PickerTarget, egui::Pos2),
    Typing(u64),
    ScrollBottom,
    JoinVoice(Option<u64>, u64),
    LeaveVoice,
    ToggleMute,
    ToggleStage,
    ToggleDeaf,
    CallDm(u64),
    AcceptCall(u64),
    DeclineCall(u64),
    OpenStreamPicker,
    StartStream,
    StopStream,
    WatchStream(u64),
    StopWatching,
    /// Развернуть трансляцию на всё окно / свернуть обратно.
    ToggleStreamFullscreen,
    SelectGuild(Option<u64>),
    OpenChannel(u64),
    ToggleCategory(u64),
    LoadOlder(u64),
    Send(u64, String),
    /// Повторить неотправленное сообщение (канал, nonce).
    RetrySend(u64, String),
    /// Убрать неотправленное сообщение из ленты.
    DismissFailed(u64, String),
    Attach(u64),
    Download(String, String),
    OpenUrl(String),
    OpenSettings,
    Logout,
    // --- профили, статусы, друзья, серверы ---
    OpenProfile(u64),
    CloseProfile,
    MessageUser(u64),
    OpenStatusMenu,
    SetStatus(String),
    OpenFriends,
    FriendAdd(String),
    FriendAccept(u64),
    FriendRequest(u64),
    FriendRemove(u64),
    FriendBlock(u64),
    CopyId(u64),
    GuildMenu(u64, egui::Pos2),
    AskLeave(u64),
    LeaveGuild(u64),
    CopyInvite(u64),
    OpenServerDialog,
    JoinInvite(String),
    CreateGuild(String),
    // --- сообщения и каналы ---
    MessageMenu(u64, Box<Msg>, egui::Pos2),
    /// Меню пользователя по правому клику.
    UserMenu(u64, egui::Pos2),
    /// Меню канала: (сервер, канал, позиция).
    ChannelMenu(Option<u64>, u64, egui::Pos2),
    SetUserVolume(u64, f32),
    /// Заглушить / включить уведомления сервера или канала.
    ToggleMuted(u64),
    MarkRead(u64),
    MarkUnread(u64, Option<u64>),
    CreateThread(u64, u64, String),
    /// Новая ветка без исходного сообщения: (канал, название).
    CreateStandaloneThread(u64, String),
    AskRename(u64),
    RenameChannel(u64, String),
    ChannelInvite(u64),
    /// Создать приглашение на сервер и отправить пользователю в личку.
    InviteUser(u64, u64),
    InsertMention(String),
    /// Открыть профиль сразу на вкладке (1 — общие серверы, 2 — общие друзья).
    OpenProfileTab(u64, u8),
    Pin(u64, u64, bool),
    OpenPins(u64),
    OpenSearch,
    RunSearch(String),
    OpenThreads(u64),
    OpenThread(Option<u64>, Channel),
    Jump(Option<u64>, u64),
    OpenSwitcher,
}

struct App {
    cmd: UnboundedSender<Cmd>,
    events: Receiver<Event>,
    screen: Screen,
    settings: Settings,
    pal: Palette,
    settings_open: bool,
    pending_zoom: f32,

    // вход
    token_input: String,
    proxy_input: String,
    bot_input: bool,
    remember: bool,
    login_error: Option<String>,
    logging_in: bool,
    pending: Option<Auth>,

    // сессия
    status: Status,
    me_id: u64,
    me_name: String,
    me_avatar: Option<String>,
    guilds: Vec<Guild>,
    dms: Vec<Dm>,
    sel_guild: Option<u64>,
    sel_channel: Option<u64>,
    last_channel: HashMap<Option<u64>, u64>,
    collapsed: HashSet<u64>,
    cache: HashMap<u64, ChannelCache>,
    lru: VecDeque<u64>,
    /// непрочитанный канал -> id сервера (0 для личных сообщений)
    unread: HashMap<u64, u64>,
    draft: String,
    media: MediaCache,
    reply_to: Option<(u64, String)>,
    editing: Option<(u64, u64)>,
    pending_delete: Option<(u64, u64, String)>,
    picker: Option<(PickerTarget, egui::Pos2, f64)>,
    members_open: bool,
    member_lists: HashMap<u64, MemberList>,
    member_sub: Option<(u64, u64)>,
    /// канал -> кто печатает (id, имя, когда)
    typing: HashMap<u64, Vec<(u64, String, Instant)>>,
    last_typing: Instant,
    scroll_until: Option<Instant>,
    voice: Option<VoiceConn>,
    self_mute: bool,
    self_deaf: bool,
    /// голосовой канал -> кто в нём
    voice_members: HashMap<u64, Vec<VoiceMember>>,
    speaking: HashSet<u64>,
    incoming_call: Option<u64>,
    stage_open: bool,
    mic_level: f32,
    devices: Option<(Vec<String>, Vec<String>)>,
    mic_test: Option<audio::Audio>,
    /// мы транслируем экран (и пошли ли уже кадры)
    streaming: bool,
    stream_live: bool,
    /// чью трансляцию смотрим: (ключ, id автора)
    watching: Option<(String, u64)>,
    stream_tex: Option<egui::TextureHandle>,
    stream_fullscreen: bool,
    stream_picker: bool,
    /// Окна для трансляции: (id, заголовок, приложение).
    windows: Vec<(u32, String, String)>,
    /// Вкладка выбора: false — экраны, true — окна.
    stream_windows_tab: bool,
    /// Выбранное окно (если транслируем окно, а не экран).
    stream_window: Option<u32>,
    monitors: Vec<(String, u32, u32)>,

    // профили, статусы, друзья
    /// имена пользователей, которых мы уже видели (для упоминаний)
    users: HashMap<u64, String>,
    presence: HashMap<u64, String>,
    friends: Vec<discord::Friend>,
    /// канал -> сколько раз нас упомянули (или личных сообщений) с последнего просмотра
    mentions: HashMap<u64, u32>,
    my_status: String,
    status_menu: bool,
    profile: Option<extra::ProfileView>,
    friends_open: bool,
    friends_tab: u8,
    friend_input: String,
    // окна
    msg_menu: Option<(u64, Msg, egui::Pos2)>,
    user_menu: Option<(u64, egui::Pos2)>,
    /// Подменю «Пригласить на сервер» открыто.
    user_menu_invite: bool,
    channel_menu: Option<(Option<u64>, u64, egui::Pos2)>,
    rename_channel: Option<(u64, String)>,
    /// Поле «Название новой ветки» в окне веток.
    thread_new_name: String,
    guild_menu: Option<(u64, egui::Pos2)>,
    confirm_leave: Option<u64>,
    server_dialog: Option<(u8, String)>,
    pins: Option<(u64, Option<Result<Vec<Msg>, String>>)>,
    search: Option<extra::SearchView>,
    threads: Option<(u64, Option<Result<Vec<(u64, Channel, u32)>, String>>)>,
    switcher: Option<(String, usize)>,
    complete_sel: usize,
    last_title: String,
    window_focused: bool,
    attention: bool,
    menu_frame: u64,
    frame_no: u64,

    // служебное
    toast: Option<(String, Instant, bool)>,
    mem_mb: f64,
    mem_checked: Instant,
    /// Есть сохранённый вход (можно войти снова одной кнопкой).
    has_saved: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = settings::load();
        fonts::install(&cc.egui_ctx, settings.font_inter);
        let pal = theme::palette(&settings);
        theme::apply(&cc.egui_ctx, &pal, settings.theme);
        cc.egui_ctx.set_zoom_factor(settings.zoom);

        let (cmd, events) = discord::spawn(cc.egui_ctx.clone());
        let mut app = Self {
            cmd,
            events,
            screen: Screen::Login,
            pending_zoom: settings.zoom,
            settings,
            pal,
            settings_open: false,
            token_input: String::new(),
            proxy_input: store::load_proxy(),
            bot_input: false,
            remember: true,
            login_error: None,
            logging_in: false,
            pending: None,
            status: Status::Connecting,
            me_id: 0,
            me_name: String::new(),
            me_avatar: None,
            guilds: Vec::new(),
            dms: Vec::new(),
            sel_guild: None,
            sel_channel: None,
            last_channel: HashMap::new(),
            collapsed: HashSet::new(),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            unread: HashMap::new(),
            draft: String::new(),
            media: MediaCache::default(),
            reply_to: None,
            editing: None,
            pending_delete: None,
            picker: None,
            members_open: true,
            member_lists: HashMap::new(),
            member_sub: None,
            typing: HashMap::new(),
            last_typing: Instant::now() - Duration::from_secs(60),
            scroll_until: None,
            voice: None,
            self_mute: false,
            self_deaf: false,
            voice_members: HashMap::new(),
            speaking: HashSet::new(),
            incoming_call: None,
            stage_open: true,
            mic_level: 0.0,
            devices: None,
            mic_test: None,
            streaming: false,
            stream_live: false,
            watching: None,
            stream_tex: None,
            stream_fullscreen: false,
            stream_picker: false,
            windows: Vec::new(),
            stream_windows_tab: false,
            stream_window: None,
            monitors: Vec::new(),
            users: HashMap::new(),
            presence: HashMap::new(),
            friends: Vec::new(),
            mentions: HashMap::new(),
            my_status: "online".into(),
            status_menu: false,
            profile: None,
            friends_open: true,
            friends_tab: 0,
            friend_input: String::new(),
            msg_menu: None,
            user_menu: None,
            user_menu_invite: false,
            channel_menu: None,
            rename_channel: None,
            thread_new_name: String::new(),
            guild_menu: None,
            confirm_leave: None,
            server_dialog: None,
            pins: None,
            search: None,
            threads: None,
            switcher: None,
            complete_sel: 0,
            last_title: String::new(),
            window_focused: true,
            attention: false,
            menu_frame: 0,
            frame_no: 0,
            toast: None,
            mem_mb: 0.0,
            mem_checked: Instant::now() - Duration::from_secs(10),
            has_saved: store::load().is_some(),
        };
        let _ = app.cmd.send(Cmd::VoiceOptions(voice_opts(&app.settings)));
        PTT_VK.store(
            if app.settings.ptt {
                app.settings.ptt_key
            } else {
                0
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        spawn_ptt_watcher(cc.egui_ctx.clone());
        if let Some((token, bot)) = store::load() {
            let proxy = app.proxy_input.clone();
            app.start_login(Auth { token, bot, proxy });
        }
        app
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some((text.into(), Instant::now(), error));
    }

    // ---------- вход / выход ----------

    fn login_from_input(&mut self) {
        let raw = self.token_input.trim().trim_matches('"').to_string();
        let token = match raw.strip_prefix("Bot ") {
            Some(t) => {
                self.bot_input = true;
                t.trim().to_string()
            }
            None => raw,
        };
        if token.is_empty() {
            self.login_error = Some("Вставьте токен".into());
            return;
        }
        let bot = self.bot_input;
        let proxy = self.proxy_input.trim().to_string();
        store::save_proxy(&proxy);
        self.start_login(Auth { token, bot, proxy });
    }

    fn start_login(&mut self, auth: Auth) {
        self.pending = Some(auth.clone());
        self.logging_in = true;
        self.login_error = None;
        self.status = Status::Connecting;
        let _ = self.cmd.send(Cmd::Login(auth));
    }

    /// `forget` — удалить сохранённый вход. Делается только по кнопке «Выйти»
    /// или когда Discord сам отклонил токен; из-за проблем со связью вход не теряется.
    fn logout(&mut self, error: Option<String>, forget: bool) {
        let _ = self.cmd.send(Cmd::Logout);
        if forget {
            store::clear();
        }
        self.has_saved = store::load().is_some();
        self.guilds = Vec::new();
        self.dms = Vec::new();
        self.cache = HashMap::new();
        self.lru = VecDeque::new();
        self.unread = HashMap::new();
        self.last_channel = HashMap::new();
        self.media.clear();
        self.sel_guild = None;
        self.sel_channel = None;
        self.pending = None;
        self.logging_in = false;
        self.settings_open = false;
        self.draft.clear();
        self.token_input.clear();
        self.me_name.clear();
        self.me_avatar = None;
        self.screen = Screen::Login;
        self.login_error = error;
    }

    // ---------- навигация ----------

    fn select_guild(&mut self, guild: Option<u64>) {
        self.sel_guild = guild;
        let remembered = self.last_channel.get(&guild).copied();
        let target = match guild {
            Some(gid) => {
                let Some(g) = self.guilds.iter().find(|g| g.id == gid) else {
                    self.sel_channel = None;
                    return;
                };
                remembered
                    .filter(|id| g.channels.iter().any(|c| c.id == *id))
                    .or_else(|| {
                        channel_rows(g).into_iter().find_map(|r| match r {
                            Row::Chan(c) if is_text(c.kind) => Some(c.id),
                            _ => None,
                        })
                    })
            }
            None => remembered.filter(|id| self.dms.iter().any(|d| d.id == *id)),
        };
        match target {
            Some(id) => self.open_channel(id),
            None => self.sel_channel = None,
        }
    }

    fn open_channel(&mut self, id: u64) {
        if self.sel_channel != Some(id) {
            if self.editing.take().is_some() {
                self.draft.clear();
            }
            self.reply_to = None;
        }
        self.sel_channel = Some(id);
        self.scroll_until = Some(Instant::now() + Duration::from_millis(1500));
        if let Some(g) = self.sel_guild {
            if self.member_sub != Some((g, id)) {
                self.member_sub = Some((g, id));
                let _ = self.cmd.send(Cmd::SubscribeMembers {
                    guild_id: g,
                    channel_id: id,
                });
            }
        }
        self.last_channel.insert(self.sel_guild, id);
        self.unread.remove(&id);
        self.mentions.remove(&id);
        self.friends_open = false;
        self.lru.retain(|x| *x != id);
        self.lru.push_back(id);
        while self.lru.len() > self.settings.cached_channels {
            if let Some(old) = self.lru.pop_front() {
                self.cache.remove(&old);
            }
        }
        if !self.cache.contains_key(&id) {
            self.cache.insert(
                id,
                ChannelCache {
                    msgs: VecDeque::new(),
                    loading: true,
                    has_more: false,
                    error: None,
                },
            );
            let _ = self.cmd.send(Cmd::FetchMessages {
                channel_id: id,
                before: None,
            });
        }
    }

    fn load_older(&mut self, id: u64) {
        if let Some(c) = self.cache.get_mut(&id) {
            if c.loading {
                return;
            }
            if let Some(first) = c.msgs.front() {
                c.loading = true;
                let _ = self.cmd.send(Cmd::FetchMessages {
                    channel_id: id,
                    before: Some(first.id),
                });
            }
        }
    }

    fn upload(&mut self, channel_id: u64, path: PathBuf) {
        let content = std::mem::take(&mut self.draft).trim().to_string();
        let _ = self.cmd.send(Cmd::Upload {
            channel_id,
            path,
            content,
        });
    }

    // ---------- события из сети ----------

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ctx, ev);
        }
    }

    fn on_event(&mut self, ctx: &egui::Context, ev: Event) {
        match ev {
            Event::Status(s) => self.status = s,
            Event::Ready {
                user_id,
                username,
                avatar,
                guilds,
                dms,
                presences,
                friends,
                status,
                custom_status,
            } => {
                self.presence = presences.into_iter().collect();
                for f in &friends {
                    self.users.insert(f.id, f.name.clone());
                }
                for d in &dms {
                    for (id, name, _) in &d.recipients {
                        self.users.insert(*id, name.clone());
                    }
                }
                self.friends = friends;
                if let Some(st) = status.filter(|s| !s.is_empty()) {
                    self.settings.status = st;
                }
                if let Some(c) = custom_status {
                    self.settings.custom_status = c;
                }
                self.my_status = self.settings.status.clone();
                let _ = self.cmd.send(Cmd::SetPresence {
                    status: self.settings.status.clone(),
                    custom: self.settings.custom_status.clone(),
                });
                self.status = Status::Online;
                self.me_id = user_id;
                self.me_name = username;
                self.me_avatar = avatar;
                self.guilds = guilds;
                self.dms = dms;
                self.logging_in = false;
                if self.screen == Screen::Login {
                    self.screen = Screen::Main;
                    if self.remember {
                        if let Some(a) = &self.pending {
                            store::save(&a.token, a.bot);
                        }
                    }
                    self.token_input.clear();
                }
                self.cache = HashMap::new();
                self.lru = VecDeque::new();
                self.member_sub = None;
                self.member_lists = HashMap::new();
                self.voice_members = HashMap::new();
                for g in &self.guilds {
                    for m in &g.voice {
                        self.voice_members
                            .entry(m.channel_id)
                            .or_default()
                            .push(m.clone());
                    }
                }
                let guild = self
                    .sel_guild
                    .filter(|id| self.guilds.iter().any(|g| g.id == *id));
                self.select_guild(guild);
            }
            Event::GuildCreate(g) => {
                let ids: HashSet<u64> = g.channels.iter().map(|c| c.id).collect();
                self.voice_members.retain(|cid, _| !ids.contains(cid));
                for m in &g.voice {
                    self.voice_members
                        .entry(m.channel_id)
                        .or_default()
                        .push(m.clone());
                }
                match self.guilds.iter_mut().find(|x| x.id == g.id) {
                    Some(existing) => *existing = g,
                    None => self.guilds.push(g),
                }
                if self.sel_guild.is_some() && self.sel_channel.is_none() {
                    self.select_guild(self.sel_guild);
                }
            }
            Event::GuildDelete(id) => {
                self.guilds.retain(|g| g.id != id);
                self.unread.retain(|_, g| *g != id);
                if self.sel_guild == Some(id) {
                    self.select_guild(None);
                }
            }
            Event::GuildChannel { guild_id, channel } => {
                if let Some(g) = self.guilds.iter_mut().find(|g| g.id == guild_id) {
                    match g.channels.iter_mut().find(|c| c.id == channel.id) {
                        Some(c) => *c = channel,
                        None => g.channels.push(channel),
                    }
                }
            }
            Event::DmChannel(dm) => {
                if !self.dms.iter().any(|d| d.id == dm.id) {
                    self.dms.insert(0, dm);
                }
            }
            Event::ChannelDelete { guild_id, id } => {
                match guild_id {
                    Some(gid) => {
                        if let Some(g) = self.guilds.iter_mut().find(|g| g.id == gid) {
                            g.channels.retain(|c| c.id != id);
                        }
                    }
                    None => self.dms.retain(|d| d.id != id),
                }
                self.cache.remove(&id);
                self.unread.remove(&id);
                if self.sel_channel == Some(id) {
                    self.select_guild(self.sel_guild);
                }
            }
            Event::SendFailed {
                channel_id,
                nonce,
                error,
            } => {
                let found = self.cache.get_mut(&channel_id).and_then(|c| {
                    c.msgs
                        .iter_mut()
                        .find(|m| m.nonce.as_deref() == Some(nonce.as_str()))
                });
                match found {
                    Some(m) => {
                        m.sending = false;
                        m.failed = Some(error);
                    }
                    None => self.toast(format!("Не отправлено: {error}"), true),
                }
            }
            Event::MessageCreate(m) => {
                if let Some(list) = self.typing.get_mut(&m.channel_id) {
                    list.retain(|(uid, _, _)| *uid != m.author_id);
                }
                if m.author_id == self.me_id && self.sel_channel == Some(m.channel_id) {
                    self.scroll_until = Some(Instant::now() + Duration::from_millis(1200));
                }
                self.on_message(m)
            }
            Event::Reaction {
                channel_id,
                message_id,
                user_id,
                emoji,
                add,
            } => self.apply_reaction(channel_id, message_id, user_id, &emoji, add),
            Event::ReactionsCleared {
                channel_id,
                message_id,
            } => {
                if let Some(c) = self.cache.get_mut(&channel_id) {
                    if let Some(m) = c.msgs.iter_mut().find(|m| m.id == message_id) {
                        m.reactions.clear();
                    }
                }
            }
            Event::Typing {
                channel_id,
                user_id,
                name,
            } => {
                if user_id != self.me_id {
                    let name = name
                        .or_else(|| {
                            self.dms
                                .iter()
                                .find(|d| d.id == channel_id)
                                .map(|d| d.name.clone())
                        })
                        .unwrap_or_else(|| "Кто-то".into());
                    let list = self.typing.entry(channel_id).or_default();
                    list.retain(|(uid, _, _)| *uid != user_id);
                    list.push((user_id, name, Instant::now()));
                }
            }
            Event::MemberList {
                guild_id,
                groups,
                ops,
            } => {
                let list = self.member_lists.entry(guild_id).or_default();
                if !groups.is_empty() {
                    list.groups = groups.into_iter().collect();
                }
                for op in ops {
                    match op {
                        ListOp::Sync { start, items } => {
                            if start == 0 {
                                list.items.clear();
                            }
                            if list.items.len() < start {
                                list.items.resize(start, None);
                            }
                            for (i, item) in items.into_iter().enumerate() {
                                let idx = start + i;
                                if idx < list.items.len() {
                                    list.items[idx] = Some(item);
                                } else {
                                    list.items.push(Some(item));
                                }
                            }
                        }
                        ListOp::Insert { index, item } => {
                            if index <= list.items.len() {
                                list.items.insert(index, Some(item));
                            } else {
                                list.items.push(Some(item));
                            }
                        }
                        ListOp::Update { index, item } => {
                            if index < list.items.len() {
                                list.items[index] = Some(item);
                            }
                        }
                        ListOp::Delete { index } => {
                            if index < list.items.len() {
                                list.items.remove(index);
                            }
                        }
                    }
                }
            }
            Event::MessageUpdate {
                id,
                channel_id,
                content,
            } => {
                if let Some(c) = self.cache.get_mut(&channel_id) {
                    if let Some(m) = c.msgs.iter_mut().rev().find(|m| m.id == id) {
                        m.content = content;
                        m.edited = true;
                    }
                }
            }
            Event::MessageDelete { id, channel_id } => {
                if let Some(c) = self.cache.get_mut(&channel_id) {
                    c.msgs.retain(|m| m.id != id);
                }
            }
            Event::History {
                channel_id,
                before,
                result,
            } => {
                if let Some(c) = self.cache.get_mut(&channel_id) {
                    c.loading = false;
                    match result {
                        Ok(list) => {
                            c.error = None;
                            c.has_more = list.len() == 50;
                            if before.is_some() {
                                let first = c.msgs.front().map(|m| m.id).unwrap_or(u64::MAX);
                                for m in list.into_iter().rev().filter(|m| m.id < first) {
                                    c.msgs.push_front(m);
                                }
                            } else {
                                c.msgs = list.into();
                            }
                        }
                        Err(e) => c.error = Some(e),
                    }
                }
            }
            Event::Image { key, result } => self.media.on_loaded(ctx, key, result),
            Event::Voice(v) => match v {
                VoiceUi::Connecting {
                    guild_id,
                    channel_id,
                } => {
                    self.voice = Some(VoiceConn {
                        guild_id,
                        channel_id,
                        status: VoiceStatus::Connecting,
                        ping: 0,
                        e2ee: false,
                    });
                }
                VoiceUi::Connected { ping_ms, e2ee } => {
                    if let Some(c) = &mut self.voice {
                        c.status = VoiceStatus::Connected;
                        c.ping = ping_ms;
                        c.e2ee = e2ee;
                    }
                }
                VoiceUi::Disconnected(err) => match err {
                    Some(e) => {
                        if let Some(c) = &mut self.voice {
                            c.status = VoiceStatus::Failed(e.clone());
                        }
                        self.toast(format!("Звонок: {e}"), true);
                    }
                    None => {
                        self.voice = None;
                        self.speaking.clear();
                    }
                },
            },
            Event::MicLevel(l) => self.mic_level = l,
            Event::Speaking { user_id, speaking } => {
                if speaking {
                    self.speaking.insert(user_id);
                } else {
                    self.speaking.remove(&user_id);
                }
            }
            Event::VoiceState { member, left, .. } => {
                let mut prev: Option<VoiceMember> = None;
                for list in self.voice_members.values_mut() {
                    if let Some(i) = list.iter().position(|m| m.user_id == member.user_id) {
                        prev = Some(list.remove(i));
                    }
                }
                self.voice_members.retain(|_, l| !l.is_empty());
                if !left {
                    let mut m = member;
                    if let Some(p) = prev {
                        if m.name.is_none() {
                            m.name = p.name;
                        }
                        if m.avatar.is_none() {
                            m.avatar = p.avatar;
                        }
                    }
                    self.voice_members.entry(m.channel_id).or_default().push(m);
                } else {
                    self.speaking.remove(&member.user_id);
                }
            }
            Event::CallRing {
                channel_id,
                ringing,
            } => {
                let in_call = self
                    .voice
                    .as_ref()
                    .map_or(false, |v| v.channel_id == channel_id);
                if ringing && !in_call {
                    self.incoming_call = Some(channel_id);
                } else if self.incoming_call == Some(channel_id) {
                    self.incoming_call = None;
                }
            }
            Event::CallEnded { channel_id } => {
                if self.incoming_call == Some(channel_id) {
                    self.incoming_call = None;
                }
            }
            Event::StreamLive { .. } => {
                if self.streaming && !self.stream_live {
                    self.stream_live = true;
                    self.toast("Вы в эфире — участники видят ваш экран", false);
                }
            }
            Event::StreamFrame { key, image } => {
                if self.watching.as_ref().map_or(false, |(k, _)| *k == key) {
                    match &mut self.stream_tex {
                        Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            self.stream_tex = Some(ctx.load_texture(
                                "go_live",
                                image,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                    }
                }
            }
            Event::StreamEnded { key, error } => {
                let mine = key.ends_with(&format!(":{}", self.me_id));
                if self.watching.as_ref().map_or(false, |(k, _)| *k == key) {
                    self.watching = None;
                    self.stream_tex = None;
                    if let Some(e) = &error {
                        self.toast(format!("Трансляция: {e}"), true);
                    }
                } else if mine && self.streaming {
                    self.streaming = false;
                    self.stream_live = false;
                    match &error {
                        Some(e) => self.toast(format!("Демонстрация экрана: {e}"), true),
                        None => self.toast("Демонстрация экрана остановлена", false),
                    }
                }
            }
            Event::Presence { user_id, status } => {
                self.presence.insert(user_id, status);
            }
            Event::Relationship { friend, removed } => {
                if let Some(id) = removed {
                    self.friends.retain(|f| f.id != id);
                }
                if let Some(f) = friend {
                    let mut f = f;
                    if f.name == "Неизвестный" {
                        if let Some(n) = self.users.get(&f.id) {
                            f.name = n.clone();
                        }
                    }
                    if f.kind == 3 {
                        self.toast(format!("{} хочет добавить вас в друзья", f.name), false);
                    }
                    self.users.insert(f.id, f.name.clone());
                    match self.friends.iter_mut().find(|x| x.id == f.id) {
                        Some(x) => *x = f,
                        None => self.friends.push(f),
                    }
                }
            }
            Event::Api { tag, result } => self.on_api(ctx, tag, result),
            Event::Info(t) => self.toast(t, false),
            Event::Error(e) => self.toast(e, true),
            Event::Trouble(e) => {
                if self.screen == Screen::Login {
                    self.login_error = Some(e);
                } else {
                    crate::store::log(&e);
                }
            }
            // Токен отклонён — только тогда забываем сохранённый вход.
            Event::AuthFailed(e) => self.logout(Some(e), true),
            Event::Fatal(e) => self.logout(Some(e), false),
        }
    }

    fn on_message(&mut self, m: Msg) {
        let cid = m.channel_id;
        if m.guild_id.is_none() {
            match self.dms.iter().position(|d| d.id == cid) {
                Some(i) => {
                    let mut dm = self.dms.remove(i);
                    dm.last_message = m.id;
                    self.dms.insert(0, dm);
                }
                None if m.author_id != self.me_id => self.dms.insert(
                    0,
                    Dm {
                        id: cid,
                        name: m.author.clone(),
                        last_message: m.id,
                        avatar: Some((m.author_id, m.author_avatar.clone())),
                        group: false,
                        recipients: vec![(m.author_id, m.author.clone(), m.author_avatar.clone())],
                    },
                ),
                None => {}
            }
        }
        self.users.insert(m.author_id, m.author.clone());
        for (id, name) in &m.mentions {
            self.users.insert(*id, name.clone());
        }
        let muted = self.settings.muted.contains(&cid)
            || m.guild_id
                .map_or(false, |g| self.settings.muted.contains(&g));
        if m.author_id != self.me_id && self.sel_channel != Some(cid) && !muted {
            self.unread.insert(cid, m.guild_id.unwrap_or(0));
        }
        self.notify_message(&m);
        let limit = self.settings.messages_per_channel;
        if let Some(c) = self.cache.get_mut(&cid) {
            if c.msgs.iter().rev().take(30).any(|x| x.id == m.id) {
                return;
            }
            // Подтверждение своего сообщения: заменяем «черновик» с тем же nonce.
            if let Some(n) = m.nonce.as_deref() {
                if let Some(pos) = c.msgs.iter().rposition(|x| {
                    (x.sending || x.failed.is_some()) && x.nonce.as_deref() == Some(n)
                }) {
                    c.msgs.remove(pos);
                }
            }
            let pos = c
                .msgs
                .iter()
                .rposition(|x| x.id < m.id)
                .map(|p| p + 1)
                .unwrap_or(0);
            c.msgs.insert(pos, m);
            while c.msgs.len() > limit {
                c.msgs.pop_front();
                c.has_more = true;
            }
        }
    }

    fn refresh_memory(&mut self, ctx: &egui::Context) {
        if self.mem_checked.elapsed() >= Duration::from_secs(2) {
            if let Some(s) = memory_stats::memory_stats() {
                self.mem_mb = s.physical_mem as f64 / 1_048_576.0;
            }
            self.mem_checked = Instant::now();
        }
        if self.settings.show_ram || self.settings_open {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

    fn channel_title(&self) -> Option<(&'static str, String)> {
        let id = self.sel_channel?;
        match self.sel_guild {
            Some(gid) => {
                let g = self.guilds.iter().find(|g| g.id == gid)?;
                let c = g.channels.iter().find(|c| c.id == id)?;
                Some(("#", c.name.clone()))
            }
            None => {
                let d = self.dms.iter().find(|d| d.id == id)?;
                Some(("@", d.name.clone()))
            }
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context, before: &Settings) {
        if before.font_inter != self.settings.font_inter {
            fonts::install(ctx, self.settings.font_inter);
        }
        if before.theme != self.settings.theme || before.accent != self.settings.accent {
            self.pal = theme::palette(&self.settings);
            theme::apply(ctx, &self.pal, self.settings.theme);
        }
        if before.load_images != self.settings.load_images
            || before.animate_gifs != self.settings.animate_gifs
            || before.load_avatars != self.settings.load_avatars
        {
            self.media.clear();
        }
        if voice_opts(before) != voice_opts(&self.settings) {
            let _ = self.cmd.send(Cmd::VoiceOptions(voice_opts(&self.settings)));
        }
        PTT_VK.store(
            if self.settings.ptt {
                self.settings.ptt_key
            } else {
                0
            },
            std::sync::atomic::Ordering::Relaxed,
        );
        settings::save(&self.settings);
    }

    /// Непрозрачный фон для всплывающих окон (у «стеклянных» тем панели полупрозрачные).
    fn solid(&self) -> Color32 {
        match theme::backdrop(self.settings.theme) {
            Some(b) => mix(b.base, Color32::WHITE, 0.06),
            None => self.pal.side,
        }
    }

    // ======================= экран входа =======================

    fn login_ui(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let mut submit = false;
        let mut cancel = false;
        let mut resume_saved = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        // карточка плавно «выезжает» при открытии экрана
                        let appear = ui.ctx().animate_bool_with_time(egui::Id::new("login_appear"), true, 0.35);
                        ui.add_space(48.0 + 16.0 * (1.0 - appear));
                        ui.multiply_opacity(appear.max(0.05));
                        egui::Frame::none()
                            .fill(pal.side)
                            .rounding(22.0)
                            .stroke(Stroke::new(1.0, mix(pal.border, pal.accent, 0.25)))
                            .shadow(Shadow {
                                offset: vec2(0.0, 18.0),
                                blur: 48.0,
                                spread: 0.0,
                                color: Color32::from_black_alpha(if pal.light { 40 } else { 120 }),
                            })
                            .inner_margin(Margin::same(30.0))
                            .show(ui, |ui| {
                                ui.set_width(380.0);
                                ui.vertical_centered(|ui| {
                                    logo(ui, &pal, 68.0);
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new("Rustcord")
                                            .font(FontId::new(30.0, egui::FontFamily::Name("bold".into())))
                                            .color(pal.text),
                                    );
                                    ui.label(RichText::new("Лёгкий клиент Discord на Rust").color(pal.muted));
                                });
                                ui.add_space(22.0);
                                ui.horizontal(|ui| {
                                    if toggle_chip(ui, &pal, "👤 Аккаунт", !self.bot_input).clicked() {
                                        self.bot_input = false;
                                    }
                                    if toggle_chip(ui, &pal, "🤖 Бот (разрешено Discord)", self.bot_input).clicked() {
                                        self.bot_input = true;
                                    }
                                });
                                ui.add_space(10.0);

                                field_label(ui, &pal, "ТОКЕН");
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.token_input)
                                        .password(true)
                                        .hint_text("вставьте токен сюда")
                                        .desired_width(f32::INFINITY),
                                );
                                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    submit = true;
                                }
                                ui.add_space(4.0);
                                ui.checkbox(&mut self.remember, "Запомнить на этом компьютере");

                                ui.add_space(10.0);
                                field_label(ui, &pal, "ПРОКСИ (НЕОБЯЗАТЕЛЬНО)");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.proxy_input)
                                        .hint_text("пусто = системный, или socks5://127.0.0.1:10808")
                                        .desired_width(f32::INFINITY),
                                );
                                ui.label(
                                    RichText::new(
                                        "Если Discord работает только через VPN — включите в нём режим \
                                         «весь трафик» (TUN) или впишите сюда его локальный прокси.",
                                    )
                                    .small()
                                    .color(pal.faint),
                                );
                                ui.add_space(14.0);

                                if self.logging_in {
                                    ui.vertical_centered(|ui| {
                                        ui.spinner();
                                        let text = if self.status == Status::Reconnecting {
                                            "Не удаётся подключиться, пробую ещё раз…"
                                        } else {
                                            "Подключение к Discord…"
                                        };
                                        ui.label(RichText::new(text).color(pal.muted));
                                        if ui.button("Отмена").clicked() {
                                            cancel = true;
                                        }
                                    });
                                } else {
                                    let width = ui.available_width();
                                    if self.has_saved {
                                        if accent_button(ui, &pal, "Войти в сохранённый аккаунт", width, 40.0).clicked() {
                                            resume_saved = true;
                                        }
                                        ui.add_space(6.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(
                                                RichText::new("или вставьте новый токен выше и нажмите Enter")
                                                    .small()
                                                    .color(pal.faint),
                                            );
                                        });
                                    } else if accent_button(ui, &pal, "Войти", width, 40.0).clicked() {
                                        submit = true;
                                    }
                                }

                                if let Some(e) = &self.login_error {
                                    ui.add_space(10.0);
                                    let warn = self.logging_in;
                                    let c = if warn { pal.warn } else { pal.danger };
                                    egui::Frame::none()
                                        .fill(c.gamma_multiply(0.12))
                                        .stroke(Stroke::new(1.0, c.gamma_multiply(0.5)))
                                        .rounding(10.0)
                                        .inner_margin(Margin::same(12.0))
                                        .show(ui, |ui| {
                                            ui.horizontal_top(|ui| {
                                                ui.label(RichText::new(if warn { "⏳" } else { "⚠" }).size(16.0).color(c));
                                                ui.label(RichText::new(e).color(pal.text));
                                            });
                                        });
                                }

                                ui.add_space(14.0);
                                egui::CollapsingHeader::new("Где взять токен бота (разрешённый способ)").show(ui, |ui| {
                                    ui.label(
                                        "1. discord.com/developers/applications → New Application.\n\
                                         2. Вкладка Bot → Reset Token → скопируйте токен.\n\
                                         3. Там же включите «Message Content Intent».\n\
                                         4. OAuth2 → URL Generator: bot + права на чтение и отправку, \
                                         добавьте бота на свой сервер.\n\
                                         5. Вставьте токен и отметьте «Это токен бота».",
                                    );
                                });
                                egui::CollapsingHeader::new("Почему нет входа через браузер или QR-код").show(ui, |ui| {
                                    ui.label(
                                        "У Discord нет официального способа войти в сторонний клиент как в \
                                         полноценный аккаунт. Вход через браузер (OAuth2) даёт приложению \
                                         только ограниченный доступ — читать и писать сообщения с ним нельзя. \
                                         Вход по QR-коду — закрытый протокол официального приложения; им же \
                                         часто пользуются мошенники для угона аккаунтов. Поэтому Rustcord его \
                                         не подделывает. Разрешённые способы — токен бота или токен своего \
                                         аккаунта (на свой риск).",
                                    );
                                    if ui.link("Открыть Discord в браузере").clicked() {
                                        ui.ctx().open_url(egui::OpenUrl::new_tab("https://discord.com/app"));
                                    }
                                    if ui.link("Создать бота в Developer Portal").clicked() {
                                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                                            "https://discord.com/developers/applications",
                                        ));
                                    }
                                });
                                egui::CollapsingHeader::new("Где взять токен своего аккаунта").show(ui, |ui| {
                                    ui.label(
                                        "Откройте discord.com/app в браузере → F12 → Network (Сеть), обновите \
                                         страницу, кликните запрос к discord.com/api и найдите заголовок \
                                         authorization.\n\nНикому не передавайте токен: с ним входят в аккаунт \
                                         без пароля.",
                                    );
                                });
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new(
                                        "⚠ Вход токеном обычного аккаунта в сторонний клиент нарушает правила \
                                         Discord — аккаунт могут заблокировать. С токеном бота это разрешено.",
                                    )
                                    .small()
                                    .color(pal.warn),
                                );
                            });
                        ui.add_space(12.0);
                        ui.label(RichText::new(format!("ОЗУ: {:.0} МБ", self.mem_mb)).small().color(pal.faint));
                    });
                });
            });
        if cancel {
            self.logout(None, false);
        } else if resume_saved {
            if let Some((token, bot)) = store::load() {
                let proxy = self.proxy_input.trim().to_string();
                store::save_proxy(&proxy);
                self.start_login(Auth { token, bot, proxy });
            }
        } else if submit && !self.logging_in {
            self.login_from_input();
        }
    }

    // ======================= главный экран =======================

    fn main_ui(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let mut actions: Vec<Action> = Vec::new();
        let unread_guilds: HashSet<u64> = self.unread.values().copied().collect();
        // упоминания по серверам (0 — личные сообщения)
        let mut guild_mentions: HashMap<u64, u32> = HashMap::new();
        for (cid, n) in &self.mentions {
            let g = self
                .guilds
                .iter()
                .find(|g| g.channels.iter().any(|c| c.id == *cid))
                .map(|g| g.id)
                .unwrap_or(0);
            *guild_mentions.entry(g).or_default() += n;
        }
        let title = self.channel_title();
        let load_avatars = self.settings.load_avatars;

        // --- полоса серверов ---
        egui::SidePanel::left("rail")
            .resizable(false)
            .exact_width(76.0)
            .frame(egui::Frame::none().inner_margin(Margin::symmetric(0.0, 10.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let home = rail_item(
                            ui,
                            &pal,
                            RailIcon::Home,
                            self.sel_guild.is_none(),
                            unread_guilds.contains(&0),
                        )
                        .on_hover_text("Личные сообщения");
                        if let Some(n) = guild_mentions.get(&0) {
                            extra::badge(ui, home.rect.center() + vec2(18.0, 16.0), *n, pal.rail);
                        }
                        if home.clicked() {
                            actions.push(Action::SelectGuild(None));
                        }
                        let (sep, _) = ui.allocate_exact_size(vec2(76.0, 10.0), Sense::hover());
                        ui.painter().hline(
                            sep.center().x - 16.0..=sep.center().x + 16.0,
                            sep.center().y,
                            Stroke::new(2.0, pal.border),
                        );

                        for g in &self.guilds {
                            let tex = match (&g.icon, load_avatars) {
                                (Some(hash), true) => {
                                    let url = discord::guild_icon_url(g.id, hash);
                                    self.media.get(ui.ctx(), &self.cmd, &url, &url, 96, false)
                                }
                                _ => None,
                            };
                            let icon = RailIcon::Guild { tex, name: &g.name };
                            let resp = rail_item(
                                ui,
                                &pal,
                                icon,
                                self.sel_guild == Some(g.id),
                                unread_guilds.contains(&g.id),
                            )
                            .on_hover_text(g.name.as_str());
                            if let Some(n) = guild_mentions.get(&g.id) {
                                extra::badge(
                                    ui,
                                    resp.rect.center() + vec2(18.0, 16.0),
                                    *n,
                                    pal.rail,
                                );
                            }
                            if resp.secondary_clicked() {
                                let pos =
                                    resp.interact_pointer_pos().unwrap_or(resp.rect.right_top());
                                actions.push(Action::GuildMenu(g.id, pos));
                            }
                            if resp.clicked() {
                                actions.push(Action::SelectGuild(Some(g.id)));
                            }
                        }
                        // добавить сервер
                        let (rect, resp) = ui.allocate_exact_size(vec2(76.0, 54.0), Sense::click());
                        let h = ui
                            .ctx()
                            .animate_bool_with_time(resp.id, resp.hovered(), 0.15);
                        let r = 24.0 - 8.0 * h;
                        let c = Rect::from_center_size(rect.center(), vec2(48.0, 48.0));
                        ui.painter().rect_filled(c, r, mix(pal.elevated, pal.ok, h));
                        ui.painter().text(
                            c.center(),
                            Align2::CENTER_CENTER,
                            "+",
                            FontId::proportional(26.0),
                            mix(pal.ok, Color32::WHITE, h),
                        );
                        if resp
                            .on_hover_text("Добавить сервер: присоединиться или создать")
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .clicked()
                        {
                            actions.push(Action::OpenServerDialog);
                        }
                    });
            });

        // --- каналы / диалоги ---
        egui::SidePanel::left("side")
            .resizable(false)
            .exact_width(258.0)
            .frame(card(
                pal.side,
                pal.radius + 4.0,
                Margin {
                    left: 0.0,
                    right: 8.0,
                    top: 8.0,
                    bottom: 8.0,
                },
            ))
            .show(ctx, |ui| {
                let guild = self
                    .sel_guild
                    .and_then(|id| self.guilds.iter().find(|g| g.id == id));

                egui::TopBottomPanel::top("side_head")
                    .frame(egui::Frame::none().inner_margin(Margin::symmetric(16.0, 14.0)))
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        let name = guild.map(|g| g.name.as_str()).unwrap_or("Личные сообщения");
                        ui.add(
                            egui::Label::new(
                                RichText::new(name).size(16.5).strong().color(pal.text),
                            )
                            .truncate(),
                        );
                    });

                egui::TopBottomPanel::bottom("me_panel")
                    .frame(
                        egui::Frame::none()
                            .fill(pal.rail.gamma_multiply(0.9))
                            .inner_margin(Margin::symmetric(10.0, 8.0)),
                    )
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (rect, me_resp) =
                                ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
                            if me_resp
                                .on_hover_text("Сменить статус")
                                .on_hover_cursor(CursorIcon::PointingHand)
                                .clicked()
                            {
                                actions.push(Action::OpenStatusMenu);
                            }
                            let tex = match (&self.me_avatar, load_avatars) {
                                (Some(h), true) => {
                                    let url = discord::avatar_url(self.me_id, h);
                                    self.media.get(ui.ctx(), &self.cmd, &url, &url, 64, false)
                                }
                                _ => None,
                            };
                            paint_avatar(ui, rect, tex, self.me_id, &self.me_name);
                            if self.status == Status::Online {
                                extra::presence_dot(
                                    ui,
                                    &pal,
                                    rect.right_bottom() - vec2(4.0, 4.0),
                                    4.5,
                                    &self.my_status,
                                    pal.rail,
                                );
                            } else {
                                ui.painter().circle_filled(
                                    rect.right_bottom() - vec2(4.0, 4.0),
                                    6.0,
                                    pal.rail,
                                );
                                ui.painter().circle_filled(
                                    rect.right_bottom() - vec2(4.0, 4.0),
                                    4.5,
                                    pal.warn,
                                );
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&self.me_name).strong().color(pal.text),
                                    )
                                    .truncate(),
                                );
                                let status = match self.status {
                                    Status::Online if !self.settings.custom_status.is_empty() => {
                                        self.settings.custom_status.clone()
                                    }
                                    Status::Online => {
                                        extra::presence_text(&self.my_status).to_lowercase()
                                    }
                                    Status::Connecting => "подключение…".into(),
                                    Status::Reconnecting => "переподключение…".into(),
                                };
                                let sr = ui.add(
                                    egui::Label::new(RichText::new(status).small().color(pal.muted))
                                        .truncate()
                                        .sense(Sense::click()),
                                );
                                if sr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                    actions.push(Action::OpenStatusMenu);
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, &pal, "⚙", "Настройки").clicked() {
                                    actions.push(Action::OpenSettings);
                                }
                            });
                        });
                    });

                if let Some(vc) = self.voice.clone() {
                    egui::TopBottomPanel::bottom("voice_panel")
                        .frame(
                            egui::Frame::none()
                                .fill(pal.rail.gamma_multiply(0.9))
                                .inner_margin(Margin::symmetric(10.0, 8.0)),
                        )
                        .show_separator_line(false)
                        .show_inside(ui, |ui| {
                            let (color, status) = match &vc.status {
                                VoiceStatus::Connecting => (pal.warn, "Подключение…".to_string()),
                                VoiceStatus::Connected => {
                                    (pal.ok, format!("Голос подключён · {} мс", vc.ping))
                                }
                                VoiceStatus::Failed(_) => {
                                    (pal.danger, "Ошибка подключения".to_string())
                                }
                            };
                            ui.label(RichText::new(status).strong().color(color));
                            let place = match vc.guild_id {
                                Some(gid) => {
                                    let g = self.guilds.iter().find(|g| g.id == gid);
                                    let ch = g.and_then(|g| {
                                        g.channels.iter().find(|c| c.id == vc.channel_id)
                                    });
                                    format!(
                                        "{} / {}",
                                        ch.map(|c| c.name.as_str()).unwrap_or("канал"),
                                        g.map(|g| g.name.as_str()).unwrap_or("")
                                    )
                                }
                                None => self
                                    .dms
                                    .iter()
                                    .find(|d| d.id == vc.channel_id)
                                    .map(|d| format!("Звонок: {}", d.name))
                                    .unwrap_or_else(|| "Личный звонок".into()),
                            };
                            ui.add(
                                egui::Label::new(RichText::new(place).small().color(pal.muted))
                                    .truncate(),
                            );
                            if vc.e2ee {
                                ui.label(
                                    RichText::new("🔒 сквозное шифрование")
                                        .small()
                                        .color(pal.faint),
                                );
                            }
                            if vc.status == VoiceStatus::Connected {
                                level_meter(
                                    ui,
                                    &pal,
                                    if self.self_mute { 0.0 } else { self.mic_level },
                                    self.settings.mic_threshold,
                                    6.0,
                                );
                            }
                            if let VoiceStatus::Failed(e) = &vc.status {
                                ui.label(RichText::new(e).small().color(pal.danger));
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let mic = Icon::Mic;
                                let mic_tip = if self.self_mute {
                                    "Включить микрофон"
                                } else {
                                    "Выключить микрофон"
                                };
                                if voice_button(ui, &pal, mic, mic_tip, self.self_mute).clicked() {
                                    actions.push(Action::ToggleMute);
                                }
                                let deaf_tip = if self.self_deaf {
                                    "Включить звук"
                                } else {
                                    "Выключить звук"
                                };
                                if voice_button(
                                    ui,
                                    &pal,
                                    Icon::Headphones,
                                    deaf_tip,
                                    self.self_deaf,
                                )
                                .clicked()
                                {
                                    actions.push(Action::ToggleDeaf);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let (rect, resp) =
                                        ui.allocate_exact_size(vec2(44.0, 32.0), Sense::click());
                                    let fill = if resp.hovered() {
                                        pal.danger
                                    } else {
                                        pal.danger.gamma_multiply(0.85)
                                    };
                                    ui.painter().rect_filled(rect, 8.0, fill);
                                    draw_icon(
                                        ui.painter(),
                                        Icon::HangUp,
                                        rect.center(),
                                        18.0,
                                        Color32::WHITE,
                                        false,
                                    );
                                    if resp
                                        .on_hover_text("Отключиться")
                                        .on_hover_cursor(CursorIcon::PointingHand)
                                        .clicked()
                                    {
                                        actions.push(Action::LeaveVoice);
                                    }
                                });
                            });
                        });
                }

                egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(Margin::symmetric(8.0, 4.0)))
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                match guild {
                                    Some(g) => {
                                        let mut current_cat: Option<u64> = None;
                                        for row in channel_rows(g) {
                                            match row {
                                                Row::Category(id, name) => {
                                                    current_cat = Some(id);
                                                    ui.add_space(10.0);
                                                    let collapsed = self.collapsed.contains(&id);
                                                    let arrow =
                                                        if collapsed { "›" } else { "⌄" };
                                                    let text = RichText::new(format!(
                                                        "{arrow}  {}",
                                                        name.to_uppercase()
                                                    ))
                                                    .size(11.5)
                                                    .strong()
                                                    .color(pal.faint);
                                                    if ui
                                                        .add(
                                                            egui::Label::new(text)
                                                                .sense(Sense::click())
                                                                .truncate(),
                                                        )
                                                        .clicked()
                                                    {
                                                        actions.push(Action::ToggleCategory(id));
                                                    }
                                                }
                                                Row::Chan(c) => {
                                                    let selected = self.sel_channel == Some(c.id);
                                                    if let Some(cat) = current_cat {
                                                        if self.collapsed.contains(&cat)
                                                            && !selected
                                                        {
                                                            continue;
                                                        }
                                                    }
                                                    let unread = self.unread.contains_key(&c.id);
                                                    let thread = extra::is_thread(c.kind);
                                                    let forum = matches!(c.kind, 15 | 16);
                                                    let (glyph, enabled) = if thread {
                                                        ("    ↳", true)
                                                    } else if is_text(c.kind) {
                                                        (if c.kind == 5 { "📢" } else { "#" }, true)
                                                    } else if is_voice(c.kind) {
                                                        ("🔊", true)
                                                    } else if forum {
                                                        ("💬", true)
                                                    } else {
                                                        ("💬", false)
                                                    };
                                                    let resp = nav_row(
                                                        ui,
                                                        &pal,
                                                        selected,
                                                        unread,
                                                        enabled,
                                                        Leading::Glyph(glyph),
                                                        &c.name,
                                                        if thread { 28.0 } else { 32.0 },
                                                    );
                                                    if resp.secondary_clicked() {
                                                        if let Some(p) = resp.interact_pointer_pos() {
                                                            actions.push(Action::ChannelMenu(Some(g.id), c.id, p));
                                                        }
                                                    }
                                                    if let Some(n) = self.mentions.get(&c.id) {
                                                        extra::badge(
                                                            ui,
                                                            resp.rect.right_center()
                                                                - vec2(18.0, 0.0),
                                                            *n,
                                                            pal.side,
                                                        );
                                                    }
                                                    let in_this = self
                                                        .voice
                                                        .as_ref()
                                                        .map_or(false, |v| v.channel_id == c.id);
                                                    if is_voice(c.kind) {
                                                        if resp
                                                            .on_hover_text(if in_this {
                                                                "Вы здесь"
                                                            } else {
                                                                "Подключиться к голосовому каналу"
                                                            })
                                                            .clicked()
                                                            && !in_this
                                                        {
                                                            actions.push(Action::JoinVoice(
                                                                Some(g.id),
                                                                c.id,
                                                            ));
                                                        }
                                                        if let Some(members) =
                                                            self.voice_members.get(&c.id)
                                                        {
                                                            for m in members {
                                                                let name = m
                                                                    .name
                                                                    .clone()
                                                                    .unwrap_or_else(|| {
                                                                        member_name(
                                                                            &self.member_lists,
                                                                            g.id,
                                                                            m.user_id,
                                                                        )
                                                                    });
                                                                let tex =
                                                                    match (&m.avatar, load_avatars)
                                                                    {
                                                                        (Some(h), true) => {
                                                                            let url =
                                                                                discord::avatar_url(
                                                                                    m.user_id, h,
                                                                                );
                                                                            self.media.get(
                                                                                ui.ctx(),
                                                                                &self.cmd,
                                                                                &url,
                                                                                &url,
                                                                                64,
                                                                                false,
                                                                            )
                                                                        }
                                                                        _ => None,
                                                                    };
                                                                let vr = voice_member_row(
                                                                    ui,
                                                                    &pal,
                                                                    tex,
                                                                    m,
                                                                    &name,
                                                                    self.speaking
                                                                        .contains(&m.user_id),
                                                                );
                                                                if vr.secondary_clicked() {
                                                                    if let Some(p) = vr.interact_pointer_pos() {
                                                                        actions.push(Action::UserMenu(m.user_id, p));
                                                                    }
                                                                }
                                                                if vr.clicked() {
                                                                    actions.push(Action::OpenProfile(m.user_id));
                                                                }
                                                            }
                                                        }
                                                    } else if forum && resp.clicked() {
                                                        actions.push(Action::OpenThreads(c.id));
                                                    } else if enabled && resp.clicked() {
                                                        actions.push(Action::OpenChannel(c.id));
                                                    } else if !enabled {
                                                        resp.on_hover_text(
                                                            "Этот тип канала пока не поддерживается",
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    None => {
                                        let fresp = nav_row(
                                            ui,
                                            &pal,
                                            self.sel_channel.is_none() && self.friends_open,
                                            false,
                                            true,
                                            Leading::Glyph("👥"),
                                            "Друзья",
                                            40.0,
                                        );
                                        let pending =
                                            self.friends.iter().filter(|f| f.kind == 3).count();
                                        if pending > 0 {
                                            extra::badge(
                                                ui,
                                                fresp.rect.right_center() - vec2(18.0, 0.0),
                                                pending as u32,
                                                pal.side,
                                            );
                                        }
                                        if fresp.clicked() {
                                            actions.push(Action::OpenFriends);
                                        }
                                        ui.add_space(8.0);
                                        ui.label(
                                            RichText::new("  ЛИЧНЫЕ СООБЩЕНИЯ")
                                                .size(11.5)
                                                .strong()
                                                .color(pal.faint),
                                        );
                                        ui.add_space(2.0);
                                        if self.dms.is_empty() {
                                            ui.add_space(12.0);
                                            ui.label(
                                                RichText::new("Диалогов пока нет").color(pal.muted),
                                            );
                                        }
                                        for d in &self.dms {
                                            let tex = match (&d.avatar, load_avatars) {
                                                (Some((uid, Some(hash))), true) => {
                                                    let url = discord::avatar_url(*uid, hash);
                                                    self.media.get(
                                                        ui.ctx(),
                                                        &self.cmd,
                                                        &url,
                                                        &url,
                                                        64,
                                                        false,
                                                    )
                                                }
                                                _ => None,
                                            };
                                            let color_id =
                                                d.avatar.as_ref().map(|(u, _)| *u).unwrap_or(d.id);
                                            let lead = Leading::Avatar {
                                                tex,
                                                id: color_id,
                                                name: &d.name,
                                                group: d.group,
                                            };
                                            let resp = nav_row(
                                                ui,
                                                &pal,
                                                self.sel_channel == Some(d.id),
                                                self.unread.contains_key(&d.id),
                                                true,
                                                lead,
                                                &d.name,
                                                42.0,
                                            );
                                            if let Some((uid, _)) = &d.avatar {
                                                if let Some(st) = self.presence.get(uid) {
                                                    if st != "offline" {
                                                        extra::presence_dot(
                                                            ui,
                                                            &pal,
                                                            pos2(
                                                                resp.rect.left() + 36.0,
                                                                resp.rect.center().y + 11.0,
                                                            ),
                                                            4.5,
                                                            st,
                                                            pal.side,
                                                        );
                                                    }
                                                }
                                            }
                                            if let Some(n) = self.mentions.get(&d.id) {
                                                extra::badge(
                                                    ui,
                                                    resp.rect.right_center() - vec2(18.0, 0.0),
                                                    *n,
                                                    pal.side,
                                                );
                                            }
                                            if resp.clicked() {
                                                actions.push(Action::OpenChannel(d.id));
                                            }
                                            if resp.secondary_clicked() {
                                                if let Some(p) = resp.interact_pointer_pos() {
                                                    match (&d.avatar, d.group) {
                                                        (Some((uid, _)), false) => {
                                                            actions.push(Action::UserMenu(*uid, p))
                                                        }
                                                        _ => actions.push(Action::ChannelMenu(None, d.id, p)),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                    });
            });

        // --- заголовок канала ---
        let show_members =
            (self.sel_guild.is_some() || self.sel_channel.is_some()) && self.members_open;
        egui::TopBottomPanel::top("channel_head")
            .frame(
                card(
                    pal.header,
                    pal.radius + 4.0,
                    Margin {
                        left: 0.0,
                        right: 8.0,
                        top: 8.0,
                        bottom: 4.0,
                    },
                )
                .inner_margin(Margin::symmetric(18.0, 10.0)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    match &title {
                        Some((icon, name)) => {
                            ui.label(RichText::new(*icon).size(20.0).color(pal.faint));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(name).size(16.5).strong().color(pal.text),
                                )
                                .truncate(),
                            );
                        }
                        None => {
                            ui.label(
                                RichText::new("Rustcord")
                                    .size(16.5)
                                    .strong()
                                    .color(pal.text),
                            );
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(vc) = &self.voice {
                            let here = match vc.guild_id {
                                None => self.sel_channel == Some(vc.channel_id),
                                Some(g) => self.sel_guild == Some(g),
                            };
                            if here {
                                let tip = if self.stage_open {
                                    "Скрыть окно звонка"
                                } else {
                                    "Показать окно звонка"
                                };
                                if icon_button(ui, &pal, "📺", tip).clicked() {
                                    actions.push(Action::ToggleStage);
                                }
                            }
                        }
                        if let (None, Some(cid)) = (self.sel_guild, self.sel_channel) {
                            let in_call =
                                self.voice.as_ref().map_or(false, |v| v.channel_id == cid);
                            if !in_call && icon_button(ui, &pal, "📞", "Позвонить").clicked()
                            {
                                actions.push(Action::CallDm(cid));
                            }
                        }
                        if self.sel_guild.is_some() || self.sel_channel.is_some() {
                            let tip = if show_members {
                                "Скрыть участников"
                            } else {
                                "Показать участников"
                            };
                            if icon_button(ui, &pal, "👥", tip).clicked() {
                                actions.push(Action::ToggleMembers);
                            }
                        }
                        if let Some(cid) = self.sel_channel {
                            if icon_button(ui, &pal, "🔍", "Поиск (Ctrl+F)").clicked() {
                                actions.push(Action::OpenSearch);
                            }
                            if icon_button(ui, &pal, "📌", "Закреплённые сообщения").clicked()
                            {
                                actions.push(Action::OpenPins(cid));
                            }
                            let kind = self
                                .sel_guild
                                .and_then(|g| self.guilds.iter().find(|x| x.id == g))
                                .and_then(|g| g.channels.iter().find(|c| c.id == cid))
                                .map(|c| (c.kind, c.parent));
                            match kind {
                                Some((0 | 5, _)) => {
                                    if icon_button(ui, &pal, "🧵", "Ветки").clicked() {
                                        actions.push(Action::OpenThreads(cid));
                                    }
                                }
                                Some((10..=12, Some(parent))) => {
                                    if icon_button(ui, &pal, "↰", "К родительскому каналу")
                                        .clicked()
                                    {
                                        actions.push(Action::Jump(self.sel_guild, parent));
                                    }
                                }
                                _ => {}
                            }
                        } else if self.sel_guild.is_some() {
                            if icon_button(ui, &pal, "🔍", "Поиск (Ctrl+F)").clicked() {
                                actions.push(Action::OpenSearch);
                            }
                        }
                        if self.settings.show_ram {
                            chip(ui, &pal, &format!("ОЗУ {:.0} МБ", self.mem_mb), pal.muted);
                        }
                        if self.status != Status::Online {
                            chip(ui, &pal, "переподключение…", pal.warn);
                        }
                    });
                });
            });

        // --- участники ---
        if self.sel_guild.is_some() || self.sel_channel.is_some() {
            let gid = self.sel_guild.unwrap_or(0);
            let is_bot = self.pending.as_ref().map_or(false, |a| a.bot);
            egui::SidePanel::right("members")
                .resizable(false)
                .exact_width(248.0)
                .frame(
                    card(
                        pal.side,
                        pal.radius + 4.0,
                        Margin {
                            left: 4.0,
                            right: 8.0,
                            top: 4.0,
                            bottom: 8.0,
                        },
                    )
                    .inner_margin(Margin::symmetric(8.0, 10.0)),
                )
                .show_animated(ctx, show_members, |ui| {
                    let guild = self.guilds.iter().find(|g| g.id == gid);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            if guild.is_none() {
                                // личка или группа: собеседники + вы
                                let dm = self
                                    .sel_channel
                                    .and_then(|c| self.dms.iter().find(|d| d.id == c));
                                let Some(dm) = dm else { return };
                                let in_call: HashSet<u64> = self
                                    .voice_members
                                    .get(&dm.id)
                                    .map(|l| l.iter().map(|m| m.user_id).collect())
                                    .unwrap_or_default();
                                let mut people: Vec<(u64, String, Option<String>)> = vec![(
                                    self.me_id,
                                    self.me_name.clone(),
                                    self.me_avatar.clone(),
                                )];
                                people.extend(dm.recipients.iter().cloned());
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format!("УЧАСТНИКИ — {}", people.len()))
                                        .size(11.5)
                                        .strong()
                                        .color(pal.faint),
                                );
                                for (id, name, avatar) in &people {
                                    let tex = match (avatar, load_avatars) {
                                        (Some(h), true) => {
                                            let url = discord::avatar_url(*id, h);
                                            self.media.get(
                                                ui.ctx(),
                                                &self.cmd,
                                                &url,
                                                &url,
                                                64,
                                                false,
                                            )
                                        }
                                        _ => None,
                                    };
                                    let status = if in_call.contains(id) { "online" } else { "" };
                                    let presence = self.presence.get(id).map(String::as_str);
                                    let status = if in_call.contains(id) {
                                        "online"
                                    } else {
                                        presence.unwrap_or("")
                                    };
                                    let resp =
                                        member_row(ui, &pal, tex, *id, name, status, None, false);
                                    if resp.clicked() {
                                        actions.push(Action::OpenProfile(*id));
                                    }
                                    if resp.secondary_clicked() {
                                        if let Some(p) = resp.interact_pointer_pos() {
                                            actions.push(Action::UserMenu(*id, p));
                                        }
                                    }
                                    if in_call.contains(id) {
                                        resp.on_hover_text("В звонке");
                                    }
                                }
                                return;
                            }
                            if is_bot {
                                ui.label(
                                    RichText::new(
                                        "Список участников доступен только при входе аккаунтом.",
                                    )
                                    .color(pal.muted),
                                );
                                return;
                            }
                            let Some(list) = self.member_lists.get(&gid) else {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(
                                        RichText::new("Загрузка участников…").color(pal.muted),
                                    );
                                });
                                return;
                            };
                            let mut group_color: Option<Color32> = None;
                            let mut offline = false;
                            for item in list.items.iter().flatten() {
                                match item {
                                    ListItem::Group { id, count } => {
                                        offline = id == "offline";
                                        let (title, color) = match id.as_str() {
                                            "online" => ("В сети".to_string(), 0),
                                            "offline" => ("Не в сети".to_string(), 0),
                                            other => guild
                                                .and_then(|g| g.roles.get(other))
                                                .map(|(n, c)| (n.clone(), *c))
                                                .unwrap_or_else(|| ("Участники".into(), 0)),
                                        };
                                        group_color = (color != 0).then(|| {
                                            Color32::from_rgb(
                                                (color >> 16) as u8,
                                                (color >> 8) as u8,
                                                color as u8,
                                            )
                                        });
                                        let count = list.groups.get(id).copied().unwrap_or(*count);
                                        ui.add_space(10.0);
                                        ui.label(
                                            RichText::new(format!(
                                                "{} — {count}",
                                                title.to_uppercase()
                                            ))
                                            .size(11.5)
                                            .strong()
                                            .color(pal.faint),
                                        );
                                    }
                                    ListItem::Member {
                                        id,
                                        name,
                                        avatar,
                                        status,
                                    } => {
                                        let tex = match (avatar, load_avatars) {
                                            (Some(h), true) => {
                                                let url = discord::avatar_url(*id, h);
                                                self.media.get(
                                                    ui.ctx(),
                                                    &self.cmd,
                                                    &url,
                                                    &url,
                                                    64,
                                                    false,
                                                )
                                            }
                                            _ => None,
                                        };
                                        let resp = member_row(
                                            ui,
                                            &pal,
                                            tex,
                                            *id,
                                            name,
                                            status,
                                            group_color,
                                            offline,
                                        );
                                        if resp.clicked() {
                                            actions.push(Action::OpenProfile(*id));
                                        }
                                        if resp.secondary_clicked() {
                                            if let Some(p) = resp.interact_pointer_pos() {
                                                actions.push(Action::UserMenu(*id, p));
                                            }
                                        }
                                        resp.on_hover_cursor(CursorIcon::PointingHand);
                                    }
                                }
                            }
                        });
                });
        }

        // --- поле ввода ---
        if let (Some(cid), Some((icon, name))) = (self.sel_channel, &title) {
            let typing_text = typing_line(self.typing.get(&cid));
            let (comp_start, comps) = self.completions(cid);
            if self.complete_sel >= comps.len() {
                self.complete_sel = 0;
            }
            let mut comp_accept: Option<usize> = None;
            egui::TopBottomPanel::bottom("composer")
                .frame(
                    card(
                        pal.main,
                        pal.radius + 4.0,
                        Margin {
                            left: 0.0,
                            right: 8.0,
                            top: 0.0,
                            bottom: 8.0,
                        },
                    )
                    .rounding(Rounding {
                        nw: 0.0,
                        ne: 0.0,
                        sw: pal.radius + 4.0,
                        se: pal.radius + 4.0,
                    })
                    .inner_margin(Margin {
                        left: 16.0,
                        right: 16.0,
                        top: 2.0,
                        bottom: 12.0,
                    }),
                )
                .show_separator_line(false)
                .show(ctx, |ui| {
                    let bar = match (&self.editing, &self.reply_to) {
                        (Some(_), _) => Some("Редактирование сообщения · Esc — отмена".to_string()),
                        (None, Some((_, author))) => {
                            Some(format!("Ответ для {author} · Esc — отмена"))
                        }
                        _ => None,
                    };
                    if let Some(text) = bar {
                        egui::Frame::none()
                            .fill(pal.side)
                            .rounding(Rounding {
                                nw: 12.0,
                                ne: 12.0,
                                sw: 0.0,
                                se: 0.0,
                            })
                            .inner_margin(Margin::symmetric(12.0, 6.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(text).small().color(pal.muted));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if tool_button(ui, &pal, "✕", "Отмена").clicked() {
                                            actions.push(Action::CancelCompose);
                                        }
                                    });
                                });
                            });
                    }
                    let frame = egui::Frame::none()
                        .fill(pal.elevated)
                        .rounding(12.0)
                        .inner_margin(Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if round_button(ui, &pal, "+", pal.muted, pal.hover)
                                    .on_hover_text(
                                        "Отправить файл (можно просто перетащить в окно)",
                                    )
                                    .clicked()
                                {
                                    actions.push(Action::Attach(cid));
                                }
                                let hint = if self.editing.is_some() {
                                    "Изменить сообщение".to_string()
                                } else {
                                    format!("Написать {icon}{name}")
                                };
                                let edit_width = (ui.available_width() - 82.0).max(60.0);
                                let mut tab = false;
                                if !comps.is_empty() {
                                    if ui.input_mut(|i| {
                                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                                    }) {
                                        self.complete_sel = (self.complete_sel + 1) % comps.len();
                                    }
                                    if ui.input_mut(|i| {
                                        i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                                    }) {
                                        self.complete_sel =
                                            (self.complete_sel + comps.len() - 1) % comps.len();
                                    }
                                    tab = ui.input_mut(|i| {
                                        i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                                    });
                                }
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.draft)
                                        .id(egui::Id::new("composer_input"))
                                        .hint_text(hint)
                                        .frame(false)
                                        .lock_focus(true)
                                        .desired_width(edit_width)
                                        .vertical_align(Align::Center),
                                );
                                if tab {
                                    comp_accept = Some(self.complete_sel);
                                }
                                if resp.changed()
                                    && !self.draft.is_empty()
                                    && self.editing.is_none()
                                {
                                    actions.push(Action::Typing(cid));
                                }
                                if resp.has_focus() || resp.lost_focus() {
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        actions.push(Action::CancelCompose);
                                    }
                                    if self.draft.is_empty()
                                        && self.editing.is_none()
                                        && ui.input(|i| i.key_pressed(egui::Key::ArrowUp))
                                    {
                                        actions.push(Action::EditLast(cid));
                                    }
                                }
                                let mut enter = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if enter && !comps.is_empty() {
                                    comp_accept = Some(self.complete_sel);
                                    enter = false;
                                    resp.request_focus();
                                }
                                let emoji_btn = round_button(ui, &pal, "😊", pal.muted, pal.hover)
                                    .on_hover_text("Эмодзи");
                                if emoji_btn.clicked() {
                                    actions.push(Action::OpenPicker(
                                        PickerTarget::Composer,
                                        emoji_btn.rect.left_top() - vec2(300.0, 350.0),
                                    ));
                                }
                                let send_clicked =
                                    round_button(ui, &pal, "➤", Color32::WHITE, pal.accent)
                                        .on_hover_text("Отправить (Enter)")
                                        .clicked();
                                if enter || send_clicked {
                                    let content = self.draft.trim().to_string();
                                    if content.chars().count() > MAX_MESSAGE_LEN {
                                        self.toast = Some((
                                            format!("Сообщение длиннее {MAX_MESSAGE_LEN} символов"),
                                            Instant::now(),
                                            true,
                                        ));
                                    } else if !content.is_empty() {
                                        actions.push(Action::Send(cid, content));
                                        self.draft.clear();
                                    }
                                    resp.request_focus();
                                }
                            });
                        });
                    // подсказки @упоминаний, #каналов и :эмодзи: над полем ввода
                    if !comps.is_empty() {
                        let fr = frame.response.rect;
                        let h = comps.len() as f32 * 32.0 + 34.0;
                        egui::Area::new(egui::Id::new("autocomplete"))
                            .order(egui::Order::Foreground)
                            .fixed_pos(pos2(fr.left(), fr.top() - h - 6.0))
                            .show(ctx, |ui| {
                                egui::Frame::none()
                                    .fill(pal.side)
                                    .rounding(pal.radius)
                                    .stroke(Stroke::new(1.0, pal.border))
                                    .inner_margin(Margin::same(6.0))
                                    .show(ui, |ui| {
                                        ui.set_width(fr.width() - 12.0);
                                        ui.label(
                                            RichText::new("Tab или Enter — вставить, ↑↓ — выбор")
                                                .size(11.0)
                                                .color(pal.faint),
                                        );
                                        for (i, (label, _)) in comps.iter().enumerate() {
                                            let (r, resp) = ui.allocate_exact_size(
                                                vec2(ui.available_width(), 30.0),
                                                Sense::click(),
                                            );
                                            if i == self.complete_sel || resp.hovered() {
                                                ui.painter().rect_filled(r, 6.0, pal.hover);
                                            }
                                            ui.painter().text(
                                                r.left_center() + vec2(10.0, 0.0),
                                                Align2::LEFT_CENTER,
                                                label,
                                                FontId::proportional(14.0),
                                                pal.text,
                                            );
                                            if resp.clicked() {
                                                comp_accept = Some(i);
                                            }
                                        }
                                    });
                            });
                    }
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
                    if let Some(t) = typing_text {
                        ui.painter().text(
                            rect.left_center() + vec2(6.0, 0.0),
                            Align2::LEFT_CENTER,
                            t,
                            FontId::proportional(12.0),
                            pal.muted,
                        );
                        ctx.request_repaint_after(Duration::from_secs(1));
                    }
                });
            if let Some(i) = comp_accept {
                if let Some((_, insert)) = comps.get(i) {
                    self.draft.truncate(comp_start);
                    self.draft.push_str(insert);
                    self.draft.push(' ');
                    let id = egui::Id::new("composer_input");
                    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
                        let end = self.draft.chars().count();
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(
                                egui::text::CCursor::new(end),
                            )));
                        state.store(ctx, id);
                    }
                    ctx.memory_mut(|m| m.request_focus(id));
                    self.complete_sel = 0;
                }
            }
        }

        // --- сцена звонка ---
        let stage = self.stage_tiles(ctx);
        let stage_state = StageState {
            mute: self.self_mute,
            deaf: self.self_deaf,
            me: self.me_id,
            streaming: self.streaming,
            live: self.stream_live,
            watch: self.watching.as_ref().map(|(_, uid)| {
                (
                    *uid,
                    self.stream_tex.as_ref().map(|t| (t.id(), t.size_vec2())),
                )
            }),
        };

        // --- сообщения ---
        let force_bottom = self.scroll_until.map_or(false, |t| Instant::now() < t);
        let me_id = self.me_id;
        let guild_id = self.sel_guild;
        let toolbar_allowed = self.picker.is_none() && self.pending_delete.is_none();
        egui::CentralPanel::default()
            .frame(
                card(
                    pal.main,
                    pal.radius + 4.0,
                    Margin {
                        left: 0.0,
                        right: 8.0,
                        top: 4.0,
                        bottom: if self.sel_channel.is_some() { 0.0 } else { 8.0 },
                    },
                )
                .rounding(if self.sel_channel.is_some() {
                    Rounding {
                        nw: pal.radius + 4.0,
                        ne: pal.radius + 4.0,
                        sw: 0.0,
                        se: 0.0,
                    }
                } else {
                    Rounding::same(pal.radius + 4.0)
                }),
            )
            .show(ctx, |ui| {
                if let Some(tiles) = &stage {
                    let share = if stage_state.watch.is_some() {
                        0.78
                    } else {
                        0.55
                    };
                    let h = (ui.available_height() * share).clamp(240.0, 900.0);
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                    draw_stage(ui, &pal, rect, tiles, &stage_state, &mut actions);
                }
                let Some(cid) = self.sel_channel else {
                    if self.sel_guild.is_none() && stage.is_none() {
                        extra::friends_view(
                            ui,
                            &pal,
                            &self.friends,
                            &self.presence,
                            &mut self.friends_tab,
                            &mut self.friend_input,
                            &mut self.media,
                            &self.cmd,
                            self.settings.load_avatars,
                            &mut actions,
                        );
                    } else if stage.is_none() {
                        empty_state(ui, &pal, "Выберите канал или диалог слева");
                    }
                    return;
                };
                let Some(cache) = self.cache.get(&cid) else {
                    return;
                };
                if cache.loading && cache.msgs.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                    });
                    return;
                }
                let mut mcx = MsgCtx {
                    pal,
                    settings: &self.settings,
                    media: &mut self.media,
                    cmd: &self.cmd,
                    actions: &mut actions,
                    me_id,
                    guild_id,
                    channel_id: cid,
                    toolbar_allowed,
                    guilds: &self.guilds,
                    users: &self.users,
                };
                let out = ui.push_id(cid, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.add_space(8.0);
                            if let Some(e) = &cache.error {
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.label(RichText::new(e).color(pal.danger));
                                });
                            }
                            if cache.has_more {
                                ui.vertical_centered(|ui| {
                                    if cache.loading {
                                        ui.spinner();
                                    } else if ui
                                        .button("Загрузить более ранние сообщения")
                                        .clicked()
                                    {
                                        mcx.actions.push(Action::LoadOlder(cid));
                                    }
                                });
                            } else if cache.error.is_none() {
                                if let Some((icon, name)) = &title {
                                    channel_intro(ui, &pal, icon, name);
                                }
                            }
                            let mut prev: Option<&Msg> = None;
                            for m in &cache.msgs {
                                let new_day = prev.map_or(true, |p| day(&p.time) != day(&m.time));
                                if new_day && !m.time.is_empty() {
                                    day_separator(ui, &pal, day(&m.time));
                                }
                                let grouped = !new_day
                                    && prev.map_or(false, |p| {
                                        p.author_id == m.author_id
                                            && m.reply.is_none()
                                            && snowflake_ms(m.id).saturating_sub(snowflake_ms(p.id))
                                                < 7 * 60 * 1000
                                    });
                                message_ui(ui, &mut mcx, m, grouped);
                                prev = Some(m);
                            }
                            ui.add_space(6.0);
                            if force_bottom {
                                ui.scroll_to_cursor(Some(Align::BOTTOM));
                                ui.ctx().request_repaint();
                            }
                        })
                });
                let scroll = out.inner;
                let distance =
                    scroll.content_size.y - scroll.state.offset.y - scroll.inner_rect.height();
                if distance > 400.0 {
                    let r = scroll.inner_rect;
                    let btn = Rect::from_min_size(
                        pos2(r.right() - 190.0, r.bottom() - 48.0),
                        vec2(170.0, 34.0),
                    );
                    let resp = ui.put(
                        btn,
                        egui::Button::new(
                            RichText::new("↓ К последним")
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(pal.accent)
                        .rounding(17.0),
                    );
                    if resp.clicked() {
                        actions.push(Action::ScrollBottom);
                    }
                }
            });

        // --- перетаскивание файлов ---
        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering_files && self.sel_channel.is_some() {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop"),
            ));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
            painter.rect_stroke(screen.shrink(24.0), 18.0, Stroke::new(3.0, pal.accent));
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "Отпустите, чтобы отправить файл",
                FontId::proportional(24.0),
                Color32::WHITE,
            );
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(cid) = self.sel_channel {
            for path in dropped {
                self.upload(cid, path);
            }
        }

        for action in actions {
            self.handle_action(ctx, action);
        }
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::SelectGuild(g) => self.select_guild(g),
            Action::OpenChannel(id) => self.open_channel(id),
            Action::ToggleCategory(id) => {
                if !self.collapsed.remove(&id) {
                    self.collapsed.insert(id);
                }
            }
            Action::LoadOlder(id) => self.load_older(id),
            Action::Send(channel_id, content) => {
                if let Some((cid, mid)) = self.editing.take() {
                    let _ = self.cmd.send(Cmd::Edit {
                        channel_id: cid,
                        message_id: mid,
                        content,
                    });
                } else {
                    let reply = self.reply_to.take();
                    let reply_to = reply.as_ref().map(|(id, _)| *id);
                    let nonce = discord::nonce();
                    // Сообщение сразу появляется в ленте (полупрозрачным), пока Discord не подтвердит.
                    let local = Msg {
                        id: nonce.parse().unwrap_or(u64::MAX - 1),
                        channel_id,
                        guild_id: self.sel_guild,
                        author_id: self.me_id,
                        author: self.me_name.clone(),
                        author_avatar: self.me_avatar.clone(),
                        content: content.clone(),
                        time: chrono::Local::now().format("%d.%m.%Y %H:%M").to_string(),
                        edited: false,
                        system: false,
                        attachments: Vec::new(),
                        embeds: Vec::new(),
                        reply: reply.map(|(_, author)| discord::Reply {
                            author,
                            content: String::new(),
                        }),
                        reactions: Vec::new(),
                        mentions: Vec::new(),
                        mention_everyone: false,
                        mention_roles: Vec::new(),
                        pinned: false,
                        thread: None,
                        nonce: Some(nonce.clone()),
                        sending: true,
                        failed: None,
                    };
                    if let Some(c) = self.cache.get_mut(&channel_id) {
                        c.msgs.push_back(local);
                    }
                    let _ = self.cmd.send(Cmd::Send {
                        channel_id,
                        content,
                        reply_to,
                        nonce,
                    });
                    self.scroll_until = Some(Instant::now() + Duration::from_millis(1500));
                }
            }
            Action::RetrySend(channel_id, nonce) => {
                let found = self.cache.get_mut(&channel_id).and_then(|c| {
                    c.msgs
                        .iter_mut()
                        .find(|m| m.nonce.as_deref() == Some(nonce.as_str()))
                });
                if let Some(m) = found {
                    m.failed = None;
                    m.sending = true;
                    let _ = self.cmd.send(Cmd::Send {
                        channel_id,
                        content: m.content.clone(),
                        reply_to: None,
                        nonce,
                    });
                }
            }
            Action::DismissFailed(channel_id, nonce) => {
                if let Some(c) = self.cache.get_mut(&channel_id) {
                    c.msgs.retain(|m| {
                        m.nonce.as_deref() != Some(nonce.as_str()) || m.failed.is_none()
                    });
                }
            }
            Action::Attach(cid) => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Выберите файл для отправки")
                    .pick_file()
                {
                    self.upload(cid, path);
                }
            }
            Action::Download(url, filename) => {
                self.toast(format!("Скачиваю {filename}…"), false);
                let _ = self.cmd.send(Cmd::Download { url, filename });
            }
            Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Action::OpenSettings => self.settings_open = true,
            Action::Logout => self.logout(None, true),
            Action::UserMenu(uid, pos) => {
                self.msg_menu = None;
                self.user_menu = Some((uid, pos));
                self.user_menu_invite = false;
                self.menu_frame = self.frame_no;
            }
            Action::ChannelMenu(g, c, pos) => {
                self.channel_menu = Some((g, c, pos));
                self.menu_frame = self.frame_no;
            }
            Action::SetUserVolume(uid, v) => {
                let before = self.settings.clone();
                if (v - 1.0).abs() < 0.01 {
                    self.settings.user_volumes.remove(&uid);
                } else {
                    self.settings.user_volumes.insert(uid, v.clamp(0.0, 2.0));
                }
                self.apply_settings(ctx, &before);
            }
            Action::ToggleMuted(id) => {
                let before = self.settings.clone();
                if !self.settings.muted.remove(&id) {
                    self.settings.muted.insert(id);
                    self.unread.remove(&id);
                    self.unread.retain(|_, g| *g != id);
                    self.toast("Уведомления выключены", false);
                } else {
                    self.toast("Уведомления включены", false);
                }
                self.apply_settings(ctx, &before);
            }
            Action::MarkRead(cid) => {
                self.unread.remove(&cid);
                self.mentions.remove(&cid);
            }
            Action::MarkUnread(cid, g) => {
                self.unread.insert(cid, g.unwrap_or(0));
                if self.sel_channel == Some(cid) {
                    self.toast("Канал отмечен непрочитанным", false);
                }
            }
            Action::CreateThread(cid, mid, name) => {
                let name: String = name.trim().chars().take(90).collect();
                let name = if name.is_empty() {
                    "Обсуждение".to_string()
                } else {
                    name
                };
                self.api(
                    "POST",
                    format!("/channels/{cid}/messages/{mid}/threads"),
                    Some(serde_json::json!({ "name": name, "auto_archive_duration": 1440 })),
                    discord::ApiTag::Done(format!("Ветка «{name}» создана")),
                );
            }
            Action::CreateStandaloneThread(parent, name) => {
                let name: String = name.trim().chars().take(90).collect();
                if !name.is_empty() {
                    self.api(
                        "POST",
                        format!("/channels/{parent}/threads"),
                        Some(serde_json::json!({
                            "name": name,
                            "type": 11,
                            "auto_archive_duration": 1440
                        })),
                        discord::ApiTag::Done(format!("Ветка «{name}» создана")),
                    );
                }
            }
            Action::AskRename(cid) => {
                let name = self
                    .guilds
                    .iter()
                    .flat_map(|g| g.channels.iter())
                    .find(|c| c.id == cid)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                self.rename_channel = Some((cid, name));
            }
            Action::RenameChannel(cid, name) => {
                self.rename_channel = None;
                let name = name.trim().to_string();
                if !name.is_empty() {
                    self.api(
                        "PATCH",
                        format!("/channels/{cid}"),
                        Some(serde_json::json!({ "name": name })),
                        discord::ApiTag::Done("Канал переименован".into()),
                    );
                }
            }
            Action::ChannelInvite(cid) => self.api(
                "POST",
                format!("/channels/{cid}/invites"),
                Some(serde_json::json!({ "max_age": 604800, "max_uses": 0, "temporary": false })),
                discord::ApiTag::Invite,
            ),
            Action::InviteUser(uid, g) => {
                let channel = self
                    .guilds
                    .iter()
                    .find(|x| x.id == g)
                    .and_then(|x| x.channels.iter().find(|c| c.kind == 0).map(|c| c.id));
                match channel {
                    Some(c) => self.api(
                        "POST",
                        format!("/channels/{c}/invites"),
                        Some(serde_json::json!({ "max_age": 604800, "max_uses": 0, "temporary": false })),
                        discord::ApiTag::InviteFor(uid),
                    ),
                    None => self.toast("На сервере нет текстового канала для приглашения", true),
                }
            }
            Action::InsertMention(name) => {
                if !self.draft.is_empty() && !self.draft.ends_with(' ') {
                    self.draft.push(' ');
                }
                self.draft.push('@');
                self.draft.push_str(&name);
                self.draft.push(' ');
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("composer_input")));
            }
            Action::OpenProfileTab(uid, tab) => {
                self.extra_action(ctx, Action::OpenProfile(uid));
                if let Some(p) = self.profile.as_mut() {
                    p.tab = tab;
                }
            }
            a @ (Action::OpenProfile(_)
            | Action::CloseProfile
            | Action::MessageUser(_)
            | Action::OpenStatusMenu
            | Action::SetStatus(_)
            | Action::OpenFriends
            | Action::FriendAdd(_)
            | Action::FriendAccept(_)
            | Action::FriendRequest(_)
            | Action::FriendRemove(_)
            | Action::FriendBlock(_)
            | Action::CopyId(_)
            | Action::GuildMenu(..)
            | Action::AskLeave(_)
            | Action::LeaveGuild(_)
            | Action::CopyInvite(_)
            | Action::OpenServerDialog
            | Action::JoinInvite(_)
            | Action::CreateGuild(_)
            | Action::MessageMenu(..)
            | Action::Pin(..)
            | Action::OpenPins(_)
            | Action::OpenSearch
            | Action::RunSearch(_)
            | Action::OpenThreads(_)
            | Action::OpenThread(..)
            | Action::Jump(..)
            | Action::OpenSwitcher) => self.extra_action(ctx, a),
            Action::ToggleMembers => self.members_open = !self.members_open,
            Action::React {
                channel_id,
                message_id,
                emoji,
                add,
            } => {
                self.apply_reaction(channel_id, message_id, self.me_id, &emoji, add);
                let _ = self.cmd.send(Cmd::React {
                    channel_id,
                    message_id,
                    emoji,
                    add,
                });
            }
            Action::Reply(id, author) => {
                self.editing = None;
                self.reply_to = Some((id, author));
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("composer_input")));
            }
            Action::StartEdit(cid, mid, content) => {
                self.reply_to = None;
                self.editing = Some((cid, mid));
                self.draft = content;
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("composer_input")));
            }
            Action::EditLast(cid) => {
                let last = self.cache.get(&cid).and_then(|c| {
                    c.msgs
                        .iter()
                        .rev()
                        .find(|m| m.author_id == self.me_id && !m.system)
                        .map(|m| (m.id, m.content.clone()))
                });
                if let Some((mid, content)) = last {
                    self.reply_to = None;
                    self.editing = Some((cid, mid));
                    self.draft = content;
                }
            }
            Action::CancelCompose => {
                if self.editing.take().is_some() {
                    self.draft.clear();
                }
                self.reply_to = None;
            }
            Action::AskDelete(cid, mid, preview) => self.pending_delete = Some((cid, mid, preview)),
            Action::CopyText(text) => {
                ctx.copy_text(text);
                self.toast("Текст скопирован", false);
            }
            Action::Report(link) => {
                ctx.copy_text(link);
                ctx.open_url(egui::OpenUrl::new_tab(
                    "https://support.discord.com/hc/requests/new",
                ));
                self.toast(
                    "Ссылка на сообщение скопирована — вставьте её в форму жалобы Discord",
                    false,
                );
            }
            Action::OpenPicker(target, pos) => {
                self.picker = Some((target, pos, ctx.input(|i| i.time)));
            }
            Action::Typing(cid) => {
                if self.last_typing.elapsed() > Duration::from_secs(8) {
                    self.last_typing = Instant::now();
                    let _ = self.cmd.send(Cmd::Typing { channel_id: cid });
                }
            }
            Action::ToggleStage => self.stage_open = !self.stage_open,
            Action::JoinVoice(guild_id, channel_id) => {
                self.stage_open = true;
                let _ = self.cmd.send(Cmd::JoinVoice {
                    guild_id,
                    channel_id,
                });
            }
            Action::LeaveVoice => {
                let _ = self.cmd.send(Cmd::LeaveVoice);
                self.voice = None;
                self.speaking.clear();
                self.streaming = false;
                self.stream_live = false;
                self.watching = None;
                self.stream_tex = None;
            }
            Action::OpenStreamPicker => {
                if self.pending.as_ref().map_or(false, |a| a.bot) {
                    self.toast(
                        "Discord не даёт ботам транслировать экран — войдите обычным аккаунтом",
                        true,
                    );
                } else {
                    self.monitors = stream::monitors();
                    self.windows = stream::windows();
                    if self
                        .stream_window
                        .map_or(false, |id| !self.windows.iter().any(|w| w.0 == id))
                    {
                        self.stream_window = None;
                    }
                    if self.monitors.is_empty() && self.windows.is_empty() {
                        self.toast("Не удалось найти мониторы для захвата", true);
                    } else {
                        if self.settings.stream_monitor >= self.monitors.len() {
                            self.settings.stream_monitor = 0;
                        }
                        self.stream_picker = true;
                    }
                }
            }
            Action::StartStream => {
                self.stream_picker = false;
                settings::save(&self.settings);
                self.streaming = true;
                self.stream_live = false;
                let _ = self.cmd.send(Cmd::StartStream(stream::SendCfg {
                    monitor: self.settings.stream_monitor,
                    window: if self.stream_windows_tab {
                        self.stream_window
                    } else {
                        None
                    },
                    height: self.settings.stream_height,
                    fps: self.settings.stream_fps,
                    quality: self.settings.stream_quality,
                }));
            }
            Action::StopStream => {
                let _ = self.cmd.send(Cmd::StopStream);
            }
            Action::WatchStream(user_id) => {
                if let Some(vc) = &self.voice {
                    let key = match vc.guild_id {
                        Some(g) => format!("guild:{g}:{}:{user_id}", vc.channel_id),
                        None => format!("call:{}:{user_id}", vc.channel_id),
                    };
                    self.stream_tex = None;
                    self.watching = Some((key.clone(), user_id));
                    self.stage_open = true;
                    // Свою трансляцию показываем из локального превью захвата:
                    // «смотреть» себя через сервер нельзя, а запрос на просмотр
                    // своего же ключа мог оборвать собственную трансляцию.
                    if user_id != self.me_id {
                        let _ = self.cmd.send(Cmd::WatchStream(key));
                    }
                }
            }
            Action::ToggleStreamFullscreen => {
                self.stream_fullscreen = !self.stream_fullscreen && self.watching.is_some();
            }
            Action::StopWatching => {
                self.stream_fullscreen = false;
                let own = self
                    .watching
                    .as_ref()
                    .map_or(false, |(_, u)| *u == self.me_id);
                self.watching = None;
                self.stream_tex = None;
                if !own {
                    let _ = self.cmd.send(Cmd::StopWatching);
                }
            }
            Action::ToggleMute => {
                self.self_mute = !self.self_mute;
                if !self.self_mute && self.self_deaf {
                    self.self_deaf = false;
                    let _ = self.cmd.send(Cmd::SetDeaf(false));
                }
                let _ = self.cmd.send(Cmd::SetMute(self.self_mute));
            }
            Action::ToggleDeaf => {
                self.self_deaf = !self.self_deaf;
                let _ = self.cmd.send(Cmd::SetDeaf(self.self_deaf));
            }
            Action::CallDm(cid) => {
                self.stage_open = true;
                let _ = self.cmd.send(Cmd::JoinVoice {
                    guild_id: None,
                    channel_id: cid,
                });
                let _ = self.cmd.send(Cmd::RingCall(cid));
            }
            Action::AcceptCall(cid) => {
                self.stage_open = true;
                self.incoming_call = None;
                let _ = self.cmd.send(Cmd::JoinVoice {
                    guild_id: None,
                    channel_id: cid,
                });
            }
            Action::DeclineCall(cid) => {
                self.incoming_call = None;
                let _ = self.cmd.send(Cmd::DeclineCall(cid));
            }
            Action::ScrollBottom => {
                self.scroll_until = Some(Instant::now() + Duration::from_millis(600));
            }
        }
    }

    fn apply_reaction(
        &mut self,
        channel_id: u64,
        message_id: u64,
        user_id: u64,
        emoji: &Emoji,
        add: bool,
    ) {
        let mine = user_id == self.me_id;
        let Some(c) = self.cache.get_mut(&channel_id) else {
            return;
        };
        let Some(m) = c.msgs.iter_mut().rev().find(|m| m.id == message_id) else {
            return;
        };
        match m.reactions.iter().position(|r| r.emoji == *emoji) {
            Some(i) => {
                let r = &mut m.reactions[i];
                if mine && r.me == add {
                    return; // уже учтено (мы сами нажали, сервер подтвердил)
                }
                if add {
                    r.count += 1;
                } else {
                    r.count = r.count.saturating_sub(1);
                }
                if mine {
                    r.me = add;
                }
                if r.count == 0 {
                    m.reactions.remove(i);
                }
            }
            None if add => m.reactions.push(Reaction {
                emoji: emoji.clone(),
                count: 1,
                me: mine,
            }),
            None => {}
        }
    }

    // ======================= панель эмодзи и удаление =======================

    fn picker_ui(&mut self, ctx: &egui::Context) {
        let Some((target, pos, opened_at)) = self.picker else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let mut chosen: Option<String> = None;
        let area = egui::Area::new(egui::Id::new("emoji_picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(solid)
                    .rounding(12.0)
                    .stroke(Stroke::new(1.0, pal.border))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(336.0);
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for (category, list) in emoji::CATEGORIES {
                                    ui.label(
                                        RichText::new(category.to_uppercase())
                                            .size(11.0)
                                            .strong()
                                            .color(pal.faint),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                                        for e in list.split_whitespace() {
                                            let (rect, resp) = ui.allocate_exact_size(
                                                vec2(36.0, 36.0),
                                                Sense::click(),
                                            );
                                            if resp.hovered() {
                                                ui.painter().rect_filled(rect, 6.0, pal.hover);
                                            }
                                            if ui.is_rect_visible(rect) {
                                                let url = emoji::twemoji_url(e);
                                                let key = format!("72|false|{url}");
                                                let inner = Rect::from_center_size(
                                                    rect.center(),
                                                    vec2(26.0, 26.0),
                                                );
                                                match self
                                                    .media
                                                    .get(ctx, &self.cmd, &key, &url, 72, false)
                                                {
                                                    Some((id, _)) => {
                                                        egui::Image::from_texture(
                                                            SizedTexture::new(id, inner.size()),
                                                        )
                                                        .paint_at(ui, inner);
                                                    }
                                                    None => {
                                                        ui.painter().text(
                                                            rect.center(),
                                                            Align2::CENTER_CENTER,
                                                            e,
                                                            FontId::proportional(20.0),
                                                            pal.text,
                                                        );
                                                    }
                                                }
                                            }
                                            if resp
                                                .on_hover_cursor(CursorIcon::PointingHand)
                                                .clicked()
                                            {
                                                chosen = Some(e.to_string());
                                            }
                                        }
                                    });
                                    ui.add_space(6.0);
                                }
                            });
                    });
            });
        let now = ctx.input(|i| i.time);
        let clicked_outside =
            ctx.input(|i| i.pointer.any_click()) && !area.response.contains_pointer();
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if let Some(e) = chosen {
            match target {
                PickerTarget::Composer => {
                    self.draft.push_str(&e);
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new("composer_input")));
                }
                PickerTarget::React {
                    channel_id,
                    message_id,
                } => {
                    self.picker = None;
                    self.handle_action(
                        ctx,
                        Action::React {
                            channel_id,
                            message_id,
                            emoji: Emoji::unicode(&e),
                            add: true,
                        },
                    );
                    return;
                }
            }
        }
        if escape || (clicked_outside && now - opened_at > 0.15) {
            self.picker = None;
        }
    }

    fn stage_tiles(&mut self, ctx: &egui::Context) -> Option<Vec<Tile>> {
        let vc = self.voice.clone()?;
        if !self.stage_open {
            return None;
        }
        let visible = match vc.guild_id {
            None => self.sel_channel == Some(vc.channel_id),
            Some(g) => self.sel_guild == Some(g),
        };
        if !visible {
            return None;
        }
        let mut members: Vec<VoiceMember> = self
            .voice_members
            .get(&vc.channel_id)
            .cloned()
            .unwrap_or_default();
        if !members.iter().any(|m| m.user_id == self.me_id) {
            members.insert(
                0,
                VoiceMember {
                    user_id: self.me_id,
                    channel_id: vc.channel_id,
                    muted: self.self_mute,
                    deafened: self.self_deaf,
                    streaming: self.streaming,
                    name: Some(self.me_name.clone()),
                    avatar: self.me_avatar.clone(),
                },
            );
        }
        let dm = self.dms.iter().find(|d| d.id == vc.channel_id).cloned();
        let mut tiles = Vec::new();
        for m in members {
            let (mut name, mut avatar) = (m.name.clone(), m.avatar.clone());
            if m.user_id == self.me_id {
                name = Some(self.me_name.clone());
                avatar = self.me_avatar.clone();
            } else if let Some(d) = &dm {
                if let Some((_, n, a)) = d.recipients.iter().find(|(id, _, _)| *id == m.user_id) {
                    name = name.or_else(|| Some(n.clone()));
                    avatar = avatar.or_else(|| a.clone());
                }
            }
            let name = name.unwrap_or_else(|| match vc.guild_id {
                Some(g) => member_name(&self.member_lists, g, m.user_id),
                None => "Участник".into(),
            });
            let tex = match (&avatar, self.settings.load_avatars) {
                (Some(h), true) => {
                    let url = format!(
                        "https://cdn.discordapp.com/avatars/{}/{h}.png?size=128",
                        m.user_id
                    );
                    self.media.get(ctx, &self.cmd, &url, &url, 128, false)
                }
                _ => None,
            };
            let me = m.user_id == self.me_id;
            tiles.push(Tile {
                id: m.user_id,
                name,
                tex,
                muted: if me { self.self_mute } else { m.muted },
                deafened: if me { self.self_deaf } else { m.deafened },
                speaking: self.speaking.contains(&m.user_id),
                streaming: if me { self.streaming } else { m.streaming },
            });
        }
        Some(tiles)
    }

    /// Трансляция на всё окно: Esc, двойной клик или кнопка — обратно.
    fn stream_fullscreen_ui(&mut self, ctx: &egui::Context) {
        if !self.stream_fullscreen {
            return;
        }
        let Some((_, uid)) = self.watching.clone() else {
            self.stream_fullscreen = false;
            return;
        };
        let pal = self.pal;
        let frame = self.stream_tex.as_ref().map(|t| (t.id(), t.size_vec2()));
        let name = if uid == self.me_id {
            "ваша трансляция".to_string()
        } else {
            self.users
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| "участник".into())
        };
        let mut actions = Vec::new();
        egui::Area::new(egui::Id::new("stream_fullscreen"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let rect = ctx.screen_rect();
                ui.allocate_rect(rect, Sense::hover());
                ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
                draw_stream_view(ui, &pal, rect, &name, frame, true, &mut actions);
                let r = ui.interact(rect, ui.id().with("fs_dbl"), Sense::click());
                if r.double_clicked() {
                    actions.push(Action::ToggleStreamFullscreen);
                }
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            actions.push(Action::ToggleStreamFullscreen);
        }
        for a in actions {
            self.handle_action(ctx, a);
        }
    }

    fn stream_picker_ui(&mut self, ctx: &egui::Context) {
        if !self.stream_picker {
            return;
        }
        let pal = self.pal;
        let solid = self.solid();
        let mut start = false;
        let mut open = true;
        egui::Window::new(RichText::new("Демонстрация экрана").strong())
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(solid)
                    .rounding(pal.radius + 6.0)
                    .inner_margin(Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal(|ui| {
                    if toggle_chip(
                        ui,
                        &pal,
                        &format!("🖥  Экраны · {}", self.monitors.len()),
                        !self.stream_windows_tab,
                    )
                    .clicked()
                    {
                        self.stream_windows_tab = false;
                    }
                    if toggle_chip(
                        ui,
                        &pal,
                        &format!("🗗  Окна · {}", self.windows.len()),
                        self.stream_windows_tab,
                    )
                    .clicked()
                    {
                        self.stream_windows_tab = true;
                        if self.stream_window.is_none() {
                            self.stream_window = self.windows.first().map(|w| w.0);
                        }
                    }
                });
                ui.add_space(8.0);
                if self.stream_windows_tab {
                    if self.windows.is_empty() {
                        ui.label(
                            RichText::new("Нет открытых окон, которые можно показать.")
                                .color(pal.muted),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (id, title, app) in &self.windows {
                                let selected = self.stream_window == Some(*id);
                                let (rect, resp) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 46.0),
                                    Sense::click(),
                                );
                                let hov =
                                    ui.ctx()
                                        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                                let fill = if selected {
                                    pal.accent.gamma_multiply(0.22)
                                } else {
                                    mix(pal.elevated, pal.hover, hov)
                                };
                                ui.painter().rect_filled(rect, 10.0, fill);
                                if selected {
                                    ui.painter().rect_stroke(
                                        rect,
                                        10.0,
                                        Stroke::new(1.5, pal.accent),
                                    );
                                }
                                let clip = rect.shrink2(vec2(14.0, 0.0));
                                ui.painter_at(clip).text(
                                    rect.left_center() + vec2(14.0, -8.0),
                                    Align2::LEFT_CENTER,
                                    title,
                                    FontId::proportional(14.0),
                                    pal.text,
                                );
                                ui.painter_at(clip).text(
                                    rect.left_center() + vec2(14.0, 10.0),
                                    Align2::LEFT_CENTER,
                                    app,
                                    FontId::proportional(11.5),
                                    pal.muted,
                                );
                                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                    self.stream_window = Some(*id);
                                }
                                ui.add_space(4.0);
                            }
                        });
                } else {
                    for (i, (name, w, h)) in self.monitors.iter().enumerate() {
                        let selected = self.settings.stream_monitor == i;
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::click());
                        let hov = ui
                            .ctx()
                            .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                        let fill = if selected {
                            pal.accent.gamma_multiply(0.22)
                        } else {
                            mix(pal.elevated, pal.hover, hov)
                        };
                        ui.painter().rect_filled(rect, 12.0, fill);
                        if selected {
                            ui.painter()
                                .rect_stroke(rect, 12.0, Stroke::new(1.5, pal.accent));
                        }
                        draw_icon(
                            ui.painter(),
                            Icon::Screen,
                            rect.left_center() + vec2(28.0, 0.0),
                            24.0,
                            if selected { pal.accent } else { pal.muted },
                            false,
                        );
                        ui.painter().text(
                            rect.left_center() + vec2(56.0, -8.0),
                            Align2::LEFT_CENTER,
                            name,
                            FontId::proportional(14.5),
                            pal.text,
                        );
                        ui.painter().text(
                            rect.left_center() + vec2(56.0, 10.0),
                            Align2::LEFT_CENTER,
                            format!("{w} × {h}"),
                            FontId::proportional(12.0),
                            pal.muted,
                        );
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            self.settings.stream_monitor = i;
                        }
                        ui.add_space(6.0);
                    }
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("КАЧЕСТВО")
                        .size(11.5)
                        .strong()
                        .color(pal.muted),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for (h, label) in [(480u32, "480p"), (720, "720p"), (1080, "1080p")] {
                        let on = self.settings.stream_height == h;
                        if toggle_chip(ui, &pal, label, on).clicked() {
                            self.settings.stream_height = h;
                        }
                    }
                    ui.add_space(12.0);
                    for fps in [15u32, 30, 60] {
                        let on = self.settings.stream_fps == fps;
                        if toggle_chip(ui, &pal, &format!("{fps} к/с"), on).clicked() {
                            self.settings.stream_fps = fps;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    for (q, label) in [(0u8, "Экономно"), (1, "Баланс"), (2, "Чётко")]
                    {
                        if toggle_chip(ui, &pal, label, self.settings.stream_quality == q).clicked()
                        {
                            self.settings.stream_quality = q;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Кодирование идёт на процессоре. Если компьютер слабый или интернет \
                         медленный — выберите 720p и 30 к/с.",
                    )
                    .size(12.0)
                    .color(pal.faint),
                );
                ui.add_space(12.0);
                ui.vertical_centered_justified(|ui| {
                    if ui
                        .add_sized(
                            [ui.available_width(), 40.0],
                            egui::Button::new(
                                RichText::new("Начать трансляцию")
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(pal.accent)
                            .rounding(20.0),
                        )
                        .clicked()
                    {
                        start = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        if start {
            self.handle_action(ctx, Action::StartStream);
        } else if !open {
            self.stream_picker = false;
        }
    }

    fn call_ui(&mut self, ctx: &egui::Context) {
        let Some(cid) = self.incoming_call else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let name = self
            .dms
            .iter()
            .find(|d| d.id == cid)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Кто-то".into());
        let mut action: Option<Action> = None;
        let pulse = (ctx.input(|i| i.time) * 3.0).sin() as f32 * 0.5 + 0.5;
        ctx.request_repaint_after(Duration::from_millis(33));
        egui::Area::new(egui::Id::new("incoming_call"))
            .anchor(Align2::CENTER_TOP, [0.0, 24.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(solid)
                    .rounding(pal.radius + 6.0)
                    .stroke(Stroke::new(1.5, mix(pal.border, pal.ok, pulse)))
                    .inner_margin(Margin::same(16.0))
                    .show(ui, |ui| {
                        ui.set_width(320.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("📞 Входящий звонок").color(pal.muted));
                            ui.label(RichText::new(&name).size(20.0).strong().color(pal.text));
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.add_space(40.0);
                                if ui
                                    .add_sized(
                                        [110.0, 36.0],
                                        egui::Button::new(
                                            RichText::new("Принять").strong().color(Color32::WHITE),
                                        )
                                        .fill(pal.ok.gamma_multiply(0.8))
                                        .rounding(18.0),
                                    )
                                    .clicked()
                                {
                                    action = Some(Action::AcceptCall(cid));
                                }
                                if ui
                                    .add_sized(
                                        [110.0, 36.0],
                                        egui::Button::new(
                                            RichText::new("Отклонить")
                                                .strong()
                                                .color(Color32::WHITE),
                                        )
                                        .fill(pal.danger)
                                        .rounding(18.0),
                                    )
                                    .clicked()
                                {
                                    action = Some(Action::DeclineCall(cid));
                                }
                            });
                        });
                    });
            });
        if let Some(a) = action {
            self.handle_action(ctx, a);
        }
    }

    fn delete_ui(&mut self, ctx: &egui::Context) {
        let Some((cid, mid, preview)) = self.pending_delete.clone() else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let mut decision: Option<bool> = None;
        egui::Window::new(RichText::new("Удалить сообщение?").strong())
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(solid)
                    .inner_margin(Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("Сообщение будет удалено у всех.").color(pal.muted));
                if !preview.is_empty() {
                    egui::Frame::none()
                        .fill(pal.elevated)
                        .rounding(8.0)
                        .inner_margin(Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&preview).color(pal.text));
                        });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Удалить").strong().color(Color32::WHITE),
                            )
                            .fill(pal.danger),
                        )
                        .clicked()
                    {
                        decision = Some(true);
                    }
                    if ui.button("Отмена").clicked() {
                        decision = Some(false);
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }
        match decision {
            Some(true) => {
                self.pending_delete = None;
                let _ = self.cmd.send(Cmd::Delete {
                    channel_id: cid,
                    message_id: mid,
                });
            }
            Some(false) => self.pending_delete = None,
            None => {}
        }
    }

    // ======================= настройки =======================

    fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let pal = self.pal;
        let before = self.settings.clone();
        let mut open = true;
        let mut clear_media = false;
        let mut logout = false;
        let mut apply_zoom = false;
        let (images, frames) = self.media.stats();
        if self.devices.is_none() {
            self.devices = Some(audio::list_devices());
        }
        let testing = self.mic_test.is_some();
        let test_level = match &self.mic_test {
            Some(a) => a.level(),
            None => self.mic_level,
        };
        if testing || self.voice.is_some() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        let mut refresh_devices = false;
        let mut toggle_test = false;
        let solid = self.solid();

        egui::Window::new(RichText::new("Настройки").strong())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(solid)
                    .inner_margin(Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(560.0)
                    .show(ui, |ui| {
                        ui.set_width(410.0);
                        section(ui, &pal, "Внешний вид");
                        for (group, themes) in theme::THEME_GROUPS {
                            ui.add_space(4.0);
                            ui.label(RichText::new(group).size(12.5).color(pal.muted));
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                                for &(theme, label) in themes {
                                    let selected = self.settings.theme == theme;
                                    if theme_card(ui, &pal, theme, label, selected).clicked() {
                                        self.settings.theme = theme;
                                    }
                                }
                            });
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new("Акцентный цвет").color(pal.muted));
                        ui.horizontal_wrapped(|ui| {
                            for (i, (name, color)) in ACCENTS.iter().enumerate() {
                                let (rect, resp) =
                                    ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
                                let r = if resp.hovered() { 14.0 } else { 12.0 };
                                ui.painter().circle_filled(rect.center(), r, *color);
                                if self.settings.accent == i {
                                    ui.painter().circle_stroke(
                                        rect.center(),
                                        15.0,
                                        Stroke::new(2.0, pal.text),
                                    );
                                }
                                if resp.on_hover_text(*name).clicked() {
                                    self.settings.accent = i;
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Шрифт:").color(pal.muted));
                            if toggle_chip(ui, &pal, "Inter", self.settings.font_inter).clicked() {
                                self.settings.font_inter = true;
                            }
                            if toggle_chip(ui, &pal, "Системный (Segoe UI)", !self.settings.font_inter).clicked() {
                                self.settings.font_inter = false;
                            }
                        });
                        let zoom = ui.add(
                            egui::Slider::new(&mut self.pending_zoom, 0.75..=1.6)
                                .step_by(0.05)
                                .text("Масштаб интерфейса"),
                        );
                        if zoom.drag_stopped() || (zoom.changed() && !zoom.dragged()) {
                            apply_zoom = true;
                        }
                        ui.checkbox(
                            &mut self.settings.compact,
                            "Компактные сообщения (без аватарок, плотнее)",
                        );
                        if theme::backdrop(self.settings.theme).is_some() {
                            ui.checkbox(
                                &mut self.settings.animate_background,
                                "Анимировать фон (чуть больше нагрузка на видеокарту)",
                            );
                        }

                        section(ui, &pal, "Картинки и медиа");
                        ui.checkbox(
                            &mut self.settings.load_images,
                            "Показывать картинки и превью",
                        );
                        ui.checkbox(&mut self.settings.animate_gifs, "Анимировать GIF");
                        ui.checkbox(
                            &mut self.settings.load_avatars,
                            "Показывать аватарки и иконки серверов",
                        );
                        ui.label(
                            RichText::new(
                                "Отключите, чтобы клиент ел ещё меньше памяти и трафика.",
                            )
                            .small()
                            .color(pal.faint),
                        );

                        section(ui, &pal, "Голос");
                        let (ins, outs) = self.devices.clone().unwrap_or_default();
                        let default_name = "По умолчанию (системное)".to_string();
                        ui.label(RichText::new("Микрофон").color(pal.muted));
                        #[allow(deprecated)]
                        egui::ComboBox::from_id_source("input_device")
                            .width(390.0)
                            .selected_text(
                                self.settings
                                    .input_device
                                    .clone()
                                    .unwrap_or(default_name.clone()),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.settings.input_device,
                                    None,
                                    default_name.as_str(),
                                );
                                for n in &ins {
                                    ui.selectable_value(
                                        &mut self.settings.input_device,
                                        Some(n.clone()),
                                        n.as_str(),
                                    );
                                }
                            });
                        ui.label(RichText::new("Наушники / динамики").color(pal.muted));
                        #[allow(deprecated)]
                        egui::ComboBox::from_id_source("output_device")
                            .width(390.0)
                            .selected_text(
                                self.settings
                                    .output_device
                                    .clone()
                                    .unwrap_or(default_name.clone()),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.settings.output_device,
                                    None,
                                    default_name.as_str(),
                                );
                                for n in &outs {
                                    ui.selectable_value(
                                        &mut self.settings.output_device,
                                        Some(n.clone()),
                                        n.as_str(),
                                    );
                                }
                            });
                        if ui.small_button("Обновить список устройств").clicked()
                        {
                            refresh_devices = true;
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new("Громкость микрофона").color(pal.muted));
                        level_meter(ui, &pal, test_level, self.settings.mic_threshold, 10.0);
                        ui.add(
                            egui::Slider::new(&mut self.settings.mic_threshold, 0.001..=0.06)
                                .logarithmic(true)
                                .show_value(false)
                                .text("порог чувствительности"),
                        );
                        ui.label(
                            RichText::new(
                                "Когда вы говорите, полоска должна заходить за белую отметку, \
                                 а в тишине оставаться левее неё.",
                            )
                            .small()
                            .color(pal.faint),
                        );
                        ui.add(
                            egui::Slider::new(&mut self.settings.input_gain, 0.0..=3.0)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                .text("усиление микрофона"),
                        );
                        ui.add(
                            egui::Slider::new(&mut self.settings.output_volume, 0.0..=2.0)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                .text("громкость собеседников"),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new("Режим передачи").color(pal.muted));
                        ui.horizontal(|ui| {
                            let vad = !self.settings.ptt && !self.settings.always_transmit;
                            if toggle_chip(ui, &pal, "🎙 По голосу", vad).clicked() {
                                self.settings.ptt = false;
                                self.settings.always_transmit = false;
                            }
                            if toggle_chip(ui, &pal, "📻 Рация", self.settings.ptt).clicked() {
                                self.settings.ptt = true;
                                self.settings.always_transmit = false;
                            }
                            if toggle_chip(ui, &pal, "Всегда", self.settings.always_transmit && !self.settings.ptt).clicked() {
                                self.settings.ptt = false;
                                self.settings.always_transmit = true;
                            }
                        });
                        if self.settings.ptt {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Клавиша рации:").color(pal.muted));
                                let current = PTT_KEYS
                                    .iter()
                                    .find(|(_, vk)| *vk == self.settings.ptt_key)
                                    .map(|(n, _)| *n)
                                    .unwrap_or("?");
                                #[allow(deprecated)]
                                egui::ComboBox::from_id_source("ptt_key")
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        for (n, vk) in PTT_KEYS {
                                            ui.selectable_value(&mut self.settings.ptt_key, vk, n);
                                        }
                                    });
                                let down = voice::PTT_DOWN.load(std::sync::atomic::Ordering::Relaxed);
                                chip(ui, &pal, if down { "● передаю" } else { "○ отпущена" }, if down { pal.ok } else { pal.faint });
                            });
                            ui.label(
                                RichText::new(if cfg!(windows) {
                                    "Рация работает и когда окно Rustcord свёрнуто."
                                } else {
                                    "Рация работает, пока окно Rustcord в фокусе."
                                })
                                .small()
                                .color(pal.faint),
                            );
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new("Обработка звука").color(pal.muted));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Шумоподавление:").color(pal.text));
                            for (n, label) in [(0u8, "Выкл"), (1, "Стандартное"), (2, "Усиленное (RNNoise)")] {
                                if toggle_chip(ui, &pal, label, self.settings.noise == n).clicked() {
                                    self.settings.noise = n;
                                }
                            }
                        });
                        ui.checkbox(
                            &mut self.settings.echo_cancel,
                            "Эхоподавление (WebRTC AEC3) — если вы без наушников",
                        );
                        ui.checkbox(
                            &mut self.settings.agc,
                            "Автоматическая регулировка громкости микрофона",
                        );
                        if !self.settings.echo_cancel {
                            ui.checkbox(
                                &mut self.settings.echo_guard,
                                "Простое подавление эха (не передавать тихий звук, пока говорят другие)",
                            );
                        }
                        ui.label(
                            RichText::new(
                                "Krisp — закрытая технология Discord, сторонним клиентам она недоступна. \
                                 Вместо неё — нейросетевое шумоподавление RNNoise и обработка из WebRTC, \
                                 всё работает прямо на компьютере.",
                            )
                            .small()
                            .color(pal.faint),
                        );
                        if ui.button("🔊 Проверить динамики").clicked() {
                            if let Err(e) = audio::play_test_sound(self.settings.output_device.clone()) {
                                self.toast(format!("Не удалось проиграть звук: {e}"), true);
                            }
                        }
                        let label = if testing {
                            "■ Остановить проверку"
                        } else {
                            "▶ Проверить микрофон (вы услышите себя)"
                        };
                        if ui.button(label).clicked() {
                            toggle_test = true;
                        }

                        section(ui, &pal, "Память");
                        ui.add(
                            egui::Slider::new(&mut self.settings.cached_channels, 2..=20)
                                .text("каналов держать в памяти"),
                        );
                        ui.add(
                            egui::Slider::new(&mut self.settings.messages_per_channel, 50..=500)
                                .step_by(50.0)
                                .text("сообщений на канал"),
                        );
                        ui.checkbox(
                            &mut self.settings.show_ram,
                            "Показывать потребление ОЗУ в заголовке",
                        );
                        section(ui, &pal, "Уведомления");
                        ui.checkbox(
                            &mut self.settings.notifications,
                            "Уведомления на рабочем столе о личных сообщениях и упоминаниях",
                        );
                        ui.label(
                            RichText::new(
                                "В статусе «Не беспокоить» уведомления не показываются. \
                                 Горячие клавиши: Ctrl+K — быстрый переход, Ctrl+F — поиск.",
                            )
                            .small()
                            .color(pal.faint),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Сейчас: ОЗУ {:.0} МБ · картинок {images} (кадров {frames})",
                                self.mem_mb
                            ))
                            .color(pal.muted),
                        );
                        if ui.button("Очистить кэш картинок").clicked() {
                            clear_media = true;
                        }

                        section(ui, &pal, "Аккаунт");
                        ui.label(RichText::new(&self.me_name).strong().color(pal.text));
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Выйти из аккаунта").color(Color32::WHITE),
                                )
                                .fill(pal.danger),
                            )
                            .clicked()
                        {
                            logout = true;
                        }

                        section(ui, &pal, "О программе");
                        ui.label(
                            RichText::new(format!("Rustcord {}", env!("CARGO_PKG_VERSION")))
                                .color(pal.muted),
                        );
                        ui.label(
                            RichText::new(format!("Лог: {}", store::log_path_display()))
                                .small()
                                .color(pal.faint),
                        );
                        ui.label(
                            RichText::new("Цветные эмодзи: Twemoji (CC-BY 4.0)")
                                .small()
                                .color(pal.faint),
                        );
                    });
            });

        self.settings_open = open;
        if refresh_devices {
            self.devices = None;
        }
        let devices_changed = before.input_device != self.settings.input_device
            || before.output_device != self.settings.output_device;
        if toggle_test || (testing && devices_changed) || !open {
            let was = self.mic_test.take().is_some();
            if open && (!was || devices_changed) {
                match audio::Audio::start(
                    self.settings.input_device.clone(),
                    self.settings.output_device.clone(),
                    true,
                ) {
                    Ok(a) => self.mic_test = Some(a),
                    Err(e) => self.toast(format!("Проверка микрофона: {e}"), true),
                }
            }
        }
        if apply_zoom {
            self.settings.zoom = self.pending_zoom;
            ctx.set_zoom_factor(self.settings.zoom);
        }
        if before != self.settings {
            self.apply_settings(ctx, &before);
        }
        if clear_media {
            self.media.clear();
        }
        if logout {
            self.logout(None, true);
        }
    }

    fn toast_ui(&mut self, ctx: &egui::Context) {
        let expired = self
            .toast
            .as_ref()
            .map_or(false, |(_, at, _)| at.elapsed() > Duration::from_secs(4));
        if expired {
            self.toast = None;
        }
        let pal = self.pal;
        let solid = self.solid();
        let slide = self.toast.as_ref().map_or(0.0, |(_, at, _)| {
            let k = (at.elapsed().as_secs_f32() / 0.22).min(1.0);
            let ease = 1.0 - (1.0 - k).powi(3);
            (1.0 - ease) * 30.0
        });
        if slide > 0.0 {
            ctx.request_repaint();
        }
        if let Some((text, _, error)) = &self.toast {
            egui::Area::new(egui::Id::new("toast"))
                .anchor(Align2::CENTER_BOTTOM, [0.0, -96.0 + slide])
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(solid)
                        .rounding(10.0)
                        .stroke(Stroke::new(
                            1.0,
                            if *error { pal.danger } else { pal.border },
                        ))
                        .inner_margin(Margin::symmetric(14.0, 10.0))
                        .show(ui, |ui| {
                            ui.set_max_width(460.0);
                            ui.label(RichText::new(text.as_str()).color(pal.text));
                        });
                });
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        // Вне Windows клавишу рации слушаем, пока окно в фокусе.
        #[cfg(not(windows))]
        if self.settings.ptt {
            if let Some(k) = ptt_egui_key(self.settings.ptt_key) {
                let down = ctx.input(|i| i.key_down(k));
                voice::PTT_DOWN.store(down, std::sync::atomic::Ordering::Relaxed);
            }
        }
        match theme::backdrop(self.settings.theme) {
            Some(bd) => paint_backdrop(ctx, &bd, self.settings.animate_background),
            None => {
                // спокойный фон с лёгким акцентным свечением в углу
                let base = if self.pal.light {
                    mix(self.pal.rail, Color32::BLACK, 0.05)
                } else {
                    mix(self.pal.rail, Color32::BLACK, 0.25)
                };
                let bd = theme::Backdrop {
                    base,
                    glows: [
                        self.pal.accent.gamma_multiply(0.18),
                        self.pal.link.gamma_multiply(0.10),
                        Color32::TRANSPARENT,
                    ],
                    stars: false,
                };
                let animate = self.screen == Screen::Login && self.settings.animate_background;
                paint_backdrop(ctx, &bd, animate);
            }
        }
        self.refresh_memory(ctx);
        match self.screen {
            Screen::Login => self.login_ui(ctx),
            Screen::Main => self.main_ui(ctx),
        }
        self.call_ui(ctx);
        self.extra_ui(ctx);
        self.stream_picker_ui(ctx);
        self.stream_fullscreen_ui(ctx);
        self.picker_ui(ctx);
        self.delete_ui(ctx);
        self.settings_ui(ctx);
        self.toast_ui(ctx);
        self.media.evict(ctx.input(|i| i.time));
    }
}

// ======================= сообщения =======================

struct MsgCtx<'a> {
    pal: Palette,
    settings: &'a Settings,
    media: &'a mut MediaCache,
    cmd: &'a UnboundedSender<Cmd>,
    actions: &'a mut Vec<Action>,
    me_id: u64,
    guild_id: Option<u64>,
    channel_id: u64,
    toolbar_allowed: bool,
    guilds: &'a [Guild],
    users: &'a HashMap<u64, String>,
}

impl MsgCtx<'_> {
    fn image(
        &mut self,
        ctx: &egui::Context,
        url: &str,
        max_side: u32,
        animate: bool,
    ) -> (String, Option<(TextureId, Vec2)>) {
        let key = format!("{max_side}|{animate}|{url}");
        let tex = self.media.get(ctx, self.cmd, &key, url, max_side, animate);
        (key, tex)
    }

    fn message_link(&self, message_id: u64) -> String {
        let guild = self
            .guild_id
            .map(|g| g.to_string())
            .unwrap_or_else(|| "@me".into());
        format!(
            "https://discord.com/channels/{guild}/{}/{message_id}",
            self.channel_id
        )
    }
}

/// Цветной эмодзи (картинка), с запасным вариантом — символом шрифта.
fn emoji_image(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, emoji: &Emoji, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let animate = emoji.animated && cx.settings.animate_gifs;
        let (key, tex) = cx.image(ui.ctx(), &emoji.image_url(), 72, animate);
        match tex {
            Some((id, _)) => {
                egui::Image::from_texture(SizedTexture::new(id, rect.size())).paint_at(ui, rect);
            }
            None if cx.media.failed(&key) || emoji.id.is_some() => {
                let label = if emoji.id.is_some() {
                    format!(":{}:", emoji.name)
                } else {
                    emoji.name.clone()
                };
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(size * 0.7),
                    cx.pal.text,
                );
            }
            None => {}
        }
    }
    if emoji.id.is_some() {
        resp.on_hover_text(format!(":{}:", emoji.name))
    } else {
        resp
    }
}

fn tool_button(ui: &mut egui::Ui, pal: &Palette, glyph: &str, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 28.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.hover);
    }
    let color = if resp.hovered() { pal.text } else { pal.muted };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(16.0),
        color,
    );
    resp.on_hover_text(tip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

fn message_ui(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, m: &Msg, grouped: bool) {
    let pal = cx.pal;
    let compact = cx.settings.compact;
    ui.add_space(if grouped {
        0.0
    } else if compact {
        4.0
    } else {
        10.0
    });

    let bg = ui.painter().add(egui::Shape::Noop);
    let full_width = ui.available_width();
    // Плавное появление только что пришедших сообщений (история грузится без анимации).
    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    let fresh = m.sending || now_ms.saturating_sub(snowflake_ms(m.id)) < 8_000;
    let appear = if fresh {
        let id = egui::Id::new((
            "appear",
            m.channel_id,
            m.nonce.as_deref().unwrap_or(""),
            m.id,
        ));
        let now = ui.input(|i| i.time);
        let key = m
            .nonce
            .as_deref()
            .map(|n| egui::Id::new(("appear-n", n)))
            .unwrap_or(id);
        let t0 = ui
            .ctx()
            .memory_mut(|mem| *mem.data.get_temp_mut_or_insert_with(key, || now));
        let a = ((now - t0) / 0.22).clamp(0.0, 1.0) as f32;
        if a < 1.0 {
            ui.ctx().request_repaint();
        }
        a
    } else {
        1.0
    };
    let opacity = if m.sending { 0.55 } else { 1.0 } * (0.25 + 0.75 * appear);
    let slide = (1.0 - appear) * 6.0;
    if slide > 0.0 {
        ui.add_space(slide);
    }
    let row = ui.horizontal_top(|ui| {
        ui.multiply_opacity(opacity);
        ui.add_space(16.0);
        let gutter = if compact { 0.0 } else { 40.0 };
        if gutter > 0.0 {
            if grouped {
                let (rect, _) = ui.allocate_exact_size(vec2(gutter, 18.0), Sense::hover());
                if ui.rect_contains_pointer(rect.expand2(vec2(full_width, 0.0))) {
                    let t = m.time.get(11..).unwrap_or("");
                    ui.painter().text(
                        rect.right_center(),
                        Align2::RIGHT_CENTER,
                        t,
                        FontId::proportional(10.5),
                        pal.faint,
                    );
                }
            } else {
                let (rect, aresp) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
                let tex = match (
                    &m.author_avatar,
                    cx.settings.load_avatars && ui.is_rect_visible(rect),
                ) {
                    (Some(h), true) => {
                        let url = discord::avatar_url(m.author_id, h);
                        cx.media.get(ui.ctx(), cx.cmd, &url, &url, 64, false)
                    }
                    _ => None,
                };
                paint_avatar(ui, rect, tex, m.author_id, &m.author);
                if aresp.secondary_clicked() {
                    if let Some(p) = aresp.interact_pointer_pos() {
                        cx.actions.push(Action::UserMenu(m.author_id, p));
                    }
                }
                if aresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    cx.actions.push(Action::OpenProfile(m.author_id));
                }
            }
        }
        ui.vertical(|ui| {
            ui.set_max_width((full_width - gutter - 56.0).max(120.0));
            ui.spacing_mut().item_spacing.y = 3.0;
            if let Some(r) = &m.reply {
                reply_preview(ui, &pal, r, gutter);
            }
            if !grouped {
                ui.horizontal(|ui| {
                    let nr = ui.add(
                        egui::Label::new(
                            RichText::new(m.author.as_str())
                                .font(FontId::new(15.0, egui::FontFamily::Name("bold".into())))
                                .color(name_color(m.author_id)),
                        )
                        .sense(Sense::click()),
                    );
                    if nr.secondary_clicked() {
                        if let Some(p) = nr.interact_pointer_pos() {
                            cx.actions.push(Action::UserMenu(m.author_id, p));
                        }
                    }
                    if nr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        cx.actions.push(Action::OpenProfile(m.author_id));
                    }
                    ui.label(
                        RichText::new(pretty_time(&m.time))
                            .size(11.5)
                            .color(pal.faint),
                    )
                    .on_hover_text(m.time.as_str());
                    if m.pinned {
                        ui.label(RichText::new("📌").small().color(pal.faint))
                            .on_hover_text("Закреплено");
                    }
                });
            }
            let hide_text = m.embeds.iter().any(|e| {
                (e.kind == "gifv" || e.kind == "image")
                    && e.url.as_deref() == Some(m.content.trim())
            });
            if m.system {
                ui.label(
                    RichText::new(format!("→ {} {}", m.author, m.content))
                        .italics()
                        .color(pal.muted),
                );
            } else if !m.content.is_empty() && !hide_text {
                text_block(ui, cx, &m.content, &m.mentions);
            }
            if m.edited {
                ui.label(RichText::new("(изменено)").small().color(pal.faint));
            }
            if m.sending {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(11.0).color(pal.faint));
                    ui.label(RichText::new("отправляется…").small().color(pal.faint));
                });
            }
            if let Some(err) = &m.failed {
                egui::Frame::none()
                    .fill(pal.danger.gamma_multiply(0.12))
                    .stroke(Stroke::new(1.0, pal.danger.gamma_multiply(0.5)))
                    .rounding(8.0)
                    .inner_margin(Margin::symmetric(10.0, 6.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("⚠ Не отправлено: {err}"))
                                .size(13.0)
                                .color(pal.danger),
                        );
                        ui.horizontal(|ui| {
                            let nonce = m.nonce.clone().unwrap_or_default();
                            if toggle_chip(ui, &pal, "↻ Повторить", true).clicked() {
                                cx.actions
                                    .push(Action::RetrySend(m.channel_id, nonce.clone()));
                            }
                            if toggle_chip(ui, &pal, "Убрать", false).clicked() {
                                cx.actions.push(Action::DismissFailed(m.channel_id, nonce));
                            }
                            if toggle_chip(ui, &pal, "Копировать текст", false).clicked()
                            {
                                cx.actions.push(Action::CopyText(m.content.clone()));
                            }
                        });
                    });
            }
            if m.content.is_empty() && m.attachments.is_empty() && m.embeds.is_empty() && !m.system
            {
                ui.label(
                    RichText::new("[этот тип сообщения пока не поддерживается]")
                        .italics()
                        .color(pal.faint),
                );
            }
            for a in &m.attachments {
                attachment_ui(ui, cx, a);
            }
            for e in &m.embeds {
                embed_ui(ui, cx, e);
            }
            if !m.reactions.is_empty() {
                reactions_ui(ui, cx, m);
            }
            if let Some((tid, tname, count)) = &m.thread {
                let r = egui::Frame::none()
                    .fill(pal.elevated)
                    .rounding(8.0)
                    .stroke(Stroke::new(1.0, pal.border))
                    .inner_margin(Margin::symmetric(10.0, 6.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("🧵").color(pal.muted));
                            ui.label(RichText::new(tname.as_str()).strong().color(pal.text));
                            if *count > 0 {
                                ui.label(
                                    RichText::new(format!("· {count} сообщ."))
                                        .small()
                                        .color(pal.link),
                                );
                            }
                        });
                    })
                    .response
                    .interact(Sense::click());
                if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    cx.actions.push(Action::OpenThread(
                        cx.guild_id,
                        Channel {
                            id: *tid,
                            name: tname.clone(),
                            kind: 11,
                            position: 0,
                            parent: Some(m.channel_id),
                        },
                    ));
                }
            }
        });
    });

    let rect = Rect::from_min_max(
        pos2(ui.max_rect().left(), row.response.rect.top() - 2.0),
        pos2(ui.max_rect().right(), row.response.rect.bottom() + 2.0),
    );
    let hovered = ui.rect_contains_pointer(rect.expand2(vec2(0.0, 6.0)));
    let hover_t = ui.ctx().animate_bool_with_time(
        egui::Id::new(("msg-hover", m.channel_id, m.id)),
        hovered,
        0.10,
    );
    let mentioned = m.author_id != cx.me_id
        && (m.mention_everyone || m.mentions.iter().any(|(id, _)| *id == cx.me_id));
    let card = rect.shrink2(vec2(6.0, 0.0));
    if mentioned {
        let tint = 0.10 + 0.06 * hover_t;
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(card, 6.0, pal.warn.gamma_multiply(tint)),
        );
        ui.painter().rect_filled(
            Rect::from_min_size(card.left_top(), vec2(3.0, card.height())),
            Rounding {
                nw: 6.0,
                sw: 6.0,
                ne: 0.0,
                se: 0.0,
            },
            pal.warn,
        );
    } else if m.failed.is_some() {
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(card, 6.0, pal.danger.gamma_multiply(0.05)),
        );
    } else if hover_t > 0.01 {
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(card, 6.0, pal.msg_hover.gamma_multiply(hover_t)),
        );
    }
    if m.sending || m.failed.is_some() {
        return;
    }
    if hovered && !m.system && ui.input(|i| i.pointer.secondary_clicked()) {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            cx.actions
                .push(Action::MessageMenu(m.channel_id, Box::new(m.clone()), pos));
        }
    }
    if hovered && cx.toolbar_allowed && !m.system {
        message_toolbar(ui, cx, m, rect);
    }
}

/// Ответ на сообщение: «изгиб» от аватарки, мини-аватар, имя и начало текста — как в Discord.
fn reply_preview(ui: &mut egui::Ui, pal: &Palette, r: &discord::Reply, gutter: f32) {
    let h = 18.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    if gutter > 0.0 {
        // линия-изгиб уходит в колонку аватарки
        let x0 = rect.left() - gutter / 2.0 - 4.0;
        let y0 = rect.bottom() + 4.0;
        let x1 = rect.left() - 4.0;
        let y1 = rect.center().y;
        let stroke = Stroke::new(2.0, pal.faint.gamma_multiply(0.7));
        let r6 = 6.0;
        ui.painter()
            .line_segment([pos2(x0, y0), pos2(x0, y1 + r6)], stroke);
        ui.painter().add(egui::Shape::line(
            arc(
                pos2(x0 + r6, y1 + r6),
                r6,
                std::f32::consts::PI,
                1.5 * std::f32::consts::PI,
            ),
            stroke,
        ));
        ui.painter()
            .line_segment([pos2(x0 + r6, y1), pos2(x1, y1)], stroke);
    }
    let mut x = rect.left();
    let cy = rect.center().y;
    let id = r
        .author
        .bytes()
        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
    ui.painter()
        .circle_filled(pos2(x + 8.0, cy), 8.0, name_color(id));
    ui.painter().text(
        pos2(x + 8.0, cy),
        Align2::CENTER_CENTER,
        initials(&r.author)
            .chars()
            .next()
            .unwrap_or('?')
            .to_string(),
        FontId::proportional(9.0),
        Color32::WHITE,
    );
    x += 21.0;
    let name = ui.painter().text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        if r.author.is_empty() {
            "Сообщение"
        } else {
            r.author.as_str()
        },
        FontId::new(13.0, egui::FontFamily::Name("bold".into())),
        pal.muted,
    );
    x = name.right() + 6.0;
    let clip = Rect::from_min_max(pos2(x, rect.top()), rect.max);
    ui.painter_at(clip).text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        if r.content.is_empty() {
            "…"
        } else {
            r.content.as_str()
        },
        FontId::proportional(13.0),
        pal.faint,
    );
}

/// «Сегодня в 08:19», «Вчера в 21:03» или полная дата для старых сообщений.
fn pretty_time(time: &str) -> String {
    // time: «dd.mm.yyyy HH:MM»
    let (date, clock) = match time.split_once(' ') {
        Some(x) => x,
        None => return time.to_string(),
    };
    let today = chrono::Local::now();
    let fmt = |d: chrono::DateTime<chrono::Local>| d.format("%d.%m.%Y").to_string();
    if date == fmt(today) {
        format!("Сегодня в {clock}")
    } else if date == fmt(today - chrono::Duration::days(1)) {
        format!("Вчера в {clock}")
    } else {
        format!("{date} {clock}")
    }
}

/// Панель действий над сообщением при наведении.
fn message_toolbar(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, m: &Msg, rect: Rect) {
    let pal = cx.pal;
    let own = m.author_id == cx.me_id;
    let buttons = if own { 5.0 } else { 4.0 };
    let w = buttons * 30.0 + 6.0;
    let tb = Rect::from_min_size(
        pos2(rect.right() - w - 20.0, rect.top() - 6.0),
        vec2(w, 34.0),
    );
    #[allow(deprecated)]
    let mut tui = ui.child_ui(tb, Layout::left_to_right(Align::Center), None);
    egui::Frame::none()
        .fill(pal.side)
        .rounding(8.0)
        .stroke(Stroke::new(1.0, pal.border))
        .inner_margin(Margin::same(2.0))
        .show(&mut tui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let r = tool_button(ui, &pal, "😊", "Добавить реакцию");
            if r.clicked() {
                cx.actions.push(Action::OpenPicker(
                    PickerTarget::React {
                        channel_id: cx.channel_id,
                        message_id: m.id,
                    },
                    r.rect.left_bottom(),
                ));
            }
            if tool_button(ui, &pal, "↩", "Ответить").clicked() {
                cx.actions.push(Action::Reply(m.id, m.author.clone()));
            }
            if tool_button(ui, &pal, "⧉", "Копировать текст").clicked() {
                cx.actions.push(Action::CopyText(m.content.clone()));
            }
            if own {
                if tool_button(ui, &pal, "✏", "Изменить").clicked() {
                    cx.actions
                        .push(Action::StartEdit(cx.channel_id, m.id, m.content.clone()));
                }
                if tool_button(ui, &pal, "🗑", "Удалить").clicked() {
                    let preview: String = m.content.chars().take(160).collect();
                    cx.actions
                        .push(Action::AskDelete(cx.channel_id, m.id, preview));
                }
            } else if tool_button(ui, &pal, "⚑", "Пожаловаться").clicked() {
                cx.actions.push(Action::Report(cx.message_link(m.id)));
            }
        });
}

fn reactions_ui(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, m: &Msg) {
    let pal = cx.pal;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for r in &m.reactions {
            let (fill, stroke) = if r.me {
                (
                    pal.accent.gamma_multiply(0.22),
                    Stroke::new(1.0, pal.accent),
                )
            } else {
                (pal.elevated, Stroke::new(1.0, pal.border))
            };
            let resp = egui::Frame::none()
                .fill(fill)
                .stroke(stroke)
                .rounding(8.0)
                .inner_margin(Margin::symmetric(6.0, 2.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        emoji_image(ui, cx, &r.emoji, 18.0);
                        let color = if r.me { pal.text } else { pal.muted };
                        ui.label(RichText::new(r.count.to_string()).small().color(color));
                    });
                })
                .response
                .interact(Sense::click());
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                cx.actions.push(Action::React {
                    channel_id: cx.channel_id,
                    message_id: m.id,
                    emoji: r.emoji.clone(),
                    add: !r.me,
                });
            }
        }
    });
}

/// Текст сообщения с разметкой Discord (см. `markdown.rs` и `extra::rich_text`).
fn text_block(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, content: &str, mentions: &[(u64, String)]) {
    extra::rich_text(ui, cx, content, mentions);
}

fn fit(w: u32, h: u32, max_w: f32, max_h: f32) -> Vec2 {
    if w == 0 || h == 0 {
        return vec2(max_w.min(260.0), max_h.min(180.0));
    }
    let (w, h) = (w as f32, h as f32);
    let k = (max_w / w).min(max_h / h).min(1.0);
    vec2((w * k).max(24.0), (h * k).max(24.0))
}

/// Картинка с плейсхолдером: грузится, только когда видна.
fn image_box(
    ui: &mut egui::Ui,
    cx: &mut MsgCtx<'_>,
    url: &str,
    known: (u32, u32),
    max: Vec2,
    max_side: u32,
    animate: bool,
    badge: Option<&str>,
    click_url: &str,
) -> bool {
    let pal = cx.pal;
    let key = format!("{max_side}|{animate}|{url}");
    let size = match cx.media.size_of(&key) {
        Some(s) if known.0 == 0 => fit(s.x as u32, s.y as u32, max.x, max.y),
        _ => fit(known.0, known.1, max.x, max.y),
    };
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let mut failed = false;
    if ui.is_rect_visible(rect) {
        let (key, tex) = cx.image(ui.ctx(), url, max_side, animate);
        match tex {
            Some((id, _)) => {
                egui::Image::from_texture(SizedTexture::new(id, rect.size()))
                    .rounding(8.0)
                    .paint_at(ui, rect);
            }
            None => {
                ui.painter().rect_filled(rect, 8.0, pal.elevated);
                if cx.media.failed(&key) {
                    failed = true;
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "не загрузилось",
                        FontId::proportional(13.0),
                        pal.faint,
                    );
                } else {
                    ui.put(
                        Rect::from_center_size(rect.center(), vec2(22.0, 22.0)),
                        egui::Spinner::new(),
                    );
                }
            }
        }
        if let Some(b) = badge {
            let br = Rect::from_min_size(rect.left_top() + vec2(8.0, 8.0), vec2(38.0, 20.0));
            ui.painter()
                .rect_filled(br, 5.0, Color32::from_black_alpha(170));
            ui.painter().text(
                br.center(),
                Align2::CENTER_CENTER,
                b,
                FontId::proportional(11.5),
                Color32::WHITE,
            );
        }
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        cx.actions.push(Action::OpenUrl(click_url.to_string()));
    }
    failed
}

fn sticker_ui(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, a: &Attachment) {
    let pal = cx.pal;
    let (rect, resp) = ui.allocate_exact_size(vec2(160.0, 160.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let animate = a.content_type == "sticker/gif" && cx.settings.animate_gifs;
        let tex = if cx.settings.load_images {
            let (key, tex) = cx.image(ui.ctx(), &a.proxy_url, 160, animate);
            tex.ok_or(cx.media.failed(&key))
        } else {
            Err(true)
        };
        match tex {
            Ok((id, size)) => {
                let fitted = fit(size.x as u32, size.y as u32, 160.0, 160.0);
                let r = Rect::from_min_size(rect.min, fitted);
                egui::Image::from_texture(SizedTexture::new(id, r.size())).paint_at(ui, r);
            }
            Err(true) => {
                let card = rect.shrink(6.0);
                ui.painter()
                    .rect_filled(card, pal.radius + 4.0, pal.elevated);
                ui.painter()
                    .rect_stroke(card, pal.radius + 4.0, Stroke::new(1.0, pal.border));
                ui.painter().text(
                    card.center() - vec2(0.0, 22.0),
                    Align2::CENTER_CENTER,
                    "✨",
                    FontId::proportional(34.0),
                    pal.accent,
                );
                ui.painter_at(card).text(
                    card.center() + vec2(0.0, 14.0),
                    Align2::CENTER_CENTER,
                    &a.filename,
                    FontId::proportional(14.0),
                    pal.text,
                );
                ui.painter().text(
                    card.center() + vec2(0.0, 34.0),
                    Align2::CENTER_CENTER,
                    "стикер",
                    FontId::proportional(11.5),
                    pal.faint,
                );
            }
            Err(false) => {
                ui.painter()
                    .rect_filled(rect.shrink(6.0), pal.radius + 4.0, pal.elevated);
                ui.put(
                    Rect::from_center_size(rect.center(), vec2(22.0, 22.0)),
                    egui::Spinner::new(),
                );
            }
        }
    }
    resp.on_hover_text(format!("Стикер «{}»", a.filename));
}

fn attachment_ui(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, a: &Attachment) {
    let pal = cx.pal;
    if a.content_type.starts_with("sticker/") {
        sticker_ui(ui, cx, a);
        return;
    }
    if a.is_image() && cx.settings.load_images {
        let animate = a.is_gif() && cx.settings.animate_gifs && a.size <= 8 * 1024 * 1024;
        let max = vec2(400.0, 300.0);
        let (url, max_side) = if animate {
            (a.url.clone(), 320)
        } else {
            let s = fit(a.width, a.height, max.x, max.y);
            (
                discord::resized_url(&a.proxy_url, s.x as u32, s.y as u32),
                480,
            )
        };
        let badge = if a.is_gif() { Some("GIF") } else { None };
        image_box(
            ui,
            cx,
            &url,
            (a.width, a.height),
            max,
            max_side,
            animate,
            badge,
            &a.url,
        );
        return;
    }
    egui::Frame::none()
        .fill(pal.elevated)
        .rounding(10.0)
        .stroke(Stroke::new(1.0, pal.border))
        .inner_margin(Margin::same(10.0))
        .show(ui, |ui| {
            ui.set_max_width(440.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(file_icon(a)).size(26.0));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add(
                        egui::Label::new(RichText::new(&a.filename).strong().color(pal.link))
                            .truncate(),
                    );
                    let kind = if a.is_video() { " · видео" } else { "" };
                    ui.label(
                        RichText::new(format!("{}{kind}", human_size(a.size)))
                            .small()
                            .color(pal.muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button("⬇")
                        .on_hover_text("Скачать в «Загрузки»")
                        .clicked()
                    {
                        cx.actions
                            .push(Action::Download(a.url.clone(), a.filename.clone()));
                    }
                    if ui
                        .button("Открыть")
                        .on_hover_text("Открыть в браузере")
                        .clicked()
                    {
                        cx.actions.push(Action::OpenUrl(a.url.clone()));
                    }
                });
            });
        });
}

fn embed_ui(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, e: &Embed) {
    let pal = cx.pal;
    let open_url = e.url.clone().unwrap_or_default();

    if (e.kind == "gifv" || e.kind == "image") && cx.settings.load_images {
        if let Some(media) = &e.media {
            let max = vec2(360.0, 280.0);
            let known = (media.width, media.height);
            let click = if open_url.is_empty() {
                media.url.clone()
            } else {
                open_url.clone()
            };
            let gif = e.gif_url.as_ref().filter(|_| cx.settings.animate_gifs);
            let failed = match gif {
                Some(g) => image_box(ui, cx, g, known, max, 300, true, Some("GIF"), &click),
                None => false,
            };
            if gif.is_none() || failed {
                let s = fit(media.width, media.height, max.x, max.y);
                let url = discord::resized_url(&media.url, s.x as u32, s.y as u32);
                let badge = if e.kind == "gifv" { Some("GIF") } else { None };
                image_box(ui, cx, &url, known, max, 420, false, badge, &click);
            }
            return;
        }
    }

    if e.title.is_none() && e.description.is_none() && e.media.is_none() {
        return;
    }
    let stripe = e
        .color
        .map(|c| Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8))
        .unwrap_or(pal.border);
    let frame = egui::Frame::none()
        .fill(pal.elevated)
        .rounding(8.0)
        .inner_margin(Margin {
            left: 16.0,
            right: 12.0,
            top: 10.0,
            bottom: 10.0,
        })
        .show(ui, |ui| {
            ui.set_max_width(460.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            if let Some(p) = &e.provider {
                ui.label(RichText::new(p).small().color(pal.muted));
            }
            if let Some(t) = &e.title {
                if open_url.is_empty() {
                    ui.label(RichText::new(t).strong().color(pal.text));
                } else {
                    ui.hyperlink_to(RichText::new(t).strong().color(pal.link), &open_url);
                }
            }
            if let Some(d) = &e.description {
                let short: String = d.chars().take(350).collect();
                let more = if d.chars().count() > 350 { "…" } else { "" };
                ui.label(
                    RichText::new(format!("{short}{more}")).color(pal.text.gamma_multiply(0.85)),
                );
            }
            if let (Some(media), true) = (&e.media, cx.settings.load_images) {
                let max = vec2(420.0, 240.0);
                let s = fit(media.width, media.height, max.x, max.y);
                let url = discord::resized_url(&media.url, s.x as u32, s.y as u32);
                let badge = if e.kind == "video" { Some("▶") } else { None };
                let click = if open_url.is_empty() {
                    media.url.clone()
                } else {
                    open_url.clone()
                };
                image_box(
                    ui,
                    cx,
                    &url,
                    (media.width, media.height),
                    max,
                    480,
                    false,
                    badge,
                    &click,
                );
            }
        });
    let r = frame.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, vec2(4.0, r.height())),
        Rounding {
            nw: 8.0,
            sw: 8.0,
            ne: 0.0,
            se: 0.0,
        },
        stripe,
    );
}

// ======================= мелкие виджеты =======================

enum RailIcon<'a> {
    Home,
    Guild {
        tex: Option<(TextureId, Vec2)>,
        name: &'a str,
    },
}

fn rail_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: RailIcon<'_>,
    selected: bool,
    unread: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(76.0, 54.0), Sense::click());
    let hovered = resp.hovered();
    let center = rect.center();
    let icon_rect = Rect::from_center_size(center, vec2(48.0, 48.0));
    // плавное превращение круга в скруглённый квадрат и заливка акцентом
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("shape"), selected || hovered, 0.18);
    let rounding = 24.0 - (24.0 - pal.radius.max(12.0)) * t;
    if selected {
        glow(ui.painter(), center, 38.0, pal.accent.gamma_multiply(0.35));
    }
    let painter = ui.painter();

    let pill_target = if selected {
        38.0
    } else if hovered {
        20.0
    } else if unread {
        8.0
    } else {
        0.0
    };
    let pill_h = ui
        .ctx()
        .animate_value_with_time(resp.id.with("pill"), pill_target, 0.2);
    if pill_h > 0.5 {
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.left(), center.y - pill_h / 2.0),
                vec2(4.0, pill_h),
            ),
            Rounding {
                nw: 0.0,
                sw: 0.0,
                ne: 4.0,
                se: 4.0,
            },
            pal.text,
        );
    }

    match icon {
        RailIcon::Home => {
            let fill = mix(pal.elevated, pal.accent, t);
            painter.rect_filled(icon_rect, rounding, fill);
            let fg = mix(pal.text, Color32::WHITE, t);
            // облачко сообщения
            let b = Rect::from_center_size(center - vec2(0.0, 2.0), vec2(22.0, 16.0));
            painter.rect_filled(b, 5.0, fg);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(b.left() + 5.0, b.bottom() - 1.0),
                    pos2(b.left() + 12.0, b.bottom() - 1.0),
                    pos2(b.left() + 4.0, b.bottom() + 6.0),
                ],
                fg,
                Stroke::NONE,
            ));
        }
        RailIcon::Guild { tex, name } => match tex {
            Some((id, _)) => {
                egui::Image::from_texture(SizedTexture::new(id, icon_rect.size()))
                    .rounding(rounding)
                    .paint_at(ui, icon_rect);
            }
            None => {
                let fill = mix(pal.elevated, pal.accent, t);
                painter.rect_filled(icon_rect, rounding, fill);
                let fg = mix(pal.text, Color32::WHITE, t);
                let text = initials(name);
                let size = if text.chars().count() > 2 { 13.0 } else { 16.0 };
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(size),
                    fg,
                );
            }
        },
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

enum Leading<'a> {
    Glyph(&'a str),
    Avatar {
        tex: Option<(TextureId, Vec2)>,
        id: u64,
        name: &'a str,
        group: bool,
    },
}

#[allow(clippy::too_many_arguments)]
fn nav_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    selected: bool,
    unread: bool,
    enabled: bool,
    leading: Leading<'_>,
    text: &str,
    height: f32,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = resp.hovered() && enabled;
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("hover"), hovered || selected, 0.12);
    let r = (pal.radius - 4.0).max(6.0);
    if selected {
        ui.painter()
            .rect_filled(rect, r, mix(pal.hover, pal.selected, t));
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.left_top() + vec2(0.0, 7.0),
                vec2(3.0, rect.height() - 14.0),
            ),
            2.0,
            pal.accent,
        );
    } else if t > 0.01 {
        ui.painter()
            .rect_filled(rect, r, pal.hover.gamma_multiply(t));
    }
    let cy = rect.center().y;
    let mut x = rect.left() + 10.0;
    match leading {
        Leading::Glyph(g) => {
            ui.painter().text(
                pos2(x + 8.0, cy),
                Align2::CENTER_CENTER,
                g,
                FontId::proportional(17.0),
                pal.faint,
            );
            x += 24.0;
        }
        Leading::Avatar {
            tex,
            id,
            name,
            group,
        } => {
            let ar = Rect::from_center_size(pos2(x + 15.0, cy), vec2(30.0, 30.0));
            if group && tex.is_none() {
                ui.painter()
                    .circle_filled(ar.center(), 15.0, name_color(id).gamma_multiply(0.8));
                ui.painter().text(
                    ar.center(),
                    Align2::CENTER_CENTER,
                    "👥",
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
            } else {
                paint_avatar(ui, ar, tex, id, name);
            }
            x += 40.0;
        }
    }
    let color = if !enabled {
        pal.faint
    } else if selected || unread || hovered {
        pal.text
    } else {
        pal.muted
    };
    let right = if unread && !selected {
        rect.right() - 22.0
    } else {
        rect.right() - 6.0
    };
    let clip = Rect::from_min_max(pos2(x, rect.top()), pos2(right, rect.bottom()));
    ui.painter_at(clip).text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(15.0),
        color,
    );
    if unread && !selected {
        ui.painter()
            .circle_filled(pos2(rect.right() - 12.0, cy), 4.0, pal.accent);
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(rect.left() - 4.0, cy - 4.0), vec2(4.0, 8.0)),
            Rounding {
                nw: 0.0,
                sw: 0.0,
                ne: 3.0,
                se: 3.0,
            },
            pal.text,
        );
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Карточка-превью темы в настройках: мини-окно с панелями, сообщениями и акцентом.
fn theme_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    theme: Theme,
    label: &str,
    selected: bool,
) -> egui::Response {
    let preview = theme::palette_for(theme, pal.accent);
    let (slot, resp) = ui.allocate_exact_size(vec2(126.0, 92.0), Sense::click());
    if !ui.is_rect_visible(slot) {
        return resp;
    }
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("lift"), resp.hovered() || selected, 0.16);
    // при наведении карточка слегка «приподнимается»
    let rect = slot.translate(vec2(0.0, -3.0 * t));
    let rr = 12.0;
    let p = ui.painter();
    p.add(
        Shadow {
            offset: vec2(0.0, 4.0 + 4.0 * t),
            blur: 10.0 + 10.0 * t,
            spread: 0.0,
            color: Color32::from_black_alpha((30.0 + 50.0 * t) as u8),
        }
        .as_shape(rect, rr),
    );
    match theme::backdrop(theme) {
        Some(bd) => {
            p.rect_filled(rect, rr, bd.base);
            let pc = ui.painter_at(rect);
            for (k, c) in bd.glows.iter().enumerate() {
                let pos = rect.min + vec2(18.0 + k as f32 * 44.0, 22.0 + (k % 2) as f32 * 34.0);
                glow(&pc, pos, 50.0, c.gamma_multiply(0.75));
            }
            if bd.stars {
                for k in 0..14u32 {
                    let x = (k * 37 % 113) as f32 + 6.0;
                    let y = (k * 53 % 80) as f32 + 6.0;
                    pc.circle_filled(rect.min + vec2(x, y), 0.8, Color32::from_white_alpha(200));
                }
            }
        }
        None => {
            p.rect_filled(rect, rr, preview.main);
        }
    }
    let rail_w = 18.0;
    let side_w = 30.0;
    p.rect_filled(
        Rect::from_min_size(rect.min, vec2(rail_w, rect.height())),
        Rounding {
            nw: rr,
            sw: rr,
            ne: 0.0,
            se: 0.0,
        },
        preview.rail,
    );
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(rail_w, 0.0), vec2(side_w, rect.height())),
        0.0,
        preview.side,
    );
    for k in 0..3 {
        p.circle_filled(
            rect.min + vec2(rail_w / 2.0, 13.0 + k as f32 * 15.0),
            5.0,
            if k == 0 {
                preview.accent
            } else {
                preview.elevated
            },
        );
    }
    for k in 0..4 {
        let w = if k == 1 { 22.0 } else { 18.0 - k as f32 * 2.0 };
        p.rect_filled(
            Rect::from_min_size(
                rect.min + vec2(rail_w + 5.0, 10.0 + k as f32 * 10.0),
                vec2(w, 5.0),
            ),
            2.5,
            if k == 1 {
                preview.selected
            } else {
                preview.elevated
            },
        );
    }
    // сообщения: аватарка + строки текста
    let mx = rect.left() + rail_w + side_w + 7.0;
    for k in 0..2 {
        let y = rect.top() + 12.0 + k as f32 * 22.0;
        p.circle_filled(pos2(mx + 5.0, y + 5.0), 5.0, name_color(k as u64 * 7 + 3));
        p.rect_filled(
            Rect::from_min_size(pos2(mx + 14.0, y), vec2(26.0, 4.0)),
            2.0,
            preview.text.gamma_multiply(0.8),
        );
        p.rect_filled(
            Rect::from_min_size(pos2(mx + 14.0, y + 7.0), vec2(48.0 - k as f32 * 14.0, 4.0)),
            2.0,
            preview.muted.gamma_multiply(0.7),
        );
    }
    // поле ввода
    p.rect_filled(
        Rect::from_min_size(
            pos2(mx, rect.top() + 56.0),
            vec2(rect.right() - mx - 7.0, 9.0),
        ),
        4.5,
        preview.elevated,
    );
    // подпись
    let label_rect = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 20.0), rect.max);
    p.rect_filled(
        label_rect,
        Rounding {
            nw: 0.0,
            ne: 0.0,
            sw: rr,
            se: rr,
        },
        Color32::from_black_alpha(if preview.light { 0 } else { 70 }),
    );
    p.text(
        label_rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        preview.text,
    );
    let stroke = if selected {
        Stroke::new(2.5, pal.accent)
    } else {
        Stroke::new(1.0, mix(pal.border, pal.accent, t * 0.8))
    };
    p.rect_stroke(rect, rr, stroke);
    if selected {
        let c = pos2(rect.right() - 11.0, rect.top() + 11.0);
        p.circle_filled(c, 8.0, pal.accent);
        p.text(
            c,
            Align2::CENTER_CENTER,
            "✓",
            FontId::proportional(11.0),
            Color32::WHITE,
        );
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn paint_avatar(ui: &egui::Ui, rect: Rect, tex: Option<(TextureId, Vec2)>, id: u64, name: &str) {
    match tex {
        Some((tid, _)) => {
            egui::Image::from_texture(SizedTexture::new(tid, rect.size()))
                .rounding(rect.width() / 2.0)
                .paint_at(ui, rect);
        }
        None => {
            ui.painter()
                .circle_filled(rect.center(), rect.width() / 2.0, name_color(id));
            let letter: String = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_else(|| "?".into());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                letter,
                FontId::proportional(rect.width() * 0.45),
                Color32::WHITE,
            );
        }
    }
}

fn logo(ui: &mut egui::Ui, pal: &Palette, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    // мягкое «дыхание» свечения вокруг логотипа
    let pulse = 0.5 + 0.5 * (t * 1.6).sin();
    glow(
        ui.painter(),
        rect.center(),
        size * (0.95 + 0.12 * pulse),
        pal.accent.gamma_multiply(0.35 + 0.15 * pulse),
    );
    let r = size * 0.3;
    ui.painter().rect_filled(rect, r, pal.accent);
    // вертикальный градиент: светлее сверху, темнее снизу
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), rect.height() * 0.5));
    ui.painter().rect_filled(
        top,
        Rounding {
            nw: r,
            ne: r,
            sw: 0.0,
            se: 0.0,
        },
        Color32::from_white_alpha(28),
    );
    ui.painter()
        .rect_stroke(rect, r, Stroke::new(1.0, Color32::from_white_alpha(40)));
    ui.painter().text(
        rect.center() + vec2(0.0, 1.0),
        Align2::CENTER_CENTER,
        "R",
        FontId::new(size * 0.56, egui::FontFamily::Name("bold".into())),
        Color32::WHITE,
    );
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

fn field_label(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(RichText::new(text).size(11.5).strong().color(pal.muted));
}

fn accent_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    text: &str,
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.15);
        let pressed = resp.is_pointer_button_down_on();
        let r = rect.shrink(if pressed { 1.0 } else { 0.0 });
        let p = ui.painter();
        p.add(
            Shadow {
                offset: vec2(0.0, 3.0 + 3.0 * t),
                blur: 8.0 + 10.0 * t,
                spread: 0.0,
                color: pal.accent.gamma_multiply(0.25 + 0.25 * t),
            }
            .as_shape(r, 10.0),
        );
        p.rect_filled(r, 10.0, mix(pal.accent, Color32::WHITE, 0.10 * t));
        // светлый блик сверху — кнопка выглядит объёмной
        p.rect_filled(
            Rect::from_min_size(r.min, vec2(r.width(), r.height() / 2.0)),
            Rounding {
                nw: 10.0,
                ne: 10.0,
                sw: 0.0,
                se: 0.0,
            },
            Color32::from_white_alpha(14),
        );
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(15.5),
            Color32::WHITE,
        );
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn round_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    glyph: &str,
    fg: Color32,
    bg: Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.12);
    let fill = mix(
        bg,
        if pal.light {
            Color32::BLACK
        } else {
            Color32::WHITE
        },
        0.12 * t,
    );
    ui.painter().circle_filled(rect.center(), 15.0 + t, fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(16.0),
        fg,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn icon_button(ui: &mut egui::Ui, pal: &Palette, glyph: &str, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.12);
    if t > 0.01 {
        ui.painter().rect_filled(
            rect.shrink(2.0 * (1.0 - t)),
            8.0,
            pal.hover.gamma_multiply(t),
        );
    }
    let color = mix(pal.muted, pal.text, t);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(17.0),
        color,
    );
    resp.on_hover_text(tip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// Переключатель-«таблетка» (выбранный подсвечен акцентом).
fn toggle_chip(ui: &mut egui::Ui, pal: &Palette, text: &str, on: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::proportional(13.0),
        if on { Color32::WHITE } else { pal.text },
    );
    let size = galley.size() + vec2(22.0, 12.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let fill = if on {
        pal.accent
    } else {
        mix(pal.elevated, pal.hover, h)
    };
    ui.painter().rect_filled(rect, size.y / 2.0, fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn chip(ui: &mut egui::Ui, pal: &Palette, text: &str, color: Color32) {
    egui::Frame::none()
        .fill(pal.elevated)
        .rounding(10.0)
        .inner_margin(Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().monospace().color(color));
        });
}

fn section(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.add_space(14.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.5)
            .strong()
            .color(pal.accent),
    );
    ui.add_space(2.0);
}

fn empty_state(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(RichText::new(text).size(16.0).color(pal.muted));
    });
}

fn channel_intro(ui: &mut egui::Ui, pal: &Palette, icon: &str, name: &str) {
    ui.add_space(20.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.vertical(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 32.0, pal.elevated);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                icon,
                FontId::proportional(30.0),
                pal.text,
            );
            ui.label(
                RichText::new(format!("Добро пожаловать в {icon}{name}"))
                    .size(24.0)
                    .strong()
                    .color(pal.text),
            );
            ui.label(RichText::new("Это начало переписки.").color(pal.muted));
        });
    });
    ui.add_space(6.0);
}

fn day_separator(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_space(12.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let y = rect.center().y;
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(12.0), pal.faint);
    let tw = galley.size().x + 16.0;
    let cx = rect.center().x;
    ui.painter().hline(
        rect.left() + 16.0..=cx - tw / 2.0,
        y,
        Stroke::new(1.0, pal.border),
    );
    ui.painter().hline(
        cx + tw / 2.0..=rect.right() - 16.0,
        y,
        Stroke::new(1.0, pal.border),
    );
    ui.painter().galley(
        pos2(cx - galley.size().x / 2.0, y - galley.size().y / 2.0),
        galley,
        pal.faint,
    );
}

// ======================= вспомогательное =======================

fn day(time: &str) -> &str {
    time.get(..10).unwrap_or(time)
}

fn is_text(kind: u8) -> bool {
    matches!(kind, 0 | 1 | 3 | 5 | 10 | 11 | 12)
}

fn is_voice(kind: u8) -> bool {
    matches!(kind, 2 | 13)
}

fn initials(name: &str) -> String {
    let s: String = name
        .split_whitespace()
        .take(3)
        .filter_map(|w| w.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    if s.is_empty() {
        "?".into()
    } else {
        s
    }
}

fn snowflake_ms(id: u64) -> u64 {
    (id >> 22) + DISCORD_EPOCH
}

fn name_color(id: u64) -> Color32 {
    NAME_COLORS[((id >> 22) % NAME_COLORS.len() as u64) as usize]
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0 => String::new(),
        b if b < 1024 => format!("{b} Б"),
        b if b < 1024 * 1024 => format!("{:.1} КБ", b as f64 / 1024.0),
        b => format!("{:.1} МБ", b as f64 / 1_048_576.0),
    }
}

fn file_icon(a: &Attachment) -> &'static str {
    let name = a.filename.to_ascii_lowercase();
    if a.is_video() {
        "🎬"
    } else if a.content_type.starts_with("audio/")
        || [".mp3", ".ogg", ".wav", ".flac", ".m4a"]
            .iter()
            .any(|e| name.ends_with(e))
    {
        "🎵"
    } else if [".zip", ".rar", ".7z", ".tar", ".gz"]
        .iter()
        .any(|e| name.ends_with(e))
    {
        "📦"
    } else if [".txt", ".md", ".pdf", ".doc", ".docx", ".log"]
        .iter()
        .any(|e| name.ends_with(e))
    {
        "📄"
    } else {
        "📎"
    }
}

enum Row<'a> {
    Category(u64, &'a str),
    Chan(&'a Channel),
}

fn children(g: &Guild, parent: Option<u64>) -> Vec<&Channel> {
    let mut v: Vec<&Channel> = g
        .channels
        .iter()
        .filter(|c| c.kind != 4 && !extra::is_thread(c.kind) && c.parent == parent)
        .collect();
    v.sort_by_key(|c| (is_voice(c.kind), c.position, c.id));
    // активные ветки — сразу под своим каналом
    let mut out = Vec::with_capacity(v.len());
    for c in v {
        out.push(c);
        let mut threads: Vec<&Channel> = g
            .channels
            .iter()
            .filter(|t| extra::is_thread(t.kind) && t.parent == Some(c.id))
            .collect();
        threads.sort_by_key(|t| std::cmp::Reverse(t.id));
        out.extend(threads.into_iter().take(5));
    }
    out
}

/// Каналы сервера в порядке Discord: сначала без категории, затем по категориям.
fn channel_rows(g: &Guild) -> Vec<Row<'_>> {
    let mut rows: Vec<Row<'_>> = children(g, None).into_iter().map(Row::Chan).collect();
    let mut categories: Vec<&Channel> = g.channels.iter().filter(|c| c.kind == 4).collect();
    categories.sort_by_key(|c| (c.position, c.id));
    for cat in categories {
        let kids = children(g, Some(cat.id));
        if kids.is_empty() {
            continue;
        }
        rows.push(Row::Category(cat.id, &cat.name));
        rows.extend(kids.into_iter().map(Row::Chan));
    }
    rows
}

/// Строка «… печатает…» для канала.
fn typing_line(list: Option<&Vec<(u64, String, Instant)>>) -> Option<String> {
    let names: Vec<&str> = list?
        .iter()
        .filter(|(_, _, t)| t.elapsed() < Duration::from_secs(10))
        .map(|(_, n, _)| n.as_str())
        .collect();
    match names.len() {
        0 => None,
        1 => Some(format!("{} печатает…", names[0])),
        2 => Some(format!("{} и {} печатают…", names[0], names[1])),
        3 => Some(format!(
            "{}, {} и {} печатают…",
            names[0], names[1], names[2]
        )),
        _ => Some("Несколько человек печатают…".into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn member_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    tex: Option<(TextureId, Vec2)>,
    id: u64,
    name: &str,
    status: &str,
    color: Option<Color32>,
    offline: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.hover);
    }
    let cy = rect.center().y;
    let ar = Rect::from_center_size(pos2(rect.left() + 22.0, cy), vec2(30.0, 30.0));
    paint_avatar(ui, ar, tex, id, name);
    let dot = match status {
        "online" => Some(pal.ok),
        "idle" => Some(pal.warn),
        "dnd" => Some(pal.danger),
        _ => None,
    };
    if let Some(c) = dot {
        let p = ar.right_bottom() - vec2(3.0, 3.0);
        ui.painter().circle_filled(p, 6.0, pal.side);
        ui.painter().circle_filled(p, 4.5, c);
    }
    let text_color = color.unwrap_or(pal.text);
    let text_color = if offline {
        text_color.gamma_multiply(0.45)
    } else {
        text_color
    };
    let x = rect.left() + 46.0;
    let clip = Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - 6.0, rect.bottom()));
    ui.painter_at(clip).text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(14.5),
        text_color,
    );
    resp
}

/// Плавное смешивание двух цветов (для анимаций).
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// Имя участника голосового канала из списка участников сервера.
fn member_name(lists: &HashMap<u64, MemberList>, guild_id: u64, user_id: u64) -> String {
    lists
        .get(&guild_id)
        .and_then(|l| {
            l.items.iter().flatten().find_map(|it| match it {
                ListItem::Member { id, name, .. } if *id == user_id => Some(name.clone()),
                _ => None,
            })
        })
        .unwrap_or_else(|| "Участник".into())
}

fn voice_member_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    tex: Option<(TextureId, Vec2)>,
    m: &VoiceMember,
    name: &str,
    speaking: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.hover);
    }
    let cy = rect.center().y;
    let ar = Rect::from_center_size(pos2(rect.left() + 42.0, cy), vec2(22.0, 22.0));
    let glow = ui
        .ctx()
        .animate_bool_with_time(resp.id.with(("talk", m.user_id)), speaking, 0.12);
    if glow > 0.01 {
        ui.painter().circle_stroke(
            ar.center(),
            13.0,
            Stroke::new(2.0, pal.ok.gamma_multiply(glow)),
        );
    }
    paint_avatar(ui, ar, tex, m.user_id, name);
    let mut right = rect.right() - 6.0;
    let icons = match (m.deafened, m.muted) {
        (true, _) => "🎧✕",
        (false, true) => "🔇",
        _ => "",
    };
    if !icons.is_empty() {
        ui.painter().text(
            pos2(right, cy),
            Align2::RIGHT_CENTER,
            icons,
            FontId::proportional(13.0),
            pal.faint,
        );
        right -= 30.0;
    }
    let x = ar.right() + 8.0;
    let clip = Rect::from_min_max(pos2(x, rect.top()), pos2(right, rect.bottom()));
    ui.painter_at(clip).text(
        pos2(x, cy),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(14.0),
        if speaking { pal.text } else { pal.muted },
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn voice_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    tip: &str,
    active: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 32.0), Sense::click());
    let fill = if active {
        pal.danger.gamma_multiply(0.25)
    } else if resp.hovered() {
        pal.hover
    } else {
        pal.elevated
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    draw_icon(
        ui.painter(),
        icon,
        rect.center(),
        17.0,
        if active { pal.danger } else { pal.text },
        active,
    );
    resp.on_hover_text(tip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

fn voice_opts(s: &Settings) -> voice::VoiceOpts {
    voice::VoiceOpts {
        input: s.input_device.clone(),
        output: s.output_device.clone(),
        threshold: s.mic_threshold,
        always: s.always_transmit,
        echo_guard: s.echo_guard,
        echo_cancel: s.echo_cancel,
        noise: s.noise,
        agc: s.agc,
        input_gain: s.input_gain,
        ptt: s.ptt,
        output_volume: s.output_volume,
        user_volumes: s.user_volumes.clone(),
    }
}

/// Клавиши для рации: (название, виртуальный код Windows).
const PTT_KEYS: [(&str, u32); 10] = [
    ("Ё / `", 0xC0),
    ("Боковая кнопка мыши 1", 0x05),
    ("Боковая кнопка мыши 2", 0x06),
    ("Средняя кнопка мыши", 0x04),
    ("V", 0x56),
    ("B", 0x42),
    ("Caps Lock", 0x14),
    ("Левый Alt", 0xA4),
    ("Правый Ctrl", 0xA3),
    ("F8", 0x77),
];

/// Клавиша рации, которую слушает фоновый опрос (0 — рация выключена).
static PTT_VK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Фоновый опрос клавиши рации: работает, даже когда окно Rustcord не в фокусе (Windows).
#[cfg(windows)]
fn spawn_ptt_watcher(ctx: egui::Context) {
    use std::sync::atomic::Ordering;
    let _ = std::thread::Builder::new()
        .name("ptt".into())
        .spawn(move || {
            let mut last = false;
            loop {
                let vk = PTT_VK.load(Ordering::Relaxed);
                let down = vk != 0 && {
                    // SAFETY: GetAsyncKeyState только читает состояние клавиши.
                    let state = unsafe {
                        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32)
                    };
                    (state as u16 & 0x8000) != 0
                };
                if down != last {
                    last = down;
                    voice::PTT_DOWN.store(down, Ordering::Relaxed);
                    ctx.request_repaint();
                }
                std::thread::sleep(Duration::from_millis(if vk == 0 { 250 } else { 15 }));
            }
        });
}

/// Вне Windows рация работает, пока окно в фокусе (клавиша опрашивается в интерфейсе).
#[cfg(not(windows))]
fn spawn_ptt_watcher(_ctx: egui::Context) {}

/// Клавиша egui для кода рации (запасной путь, когда окно в фокусе).
fn ptt_egui_key(vk: u32) -> Option<egui::Key> {
    Some(match vk {
        0xC0 => egui::Key::Backtick,
        0x56 => egui::Key::V,
        0x42 => egui::Key::B,
        0x77 => egui::Key::F8,
        _ => return None,
    })
}

/// Мягкое светящееся пятно (радиальный градиент).
fn glow(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let segments = 48u32;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    let mid = color.gamma_multiply(0.45);
    for i in 0..segments {
        let a = i as f32 / segments as f32 * std::f32::consts::TAU;
        let dir = vec2(a.cos(), a.sin());
        mesh.colored_vertex(center + dir * radius * 0.5, mid);
        mesh.colored_vertex(center + dir * radius, Color32::TRANSPARENT);
    }
    for i in 0..segments {
        let a = 1 + 2 * i;
        let b = 1 + 2 * ((i + 1) % segments);
        mesh.add_triangle(0, a, b);
        mesh.add_triangle(a, a + 1, b + 1);
        mesh.add_triangle(a, b + 1, b);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Живой фон анимированных тем: медленно плывущие цветные пятна под полупрозрачными панелями.
fn paint_backdrop(ctx: &egui::Context, bd: &theme::Backdrop, animate: bool) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    painter.rect_filled(screen, 0.0, bd.base);
    let t = if animate { ctx.input(|i| i.time) } else { 40.0 };
    let size = screen.size();
    let m = size.x.max(size.y);
    for (i, c) in bd.glows.iter().enumerate() {
        let fi = i as f64;
        let speed = 0.05 + 0.025 * fi;
        let cx = 0.5 + 0.38 * (t * speed + fi * 2.1).sin();
        let cy = 0.5 + 0.36 * (t * speed * 0.8 + fi * 1.3).cos();
        let r = m * (0.42 + 0.08 * (t * 0.21 + fi).sin() as f32);
        glow(
            &painter,
            screen.min + vec2(cx as f32 * size.x, cy as f32 * size.y),
            r,
            c.gamma_multiply(0.55),
        );
    }
    if bd.stars {
        // мерцающие звёзды: положения фиксированы, яркость медленно «дышит»
        for k in 0..90u64 {
            let h = k.wrapping_mul(2_654_435_761) ^ 0x9E37_79B9;
            let x = (h % 10_000) as f32 / 10_000.0;
            let y = ((h / 10_000) % 10_000) as f32 / 10_000.0;
            let phase = (h % 628) as f64 / 100.0;
            let a = 0.35 + 0.65 * (0.5 + 0.5 * (t * 0.9 + phase).sin()) as f32;
            let r = if k % 9 == 0 { 1.6 } else { 0.9 };
            painter.circle_filled(
                screen.min + vec2(x * size.x, y * size.y),
                r,
                Color32::from_white_alpha((a * 210.0) as u8),
            );
        }
    }
    if animate {
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

/// Полоска громкости микрофона с отметкой порога.
fn level_meter(ui: &mut egui::Ui, pal: &Palette, level: f32, threshold: f32, height: f32) {
    let scale = |x: f32| ((x.max(1e-5).log10() + 4.0) / 3.0).clamp(0.0, 1.0);
    let width = ui.available_width().min(390.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    ui.painter().rect_filled(rect, height / 2.0, pal.elevated);
    let v = scale(level);
    let fill = Rect::from_min_size(rect.min, vec2(rect.width() * v, rect.height()));
    let color = if level > threshold { pal.ok } else { pal.muted };
    ui.painter().rect_filled(fill, height / 2.0, color);
    let x = rect.left() + rect.width() * scale(threshold);
    ui.painter().vline(
        x,
        rect.top() - 2.0..=rect.bottom() + 2.0,
        Stroke::new(2.0, pal.text),
    );
}

struct Tile {
    id: u64,
    name: String,
    tex: Option<(TextureId, Vec2)>,
    muted: bool,
    deafened: bool,
    speaking: bool,
    streaming: bool,
}

/// Состояние сцены, которое нужно для её отрисовки.
struct StageState {
    mute: bool,
    deaf: bool,
    me: u64,
    streaming: bool,
    live: bool,
    /// смотрим трансляцию: (id автора, кадр)
    watch: Option<(u64, Option<(TextureId, Vec2)>)>,
}

/// Окно звонка: плитки участников и панель управления.
fn draw_stage(
    ui: &mut egui::Ui,
    pal: &Palette,
    rect: Rect,
    tiles: &[Tile],
    st: &StageState,
    actions: &mut Vec<Action>,
) {
    let bar_h = 86.0;
    let mut area = Rect::from_min_max(rect.min + vec2(14.0, 12.0), rect.max - vec2(14.0, bar_h));
    if let Some((owner, frame)) = st.watch {
        // большая трансляция сверху, участники — узкой полосой под ней
        let strip_h = if tiles.len() > 1 {
            (area.height() * 0.2).clamp(64.0, 120.0)
        } else {
            0.0
        };
        let video = Rect::from_min_max(
            area.min,
            pos2(
                area.max.x,
                area.max.y - strip_h - if strip_h > 0.0 { 10.0 } else { 0.0 },
            ),
        );
        let name = tiles
            .iter()
            .find(|t| t.id == owner)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "Участник".into());
        draw_stream_view(ui, pal, video, &name, frame, false, actions);
        area = Rect::from_min_max(pos2(area.min.x, area.max.y - strip_h), area.max);
        if strip_h <= 0.0 {
            draw_stage_bar(ui, pal, rect, bar_h, st, actions);
            return;
        }
    }
    let n = tiles.len().max(1);
    let gap = 10.0;
    // подбираем сетку, при которой плитки 16:9 получаются самыми крупными
    let mut best = (1usize, 0.0f32, 0.0f32);
    for cols in 1..=n {
        let rows = (n + cols - 1) / cols;
        let w = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let h = (area.height() - gap * (rows as f32 - 1.0)) / rows as f32;
        let (tw, th) = if w / h > 16.0 / 9.0 {
            (h * 16.0 / 9.0, h)
        } else {
            (w, w * 9.0 / 16.0)
        };
        if tw > best.1 {
            best = (cols, tw, th);
        }
    }
    let (cols, tw, th) = best;
    let rows = (n + cols - 1) / cols;
    let total_h = rows as f32 * th + (rows as f32 - 1.0) * gap;
    for (i, t) in tiles.iter().enumerate() {
        let row = i / cols;
        let col = i % cols;
        let in_row = if row == rows - 1 {
            n - row * cols
        } else {
            cols
        };
        let row_w = in_row as f32 * tw + (in_row as f32 - 1.0) * gap;
        let x = area.center().x - row_w / 2.0 + col as f32 * (tw + gap);
        let y = area.center().y - total_h / 2.0 + row as f32 * (th + gap);
        let tile = Rect::from_min_size(pos2(x, y), vec2(tw, th));
        let r = pal.radius + 2.0;
        let bg = mix(
            Color32::from_gray(if pal.light { 210 } else { 34 }),
            name_color(t.id),
            0.4,
        );
        ui.painter().rect_filled(tile, r, bg);
        let d = (th * 0.42).min(120.0);
        let ar = Rect::from_center_size(tile.center() - vec2(0.0, th * 0.04), vec2(d, d));
        paint_avatar(ui, ar, t.tex, t.id, &t.name);
        // имя в «таблетке» снизу слева
        let galley =
            ui.painter()
                .layout_no_wrap(t.name.clone(), FontId::proportional(13.5), Color32::WHITE);
        let pill = Rect::from_min_size(
            tile.left_bottom() + vec2(10.0, -galley.size().y - 18.0),
            galley.size() + vec2(16.0, 8.0),
        );
        ui.painter()
            .rect_filled(pill, 8.0, Color32::from_black_alpha(150));
        ui.painter()
            .galley(pill.min + vec2(8.0, 4.0), galley, Color32::WHITE);
        let icon = match (t.deafened, t.muted) {
            (true, _) => "🎧",
            (false, true) => "🔇",
            _ => "",
        };
        if !icon.is_empty() {
            let ir = Rect::from_min_size(tile.right_top() + vec2(-38.0, 10.0), vec2(28.0, 28.0));
            ui.painter()
                .circle_filled(ir.center(), 14.0, Color32::from_black_alpha(150));
            ui.painter().text(
                ir.center(),
                Align2::CENTER_CENTER,
                icon,
                FontId::proportional(14.0),
                pal.danger,
            );
        }
        let g =
            ui.ctx()
                .animate_bool_with_time(egui::Id::new(("stage_talk", t.id)), t.speaking, 0.15);
        if g > 0.01 {
            ui.painter()
                .rect_stroke(tile, r, Stroke::new(3.0, pal.ok.gamma_multiply(g)));
        }
        if t.streaming {
            live_badge(
                ui,
                tile.left_top() + vec2(10.0, 10.0),
                if t.id == st.me {
                    "ВЫ В ЭФИРЕ"
                } else {
                    "В ЭФИРЕ"
                },
            );
            if t.id != st.me {
                let watching_this = st.watch.map_or(false, |(o, _)| o == t.id);
                let resp = ui
                    .interact(tile, ui.id().with(("watch_tile", t.id)), Sense::click())
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(if watching_this {
                        "Вы уже смотрите"
                    } else {
                        "Смотреть трансляцию"
                    });
                if resp.hovered() && !watching_this {
                    ui.painter()
                        .rect_filled(tile, r, Color32::from_black_alpha(90));
                    ui.painter().text(
                        tile.center(),
                        Align2::CENTER_CENTER,
                        "▶  Смотреть",
                        FontId::proportional(16.0),
                        Color32::WHITE,
                    );
                }
                if resp.clicked() && !watching_this {
                    actions.push(Action::WatchStream(t.id));
                }
            }
        }
    }
    draw_stage_bar(ui, pal, rect, bar_h, st, actions);
}

/// Красная плашка «В ЭФИРЕ».
fn live_badge(ui: &egui::Ui, at: egui::Pos2, text: &str) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(11.5), Color32::WHITE);
    let r = Rect::from_min_size(at, galley.size() + vec2(14.0, 6.0));
    ui.painter()
        .rect_filled(r, 6.0, Color32::from_rgb(237, 66, 69));
    ui.painter()
        .galley(r.min + vec2(7.0, 3.0), galley, Color32::WHITE);
}

/// Окно просмотра чужой трансляции.
fn draw_stream_view(
    ui: &mut egui::Ui,
    pal: &Palette,
    rect: Rect,
    name: &str,
    frame: Option<(TextureId, Vec2)>,
    fullscreen: bool,
    actions: &mut Vec<Action>,
) {
    let r = if fullscreen { 0.0 } else { pal.radius + 2.0 };
    ui.painter().rect_filled(rect, r, Color32::from_gray(8));
    match frame {
        Some((tex, size)) if size.x > 0.0 && size.y > 0.0 => {
            let scale = (rect.width() / size.x).min(rect.height() / size.y);
            let img = Rect::from_center_size(rect.center(), size * scale);
            ui.painter().image(
                tex,
                img,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        _ => {
            let t = ui.input(|i| i.time) as f32;
            let c = rect.center();
            for k in 0..8 {
                let a = t * 4.0 + k as f32 * std::f32::consts::TAU / 8.0;
                let alpha = ((k as f32 + 1.0) / 8.0 * 220.0) as u8;
                ui.painter().circle_filled(
                    c + vec2(a.cos(), a.sin()) * 18.0 - vec2(0.0, 14.0),
                    3.5,
                    Color32::from_white_alpha(alpha),
                );
            }
            ui.painter().text(
                c + vec2(0.0, 24.0),
                Align2::CENTER_CENTER,
                "Подключаюсь к трансляции…",
                FontId::proportional(14.0),
                Color32::from_gray(200),
            );
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }
    live_badge(ui, rect.left_top() + vec2(12.0, 12.0), "В ЭФИРЕ");
    let galley = ui.painter().layout_no_wrap(
        format!("Экран: {name}"),
        FontId::proportional(13.5),
        Color32::WHITE,
    );
    let pill = Rect::from_min_size(
        rect.left_bottom() + vec2(12.0, -galley.size().y - 20.0),
        galley.size() + vec2(16.0, 8.0),
    );
    ui.painter()
        .rect_filled(pill, 8.0, Color32::from_black_alpha(160));
    ui.painter()
        .galley(pill.min + vec2(8.0, 4.0), galley, Color32::WHITE);
    // кнопка закрытия
    let close = Rect::from_min_size(rect.right_top() + vec2(-46.0, 10.0), vec2(36.0, 36.0));
    let resp = ui
        .interact(close, ui.id().with("close_stream"), Sense::click())
        .on_hover_text("Перестать смотреть")
        .on_hover_cursor(CursorIcon::PointingHand);
    let h = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    ui.painter().circle_filled(
        close.center(),
        18.0,
        mix(Color32::from_black_alpha(160), pal.danger, h),
    );
    ui.painter().text(
        close.center(),
        Align2::CENTER_CENTER,
        "✕",
        FontId::proportional(16.0),
        Color32::WHITE,
    );
    if resp.clicked() {
        actions.push(Action::StopWatching);
    }
    // кнопка «на весь экран»
    let fs = Rect::from_min_size(rect.right_top() + vec2(-90.0, 10.0), vec2(36.0, 36.0));
    let fresp = ui
        .interact(fs, ui.id().with("fs_stream"), Sense::click())
        .on_hover_text(if fullscreen {
            "Свернуть (Esc)"
        } else {
            "На весь экран"
        })
        .on_hover_cursor(CursorIcon::PointingHand);
    let fh = ui
        .ctx()
        .animate_bool_with_time(fresp.id.with("h"), fresp.hovered(), 0.12);
    ui.painter().circle_filled(
        fs.center(),
        18.0,
        mix(Color32::from_black_alpha(160), pal.accent, fh),
    );
    // рисуем четыре уголка
    let c = fs.center();
    let (a, b) = if fullscreen { (3.0, 8.0) } else { (8.0, 3.0) };
    let st = Stroke::new(2.0, Color32::WHITE);
    for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let corner = c + vec2(sx * a, sy * a);
        let tip = c + vec2(sx * b, sy * b);
        ui.painter()
            .line_segment([corner, pos2(tip.x, corner.y)], st);
        ui.painter()
            .line_segment([corner, pos2(corner.x, tip.y)], st);
    }
    if fresp.clicked() {
        actions.push(Action::ToggleStreamFullscreen);
    }
}

/// Панель управления звонком — «таблетка» под плитками.
fn draw_stage_bar(
    ui: &mut egui::Ui,
    pal: &Palette,
    rect: Rect,
    bar_h: f32,
    st: &StageState,
    actions: &mut Vec<Action>,
) {
    let (mute, deaf) = (st.mute, st.deaf);

    // панель управления — отдельная «таблетка» под плитками
    let buttons: [(Icon, &str, bool, bool); 4] = [
        (
            Icon::Mic,
            if mute {
                "Включить микрофон"
            } else {
                "Выключить микрофон"
            },
            mute,
            false,
        ),
        (
            Icon::Headphones,
            if deaf {
                "Включить звук"
            } else {
                "Выключить звук"
            },
            deaf,
            false,
        ),
        (
            Icon::Screen,
            if st.streaming {
                "Остановить демонстрацию"
            } else {
                "Демонстрация экрана"
            },
            false,
            false,
        ),
        (Icon::HangUp, "Отключиться", false, true),
    ];
    let bw = 56.0;
    let gap = 10.0;
    let total = buttons.len() as f32 * bw + (buttons.len() as f32 - 1.0) * gap;
    let y = rect.bottom() - bar_h / 2.0;
    let pill = Rect::from_center_size(pos2(rect.center().x, y), vec2(total + 20.0, 58.0));
    ui.painter().rect_filled(
        pill,
        29.0,
        Color32::from_black_alpha(if pal.light { 30 } else { 90 }),
    );
    ui.painter()
        .rect_stroke(pill, 29.0, Stroke::new(1.0, pal.border));
    let mut x = pill.left() + 10.0;
    for (i, (icon, tip, active, danger)) in buttons.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x, y - 23.0), vec2(bw, 46.0));
        let resp = ui.interact(r, ui.id().with(("stage_btn", i)), Sense::click());
        let h = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
        let fill = if *danger {
            mix(pal.danger, Color32::WHITE, 0.12 * h)
        } else if *active {
            mix(
                pal.danger.gamma_multiply(0.3),
                pal.danger.gamma_multiply(0.45),
                h,
            )
        } else if i == 2 && st.streaming {
            mix(pal.accent, Color32::WHITE, 0.12 * h)
        } else {
            mix(pal.elevated, pal.hover, h)
        };
        ui.painter().rect_filled(r, 23.0, fill);
        let fg = if *danger {
            Color32::WHITE
        } else if *active {
            pal.danger
        } else if i == 2 && st.streaming {
            Color32::WHITE
        } else {
            pal.text
        };
        if i == 2 && st.streaming && !st.live {
            // ждём первый кадр — пульсирующая обводка
            let t = (ui.input(|i| i.time) * 4.0).sin() as f32 * 0.5 + 0.5;
            ui.painter().rect_stroke(
                r.expand(2.0),
                25.0,
                Stroke::new(2.0, pal.accent.gamma_multiply(t)),
            );
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        draw_icon(ui.painter(), *icon, r.center(), 20.0, fg, *active);
        let resp = resp
            .on_hover_text(*tip)
            .on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() {
            match i {
                0 => actions.push(Action::ToggleMute),
                1 => actions.push(Action::ToggleDeaf),
                2 if st.streaming => actions.push(Action::StopStream),
                2 => actions.push(Action::OpenStreamPicker),
                3 => actions.push(Action::LeaveVoice),
                _ => {}
            }
        }
        x += bw + gap;
    }
}

/// Панель-«карточка» с отступом от краёв окна.
fn card(fill: Color32, radius: f32, outer: Margin) -> egui::Frame {
    egui::Frame::none()
        .fill(fill)
        .rounding(radius)
        .outer_margin(outer)
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Mic,
    Headphones,
    Screen,
    HangUp,
}

fn arc(center: egui::Pos2, r: f32, from: f32, to: f32) -> Vec<egui::Pos2> {
    (0..=20)
        .map(|i| {
            let a = from + (to - from) * i as f32 / 20.0;
            center + vec2(a.cos(), a.sin()) * r
        })
        .collect()
}

/// Простые векторные иконки (одинаково чёткие при любом масштабе).
fn draw_icon(
    p: &egui::Painter,
    icon: Icon,
    c: egui::Pos2,
    size: f32,
    color: Color32,
    crossed: bool,
) {
    use std::f32::consts::PI;
    let s = size;
    let w = (s * 0.11).max(1.6);
    let stroke = Stroke::new(w, color);
    match icon {
        Icon::Mic => {
            let body = Rect::from_center_size(c - vec2(0.0, s * 0.14), vec2(s * 0.36, s * 0.56));
            p.rect_filled(body, s * 0.18, color);
            p.add(egui::Shape::line(
                arc(c - vec2(0.0, s * 0.08), s * 0.32, 0.0, PI),
                stroke,
            ));
            p.line_segment([c + vec2(0.0, s * 0.24), c + vec2(0.0, s * 0.42)], stroke);
            p.line_segment(
                [c + vec2(-s * 0.16, s * 0.42), c + vec2(s * 0.16, s * 0.42)],
                stroke,
            );
        }
        Icon::Headphones => {
            p.add(egui::Shape::line(
                arc(c + vec2(0.0, s * 0.06), s * 0.38, PI, 2.0 * PI),
                stroke,
            ));
            for dx in [-1.0f32, 1.0] {
                let ear = Rect::from_center_size(
                    c + vec2(dx * s * 0.34, s * 0.16),
                    vec2(s * 0.2, s * 0.34),
                );
                p.rect_filled(ear, s * 0.07, color);
            }
        }
        Icon::Screen => {
            let r = Rect::from_center_size(c - vec2(0.0, s * 0.08), vec2(s * 0.82, s * 0.56));
            p.rect_stroke(r, s * 0.08, stroke);
            p.line_segment([c + vec2(0.0, s * 0.2), c + vec2(0.0, s * 0.36)], stroke);
            p.line_segment(
                [c + vec2(-s * 0.2, s * 0.38), c + vec2(s * 0.2, s * 0.38)],
                stroke,
            );
        }
        Icon::HangUp => {
            // трубка, лежащая «рогами» вниз
            let thick = Stroke::new(s * 0.2, color);
            p.add(egui::Shape::line(
                arc(c + vec2(0.0, s * 0.36), s * 0.5, -PI * 0.82, -PI * 0.18),
                thick,
            ));
            for dx in [-1.0f32, 1.0] {
                let end = Rect::from_center_size(
                    c + vec2(dx * s * 0.36, s * 0.12),
                    vec2(s * 0.26, s * 0.2),
                );
                p.rect_filled(end, s * 0.06, color);
            }
        }
    }
    if crossed {
        let d = s * 0.42;
        p.line_segment(
            [c + vec2(-d, -d), c + vec2(d, d)],
            Stroke::new(w * 1.3, color),
        );
    }
}
