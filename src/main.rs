//! Rustcord — лёгкий нативный клиент Discord на Rust.
//!
//! Почему он ест мало памяти:
//! * нет встроенного браузера (официальный клиент — Electron, т.е. Chromium + Node.js);
//!   интерфейс рисуется egui напрямую через OpenGL;
//! * из ответов Discord берутся только нужные поля (без аватаров, ролей, эмодзи и т.п.);
//! * в памяти хранятся сообщения максимум из MAX_CACHED_CHANNELS последних каналов,
//!   и не больше MAX_MESSAGES_PER_CHANNEL на канал;
//! * картинки и вложения не загружаются — показывается только имя файла;
//! * окно перерисовывается только при событиях, в простое процессор не грузится.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod discord;
mod store;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use tokio::sync::mpsc::UnboundedSender;

use discord::{Auth, Channel, Cmd, Dm, Event, Guild, Msg, Status, DISCORD_EPOCH};

const MAX_MESSAGES_PER_CHANNEL: usize = 200;
const MAX_CACHED_CHANNELS: usize = 6;
const MAX_MESSAGE_LEN: usize = 2000;

const ACCENT: Color32 = Color32::from_rgb(229, 116, 58);
const ONLINE: Color32 = Color32::from_rgb(63, 179, 107);
const WARN: Color32 = Color32::from_rgb(240, 178, 50);
const ERROR: Color32 = Color32::from_rgb(255, 130, 130);
const NAME_COLORS: [Color32; 8] = [
    Color32::from_rgb(229, 116, 58),
    Color32::from_rgb(63, 179, 107),
    Color32::from_rgb(110, 150, 240),
    Color32::from_rgb(210, 100, 170),
    Color32::from_rgb(220, 170, 60),
    Color32::from_rgb(70, 185, 200),
    Color32::from_rgb(160, 120, 225),
    Color32::from_rgb(235, 95, 95),
];

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustcord")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 460.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rustcord",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

// ======================= состояние приложения =======================

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
    LoadOlder(u64),
    Send(u64, String),
    Logout,
}

struct App {
    cmd: UnboundedSender<Cmd>,
    events: Receiver<Event>,
    screen: Screen,

    // вход
    token_input: String,
    bot_input: bool,
    remember: bool,
    login_error: Option<String>,
    logging_in: bool,
    pending: Option<Auth>,

    // сессия
    status: Status,
    me_id: u64,
    me_name: String,
    guilds: Vec<Guild>,
    dms: Vec<Dm>,
    sel_guild: Option<u64>,
    sel_channel: Option<u64>,
    last_channel: HashMap<Option<u64>, u64>,
    cache: HashMap<u64, ChannelCache>,
    lru: VecDeque<u64>,
    /// непрочитанный канал -> id сервера (0 для личных сообщений)
    unread: HashMap<u64, u64>,
    draft: String,

    // служебное
    toast: Option<(String, Instant)>,
    mem_mb: f64,
    mem_checked: Instant,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = ACCENT;
        visuals.hyperlink_color = Color32::from_rgb(108, 184, 245);
        cc.egui_ctx.set_visuals(visuals);

