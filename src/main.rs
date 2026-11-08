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

mod discord;
mod fonts;
mod media;
mod net;
mod settings;
mod store;
mod theme;

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
    Attachment, Auth, Channel, Cmd, Dm, Embed, Event, Guild, Msg, Status, DISCORD_EPOCH,
};
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

enum Action {
    SelectGuild(Option<u64>),
    OpenChannel(u64),
    ToggleCategory(u64),
    LoadOlder(u64),
    Send(u64, String),
    Attach(u64),
    Download(String, String),
    OpenUrl(String),
    OpenSettings,
    Logout,
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

    // служебное
    toast: Option<(String, Instant, bool)>,
    mem_mb: f64,
    mem_checked: Instant,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::install(&cc.egui_ctx);
        let settings = settings::load();
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
            toast: None,
            mem_mb: 0.0,
            mem_checked: Instant::now() - Duration::from_secs(10),
        };
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

    fn logout(&mut self, error: Option<String>) {
        let _ = self.cmd.send(Cmd::Logout);
        store::clear();
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
        self.sel_channel = Some(id);
        self.last_channel.insert(self.sel_guild, id);
        self.unread.remove(&id);
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
            } => {
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
                let guild = self
                    .sel_guild
                    .filter(|id| self.guilds.iter().any(|g| g.id == *id));
                self.select_guild(guild);
            }
            Event::GuildCreate(g) => {
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
            Event::MessageCreate(m) => self.on_message(m),
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
            Event::Info(t) => self.toast(t, false),
            Event::Error(e) => self.toast(e, true),
            Event::Fatal(e) => self.logout(Some(e)),
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
                    },
                ),
                None => {}
            }
        }
        if m.author_id != self.me_id && self.sel_channel != Some(cid) {
            self.unread.insert(cid, m.guild_id.unwrap_or(0));
        }
        let limit = self.settings.messages_per_channel;
        if let Some(c) = self.cache.get_mut(&cid) {
            if c.msgs.iter().rev().take(30).any(|x| x.id == m.id) {
                return;
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
        settings::save(&self.settings);
    }

    // ======================= экран входа =======================

    fn login_ui(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let mut submit = false;
        let mut cancel = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(pal.rail))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(48.0);
                        egui::Frame::none()
                            .fill(pal.side)
                            .rounding(18.0)
                            .stroke(Stroke::new(1.0, pal.border))
                            .inner_margin(Margin::same(28.0))
                            .show(ui, |ui| {
                                ui.set_width(380.0);
                                ui.vertical_centered(|ui| {
                                    logo(ui, &pal, 64.0);
                                    ui.add_space(8.0);
                                    ui.label(RichText::new("Rustcord").size(28.0).strong().color(pal.text));
                                    ui.label(RichText::new("Лёгкий клиент Discord на Rust").color(pal.muted));
                                });
                                ui.add_space(20.0);

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
                                ui.checkbox(&mut self.bot_input, "Это токен бота");
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
                                    if accent_button(ui, &pal, "Войти", width, 40.0).clicked() {
                                        submit = true;
                                    }
                                }

                                if let Some(e) = &self.login_error {
                                    ui.add_space(10.0);
                                    egui::Frame::none()
                                        .fill(pal.danger.gamma_multiply(0.15))
                                        .rounding(8.0)
                                        .inner_margin(Margin::same(10.0))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new(e).color(pal.text));
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
            self.logout(None);
        } else if submit && !self.logging_in {
            self.login_from_input();
        }
    }

    // ======================= главный экран =======================

    fn main_ui(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let mut actions: Vec<Action> = Vec::new();
        let unread_guilds: HashSet<u64> = self.unread.values().copied().collect();
        let title = self.channel_title();
        let load_avatars = self.settings.load_avatars;

        // --- полоса серверов ---
        egui::SidePanel::left("rail")
            .resizable(false)
            .exact_width(76.0)
            .frame(
                egui::Frame::none()
                    .fill(pal.rail)
                    .inner_margin(Margin::symmetric(0.0, 10.0)),
            )
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
                            if resp.clicked() {
                                actions.push(Action::SelectGuild(Some(g.id)));
                            }
                        }
                    });
            });

        // --- каналы / диалоги ---
        egui::SidePanel::left("side")
            .resizable(false)
            .exact_width(250.0)
            .frame(egui::Frame::none().fill(pal.side))
            .show(ctx, |ui| {
                let guild = self.sel_guild.and_then(|id| self.guilds.iter().find(|g| g.id == id));

                egui::TopBottomPanel::top("side_head")
                    .frame(egui::Frame::none().inner_margin(Margin::symmetric(16.0, 14.0)))
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        let name = guild.map(|g| g.name.as_str()).unwrap_or("Личные сообщения");
                        ui.add(egui::Label::new(RichText::new(name).size(16.5).strong().color(pal.text)).truncate());
                    });

                egui::TopBottomPanel::bottom("me_panel")
                    .frame(egui::Frame::none().fill(pal.rail.gamma_multiply(0.9)).inner_margin(Margin::symmetric(10.0, 8.0)))
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                            let tex = match (&self.me_avatar, load_avatars) {
                                (Some(h), true) => {
                                    let url = discord::avatar_url(self.me_id, h);
                                    self.media.get(ui.ctx(), &self.cmd, &url, &url, 64, false)
                                }
                                _ => None,
                            };
                            paint_avatar(ui, rect, tex, self.me_id, &self.me_name);
                            let dot = match self.status {
                                Status::Online => pal.ok,
                                _ => pal.warn,
                            };
                            ui.painter().circle_filled(rect.right_bottom() - vec2(4.0, 4.0), 6.0, pal.rail);
                            ui.painter().circle_filled(rect.right_bottom() - vec2(4.0, 4.0), 4.5, dot);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add(egui::Label::new(RichText::new(&self.me_name).strong().color(pal.text)).truncate());
                                let status = match self.status {
                                    Status::Online => "в сети",
                                    Status::Connecting => "подключение…",
                                    Status::Reconnecting => "переподключение…",
                                };
                                ui.label(RichText::new(status).small().color(pal.muted));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, &pal, "⚙", "Настройки").clicked() {
                                    actions.push(Action::OpenSettings);
                                }
                            });
                        });
                    });

                egui::CentralPanel::default()
                    .frame(egui::Frame::none().inner_margin(Margin::symmetric(8.0, 4.0)))
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
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
                                                let arrow = if collapsed { "›" } else { "⌄" };
                                                let text = RichText::new(format!("{arrow}  {}", name.to_uppercase()))
                                                    .size(11.5)
                                                    .strong()
                                                    .color(pal.faint);
                                                if ui.add(egui::Label::new(text).sense(Sense::click()).truncate()).clicked() {
                                                    actions.push(Action::ToggleCategory(id));
                                                }
                                            }
                                            Row::Chan(c) => {
                                                let selected = self.sel_channel == Some(c.id);
                                                if let Some(cat) = current_cat {
                                                    if self.collapsed.contains(&cat) && !selected {
                                                        continue;
                                                    }
                                                }
                                                let unread = self.unread.contains_key(&c.id);
                                                let (glyph, enabled) = if is_text(c.kind) {
                                                    (if c.kind == 5 { "📢" } else { "#" }, true)
                                                } else if is_voice(c.kind) {
                                                    ("🔊", false)
                                                } else {
                                                    ("💬", false)
                                                };
                                                let resp = nav_row(ui, &pal, selected, unread, enabled, Leading::Glyph(glyph), &c.name, 32.0);
                                                if enabled && resp.clicked() {
                                                    actions.push(Action::OpenChannel(c.id));
                                                }
                                                if !enabled {
                                                    resp.on_hover_text("Голосовые каналы и форумы пока не поддерживаются");
                                                }
                                            }
                                        }
                                    }
                                }
                                None => {
                                    if self.dms.is_empty() {
                                        ui.add_space(12.0);
                                        ui.label(RichText::new("Диалогов пока нет").color(pal.muted));
                                    }
                                    for d in &self.dms {
                                        let tex = match (&d.avatar, load_avatars) {
                                            (Some((uid, Some(hash))), true) => {
                                                let url = discord::avatar_url(*uid, hash);
                                                self.media.get(ui.ctx(), &self.cmd, &url, &url, 64, false)
                                            }
                                            _ => None,
                                        };
                                        let color_id = d.avatar.as_ref().map(|(u, _)| *u).unwrap_or(d.id);
                                        let lead = Leading::Avatar { tex, id: color_id, name: &d.name, group: d.group };
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
                                        if resp.clicked() {
                                            actions.push(Action::OpenChannel(d.id));
                                        }
                                    }
                                }
                            }
                        });
                    });
            });

        // --- заголовок канала ---
        egui::TopBottomPanel::top("channel_head")
            .frame(
                egui::Frame::none()
                    .fill(pal.main)
                    .inner_margin(Margin::symmetric(18.0, 12.0)),
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
                        if self.settings.show_ram {
                            chip(ui, &pal, &format!("ОЗУ {:.0} МБ", self.mem_mb), pal.muted);
                        }
                        if self.status != Status::Online {
                            chip(ui, &pal, "переподключение…", pal.warn);
                        }
                    });
                });
            });
        {
            // тонкая линия под заголовком
            let y = ctx.available_rect().top();
            let r = ctx.available_rect();
            ctx.layer_painter(egui::LayerId::background()).hline(
                r.left()..=r.right(),
                y,
                Stroke::new(1.0, pal.border),
            );
        }

        // --- поле ввода ---
        if let (Some(cid), Some((icon, name))) = (self.sel_channel, &title) {
            egui::TopBottomPanel::bottom("composer")
                .frame(egui::Frame::none().fill(pal.main).inner_margin(Margin {
                    left: 16.0,
                    right: 16.0,
                    top: 6.0,
                    bottom: 16.0,
                }))
                .show_separator_line(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
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
                                let hint = format!("Написать {icon}{name}");
                                let edit_width = (ui.available_width() - 44.0).max(60.0);
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.draft)
                                        .hint_text(hint)
                                        .frame(false)
                                        .desired_width(edit_width)
                                        .vertical_align(Align::Center),
                                );
                                let enter = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                });
        }

        // --- сообщения ---
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(pal.main))
            .show(ctx, |ui| {
                let Some(cid) = self.sel_channel else {
                    empty_state(ui, &pal, "Выберите канал или диалог слева");
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
                };
                ui.push_id(cid, |ui| {
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
                                            && p.reply.is_none()
                                            && m.reply.is_none()
                                            && snowflake_ms(m.id).saturating_sub(snowflake_ms(p.id))
                                                < 7 * 60 * 1000
                                    });
                                message_ui(ui, &mut mcx, m, grouped);
                                prev = Some(m);
                            }
                            ui.add_space(12.0);
                        });
                });
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
                    let _ = self.cmd.send(Cmd::Send {
                        channel_id,
                        content,
                    });
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
                Action::Logout => self.logout(None),
            }
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

        egui::Window::new(RichText::new("Настройки").strong())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(pal.side)
                    .inner_margin(Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(560.0)
                    .show(ui, |ui| {
                        ui.set_width(410.0);
                        section(ui, &pal, "Внешний вид");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.settings.theme, Theme::Dark, "Тёмная");
                            ui.selectable_value(
                                &mut self.settings.theme,
                                Theme::Midnight,
                                "Чёрная (AMOLED)",
                            );
                            ui.selectable_value(&mut self.settings.theme, Theme::Light, "Светлая");
                        });
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
                    });
            });

        self.settings_open = open;
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
            self.logout(None);
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
        if let Some((text, _, error)) = &self.toast {
            egui::Area::new(egui::Id::new("toast"))
                .anchor(Align2::CENTER_BOTTOM, [0.0, -96.0])
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(pal.elevated)
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
        self.refresh_memory(ctx);
        match self.screen {
            Screen::Login => self.login_ui(ctx),
            Screen::Main => self.main_ui(ctx),
        }
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
    let row = ui.horizontal_top(|ui| {
        ui.add_space(16.0);
        let gutter = if compact { 0.0 } else { 40.0 };
        if gutter > 0.0 {
            if grouped {
                ui.allocate_exact_size(vec2(gutter, 1.0), Sense::hover());
            } else {
                let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
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
            }
        }
        ui.vertical(|ui| {
            ui.set_max_width((full_width - gutter - 56.0).max(120.0));
            ui.spacing_mut().item_spacing.y = 3.0;
            if let Some(r) = &m.reply {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("↪ {}: {}", r.author, r.content))
                            .small()
                            .color(pal.muted),
                    )
                    .truncate(),
                );
            }
            if !grouped {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(m.author.as_str())
                            .strong()
                            .color(name_color(m.author_id)),
                    );
                    ui.label(RichText::new(m.time.as_str()).small().color(pal.faint));
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
                text_block(ui, &pal, &m.content);
            }
            if m.edited {
                ui.label(RichText::new("(изменено)").small().color(pal.faint));
            }
            for a in &m.attachments {
                attachment_ui(ui, cx, a);
            }
            for e in &m.embeds {
                embed_ui(ui, cx, e);
            }
        });
    });

    let rect = Rect::from_min_max(
        pos2(ui.max_rect().left(), row.response.rect.top() - 2.0),
        pos2(ui.max_rect().right(), row.response.rect.bottom() + 2.0),
    );
    if ui.rect_contains_pointer(rect) {
        ui.painter()
            .set(bg, egui::Shape::rect_filled(rect, 0.0, pal.msg_hover));
    }
}

/// Текст сообщения: блоки кода ``` и кликабельные ссылки.
fn text_block(ui: &mut egui::Ui, pal: &Palette, content: &str) {
    for (i, part) in content.split("```").enumerate() {
        if i % 2 == 1 {
            let code = part
                .split_once('\n')
                .map(|(lang, rest)| {
                    if lang.trim().contains(' ') {
                        part
                    } else {
                        rest
                    }
                })
                .unwrap_or(part);
            egui::Frame::none()
                .fill(pal.elevated)
                .rounding(6.0)
                .stroke(Stroke::new(1.0, pal.border))
                .inner_margin(Margin::same(10.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(code.trim_end_matches('\n'))
                            .monospace()
                            .color(pal.text),
                    );
                });
            continue;
        }
        let part = part.trim_matches('\n');
        if part.is_empty() {
            continue;
        }
        for line in part.split('\n') {
            if line.trim().is_empty() {
                ui.add_space(4.0);
                continue;
            }
            if !(line.contains("http://") || line.contains("https://")) {
                ui.label(RichText::new(line).color(pal.text));
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let mut text = String::new();
                for (j, word) in line.split(' ').enumerate() {
                    let sep = if j == 0 { "" } else { " " };
                    if word.starts_with("http://") || word.starts_with("https://") {
                        if !text.is_empty() || !sep.is_empty() {
                            text.push_str(sep);
                            ui.label(RichText::new(std::mem::take(&mut text)).color(pal.text));
                        }
                        ui.hyperlink_to(RichText::new(word).color(pal.link), word);
                    } else {
                        text.push_str(sep);
                        text.push_str(word);
                    }
                }
                if !text.is_empty() {
                    ui.label(RichText::new(text).color(pal.text));
                }
            });
        }
    }
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