        let (cmd, events) = discord::spawn(cc.egui_ctx.clone());
        let mut app = Self {
            cmd,
            events,
            screen: Screen::Login,
            token_input: String::new(),
            bot_input: false,
            remember: true,
            login_error: None,
            logging_in: false,
            pending: None,
            status: Status::Connecting,
            me_id: 0,
            me_name: String::new(),
            guilds: Vec::new(),
            dms: Vec::new(),
            sel_guild: None,
            sel_channel: None,
            last_channel: HashMap::new(),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            unread: HashMap::new(),
            draft: String::new(),
            toast: None,
            mem_mb: 0.0,
            mem_checked: Instant::now() - Duration::from_secs(10),
        };
        if let Some((token, bot)) = store::load() {
            app.start_login(Auth { token, bot });
        }
        app
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
        self.start_login(Auth { token, bot });
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
        // Новые пустые коллекции, чтобы освободить память, а не просто очистить.
        self.guilds = Vec::new();
        self.dms = Vec::new();
        self.cache = HashMap::new();
        self.lru = VecDeque::new();
        self.unread = HashMap::new();
        self.last_channel = HashMap::new();
        self.sel_guild = None;
        self.sel_channel = None;
        self.pending = None;
        self.logging_in = false;
        self.draft.clear();
        self.token_input.clear();
        self.me_name.clear();
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
        while self.lru.len() > MAX_CACHED_CHANNELS {
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

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    // ---------- события из сети ----------

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev);
        }
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Status(s) => self.status = s,
            Event::Ready {
                user_id,
                username,
                guilds,
                dms,
            } => {
                self.status = Status::Online;
                self.me_id = user_id;
                self.me_name = username;
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
                // После (пере)подключения кэш мог устареть — загружаем заново.
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
            Event::Error(e) => self.toast(e),
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
                    },
                ),
                None => {}
            }
        }
        if m.author_id != self.me_id && self.sel_channel != Some(cid) {
            self.unread.insert(cid, m.guild_id.unwrap_or(0));
        }
        if let Some(c) = self.cache.get_mut(&cid) {
            if c.msgs.iter().rev().take(30).any(|x| x.id == m.id) {
                return; // уже есть (пришло и по REST, и по gateway)
            }
            let pos = c
                .msgs
                .iter()
                .rposition(|x| x.id < m.id)
                .map(|p| p + 1)
                .unwrap_or(0);
            c.msgs.insert(pos, m);
            while c.msgs.len() > MAX_MESSAGES_PER_CHANNEL {
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
        ctx.request_repaint_after(Duration::from_secs(2));
    }

    fn channel_title(&self) -> Option<(String, String)> {
        let id = self.sel_channel?;
        match self.sel_guild {
            Some(gid) => {
                let g = self.guilds.iter().find(|g| g.id == gid)?;
                let c = g.channels.iter().find(|c| c.id == id)?;
                Some((format!("# {}", c.name), format!("Написать в #{}", c.name)))
            }
            None => {
                let d = self.dms.iter().find(|d| d.id == id)?;
                Some((format!("@ {}", d.name), format!("Написать {}", d.name)))
            }
        }
    }

    // ======================= экраны =======================

    fn login_ui(&mut self, ctx: &egui::Context) {
        let mut submit = false;
        let mut cancel = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(50.0);
                    ui.label(RichText::new("Rustcord").size(34.0).strong().color(ACCENT));
                    ui.label("Лёгкий клиент Discord на Rust");
                    ui.add_space(26.0);

                    ui.label("Токен");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.token_input)
                            .password(true)
                            .hint_text("вставьте токен сюда")
                            .desired_width(360.0),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.bot_input, "Это токен бота");
                    ui.checkbox(&mut self.remember, "Запомнить на этом компьютере");
                    ui.add_space(10.0);

                    if self.logging_in {
                        ui.spinner();
                        let text = if self.status == Status::Reconnecting {
                            "Не удаётся подключиться, пробую ещё раз…"
                        } else {
                            "Подключение к Discord…"
                        };
                        ui.label(text);
                        if ui.button("Отмена").clicked() {
                            cancel = true;
                        }
                    } else if ui
                        .add_sized([360.0, 34.0], egui::Button::new(RichText::new("Войти").strong()))
                        .clicked()
                    {
                        submit = true;
                    }
                    if let Some(e) = &self.login_error {
                        ui.add_space(6.0);
                        ui.colored_label(ERROR, e);
                    }

                    ui.add_space(24.0);
                    ui.set_max_width(460.0);
                    egui::CollapsingHeader::new("Где взять токен бота (разрешённый способ)").show(ui, |ui| {
                        ui.label(
                            "1. Откройте discord.com/developers/applications → New Application.\n\
                             2. Вкладка Bot → Reset Token → скопируйте токен.\n\
                             3. Там же включите «Message Content Intent».\n\
                             4. OAuth2 → URL Generator: отметьте bot и права на чтение и отправку сообщений, \
                             откройте ссылку и добавьте бота на свой сервер.\n\
                             5. Вставьте токен сюда и отметьте «Это токен бота».",
                        );
                    });
                    egui::CollapsingHeader::new("Где взять токен своего аккаунта").show(ui, |ui| {
                        ui.label(
                            "Откройте discord.com/app в браузере и войдите. Нажмите F12 → вкладка Network (Сеть), \
                             обновите страницу, кликните любой запрос к discord.com/api и найдите в заголовках \
                             запроса строку authorization — это токен.\n\n\
                             Никому не передавайте токен: с ним можно войти в ваш аккаунт без пароля.",
                        );
                    });
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(
                            "⚠ Вход токеном обычного аккаунта в сторонний клиент нарушает правила Discord — \
                             аккаунт могут заблокировать. С токеном бота это разрешено.",
                        )
                        .small()
                        .color(WARN),
                    );
                    ui.add_space(14.0);
                    ui.label(RichText::new(format!("ОЗУ: {:.0} МБ", self.mem_mb)).small().weak());
                });
            });
        });
        if cancel {
            self.logout(None);
        } else if submit && !self.logging_in {
            self.login_from_input();
        }
    }

    fn main_ui(&mut self, ctx: &egui::Context) {
        let mut actions: Vec<Action> = Vec::new();
        let unread_guilds: HashSet<u64> = self.unread.values().copied().collect();
        let title = self.channel_title();

        // --- строка состояния (как F3 в майнкрафте: видно, сколько ест) ---
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (color, text) = match self.status {
                    Status::Online => (ONLINE, "в сети"),
                    Status::Connecting => (WARN, "подключение…"),
                    Status::Reconnecting => (WARN, "переподключение…"),
                };
                ui.colored_label(color, "●");
                ui.label(format!("{} · {}", self.me_name, text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Выйти").clicked() {
                        actions.push(Action::Logout);
                    }
                    let cached: usize = self.cache.values().map(|c| c.msgs.len()).sum();
                    ui.label(
                        RichText::new(format!(
                            "ОЗУ: {:.0} МБ · в кэше {} сообщ.",
                            self.mem_mb, cached
                        ))
                        .monospace()
                        .weak(),
                    );
                });
            });
        });

        // --- полоса серверов ---
        egui::SidePanel::left("rail")
            .resizable(false)
            .exact_width(72.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        if rail_button(
                            ui,
                            "ЛС",
                            "Личные сообщения",
                            self.sel_guild.is_none(),
                            unread_guilds.contains(&0),
                        )
                        .clicked()
                        {
                            actions.push(Action::SelectGuild(None));
                        }
                        ui.separator();
                        for g in &self.guilds {
                            let selected = self.sel_guild == Some(g.id);
                            if rail_button(
                                ui,
                                &initials(&g.name),
                                &g.name,
                                selected,
                                unread_guilds.contains(&g.id),
                            )
                            .clicked()
                            {
                                actions.push(Action::SelectGuild(Some(g.id)));
                            }
                        }
                    });
                });
            });

        // --- каналы / диалоги ---
        egui::SidePanel::left("channels")
            .resizable(false)
            .exact_width(230.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                let guild = self
                    .sel_guild
                    .and_then(|id| self.guilds.iter().find(|g| g.id == id));
                match guild {
                    Some(g) => {
                        ui.label(RichText::new(&g.name).strong().size(16.0));
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                for row in channel_rows(g) {
                                    match row {
                                        Row::Category(name) => {
                                            ui.add_space(8.0);
                                            ui.label(
                                                RichText::new(name.to_uppercase())
                                                    .small()
                                                    .strong()
                                                    .weak(),
                                            );
                                        }
                                        Row::Chan(c) if is_text(c.kind) => {
                                            let mut text = RichText::new(format!("# {}", c.name));
                                            if self.unread.contains_key(&c.id) {
                                                text = text.strong().color(Color32::WHITE);
                                            }
                                            if ui
                                                .selectable_label(
                                                    self.sel_channel == Some(c.id),
                                                    text,
                                                )
                                                .clicked()
                                            {
                                                actions.push(Action::OpenChannel(c.id));
                                            }
                                        }
                                        Row::Chan(c) if is_voice(c.kind) => {
                                            ui.label(
                                                RichText::new(format!("🔊 {}", c.name)).weak(),
                                            );
                                        }
                                        Row::Chan(c) if c.kind == 15 || c.kind == 16 => {
                                            ui.label(
                                                RichText::new(format!("💬 {} (форум)", c.name))
                                                    .weak(),
                                            );
                                        }
                                        Row::Chan(_) => {}
                                    }
                                }
                            });
                    }
                    None => {
                        ui.label(RichText::new("Личные сообщения").strong().size(16.0));
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                if self.dms.is_empty() {
                                    ui.label(RichText::new("Диалогов пока нет").weak());
                                }
                                for d in &self.dms {
                                    let mut text = RichText::new(&d.name);
                                    if self.unread.contains_key(&d.id) {
                                        text = text.strong().color(Color32::WHITE);
                                    }
                                    if ui
                                        .selectable_label(self.sel_channel == Some(d.id), text)
                                        .clicked()
                                    {
                                        actions.push(Action::OpenChannel(d.id));
                                    }
                                }
                            });
                    }
                }
            });

        // --- поле ввода ---
        if let (Some(cid), Some((_, hint))) = (self.sel_channel, &title) {
            egui::TopBottomPanel::bottom("composer").show(ctx, |ui| {
                ui.add_space(6.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.draft)
                        .hint_text(hint.as_str())
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let content = self.draft.trim().to_string();
                    if content.chars().count() > MAX_MESSAGE_LEN {
                        self.toast = Some((
                            format!("Сообщение длиннее {MAX_MESSAGE_LEN} символов"),
                            Instant::now(),
                        ));
                    } else if !content.is_empty() {
                        actions.push(Action::Send(cid, content));
                        self.draft.clear();
                    }
                    resp.request_focus();
                }
                ui.add_space(6.0);
            });
        }

        // --- заголовок канала ---
        egui::TopBottomPanel::top("channel_head").show(ctx, |ui| {
            ui.add_space(6.0);
            let text = title
                .as_ref()
                .map(|(t, _)| t.as_str())
                .unwrap_or("Rustcord");
            ui.label(RichText::new(text).strong().size(16.0));
            ui.add_space(6.0);
        });

        // --- сообщения ---
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(cid) = self.sel_channel else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Выберите канал или диалог слева").weak());
                });
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
            if let Some(e) = &cache.error {
                ui.colored_label(ERROR, e);
            }
            ui.push_id(cid, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if cache.has_more {
                            if cache.loading {
                                ui.spinner();
                            } else if ui.button("Загрузить более ранние сообщения").clicked()
                            {
                                actions.push(Action::LoadOlder(cid));
                            }
                        } else if cache.error.is_none() {
                            ui.label(RichText::new("Это начало переписки").weak());
                        }
                        let mut prev: Option<&Msg> = None;
                        for m in &cache.msgs {
                            draw_message(ui, m, prev);
                            prev = Some(m);
                        }
                        ui.add_space(8.0);
                    });
            });
        });

        for action in actions {
            match action {
                Action::SelectGuild(g) => self.select_guild(g),
                Action::OpenChannel(id) => self.open_channel(id),
                Action::LoadOlder(id) => self.load_older(id),
                Action::Send(channel_id, content) => {
                    let _ = self.cmd.send(Cmd::Send {
                        channel_id,
                        content,
                    });
                }
                Action::Logout => self.logout(None),
            }
        }
    }

    fn toast_ui(&mut self, ctx: &egui::Context) {
        let expired = self
            .toast
            .as_ref()
            .map_or(false, |(_, at)| at.elapsed() > Duration::from_secs(4));
        if expired {
            self.toast = None;
        }
        if let Some((text, _)) = &self.toast {
            egui::Window::new("toast")
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -80.0])
                .show(ctx, |ui| {
                    ui.label(text.as_str());
                });
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.refresh_memory(ctx);
        match self.screen {
            Screen::Login => self.login_ui(ctx),
            Screen::Main => self.main_ui(ctx),
        }
        self.toast_ui(ctx);
    }
}