fn attachment_ui(ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, a: &Attachment) {
    let pal = cx.pal;
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
    let rounding = if selected || hovered { 16.0 } else { 24.0 };
    let painter = ui.painter();

    let pill_h = if selected {
        38.0
    } else if hovered {
        20.0
    } else if unread {
        8.0
    } else {
        0.0
    };
    if pill_h > 0.0 {
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
            let fill = if selected || hovered {
                pal.accent
            } else {
                pal.elevated
            };
            painter.rect_filled(icon_rect, rounding, fill);
            let fg = if selected || hovered {
                Color32::WHITE
            } else {
                pal.text
            };
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
                let fill = if selected || hovered {
                    pal.accent
                } else {
                    pal.elevated
                };
                painter.rect_filled(icon_rect, rounding, fill);
                let fg = if selected || hovered {
                    Color32::WHITE
                } else {
                    pal.text
                };
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
    if selected {
        ui.painter().rect_filled(rect, 6.0, pal.selected);
    } else if hovered {
        ui.painter().rect_filled(rect, 6.0, pal.hover);
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
            .circle_filled(pos2(rect.right() - 12.0, cy), 4.0, pal.text);
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
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
    ui.painter().rect_filled(rect, size * 0.3, pal.accent);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "R",
        FontId::proportional(size * 0.55),
        Color32::WHITE,
    );
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
    ui.add_sized(
        [width, height],
        egui::Button::new(
            RichText::new(text)
                .strong()
                .size(15.5)
                .color(Color32::WHITE),
        )
        .fill(pal.accent)
        .rounding(10.0),
    )
}

fn round_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    glyph: &str,
    fg: Color32,
    bg: Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let fill = if resp.hovered() {
        bg.gamma_multiply(1.2)
    } else {
        bg
    };
    let _ = pal;
    ui.painter().circle_filled(rect.center(), 15.0, fill);
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
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.hover);
    }
    let color = if resp.hovered() { pal.text } else { pal.muted };
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
    matches!(kind, 0 | 1 | 3 | 5)
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
        .filter(|c| c.kind != 4 && c.parent == parent)
        .collect();
    v.sort_by_key(|c| (is_voice(c.kind), c.position, c.id));
    v
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