// ======================= вспомогательное =======================

fn is_text(kind: u8) -> bool {
    matches!(kind, 0 | 1 | 3 | 5)
}

fn is_voice(kind: u8) -> bool {
    matches!(kind, 2 | 13)
}

fn initials(name: &str) -> String {
    let s: String = name
        .split_whitespace()
        .take(2)
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
    NAME_COLORS[(id % NAME_COLORS.len() as u64) as usize]
}

enum Row<'a> {
    Category(&'a str),
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
        rows.push(Row::Category(&cat.name));
        rows.extend(kids.into_iter().map(Row::Chan));
    }
    rows
}

fn rail_button(
    ui: &mut egui::Ui,
    label: &str,
    tooltip: &str,
    selected: bool,
    unread: bool,
) -> egui::Response {
    let resp = ui
        .add(
            egui::Button::new(RichText::new(label).strong().size(15.0))
                .min_size(egui::vec2(48.0, 48.0))
                .selected(selected),
        )
        .on_hover_text(tooltip);
    if unread && !selected {
        ui.painter()
            .circle_filled(resp.rect.right_top() + egui::vec2(-5.0, 5.0), 4.0, ACCENT);
    }
    ui.add_space(4.0);
    resp
}

fn draw_message(ui: &mut egui::Ui, m: &Msg, prev: Option<&Msg>) {
    let grouped = prev.map_or(false, |p| {
        p.author_id == m.author_id
            && snowflake_ms(m.id).saturating_sub(snowflake_ms(p.id)) < 5 * 60 * 1000
    });
    if !grouped {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(m.author.as_str())
                    .strong()
                    .color(name_color(m.author_id)),
            );
            ui.label(RichText::new(m.time.as_str()).small().weak());
        });
    }
    ui.label(m.content.as_str());
    if m.edited {
        ui.label(RichText::new("(изменено)").small().weak());
    }
}
