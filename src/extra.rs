//! Остальной функционал Discord: разметка сообщений, профили, статусы, друзья,
//! закреплённые сообщения, поиск, ветки, быстрый переход (Ctrl+K), меню по правому
//! клику, приглашения и серверы, уведомления.

use super::*;
use crate::markdown::{self, Block, Inline, LineKind};
use egui::text::{LayoutJob, TextFormat};
use serde_json::Value;

// ======================= состояние окон =======================

pub struct ProfileView {
    pub user_id: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub name: String,
    pub username: String,
    pub avatar: Option<String>,
    pub banner: Option<String>,
    pub accent: Option<u32>,
    pub bio: String,
    pub pronouns: String,
    pub roles: Vec<String>,
    pub mutual: usize,
    pub bot: bool,
}

pub struct SearchView {
    pub query: String,
    pub loading: bool,
    pub total: u64,
    pub results: Option<Result<Vec<Msg>, String>>,
}

pub fn presence_color(pal: &Palette, status: &str) -> Option<Color32> {
    match status {
        "online" => Some(pal.ok),
        "idle" => Some(pal.warn),
        "dnd" => Some(pal.danger),
        "invisible" | "offline" => Some(pal.faint),
        _ => None,
    }
}

pub fn presence_text(status: &str) -> &'static str {
    match status {
        "online" => "В сети",
        "idle" => "Не активен",
        "dnd" => "Не беспокоить",
        "invisible" => "Невидимый",
        _ => "Не в сети",
    }
}

/// Кружок статуса поверх аватарки.
pub fn presence_dot(
    ui: &egui::Ui,
    pal: &Palette,
    center: egui::Pos2,
    r: f32,
    status: &str,
    ring: Color32,
) {
    let Some(c) = presence_color(pal, status) else {
        return;
    };
    ui.painter().circle_filled(center, r + 2.0, ring);
    ui.painter().circle_filled(center, r, c);
    match status {
        "idle" => {
            ui.painter()
                .circle_filled(center - vec2(r * 0.35, r * 0.35), r * 0.62, ring);
        }
        "dnd" => {
            ui.painter().rect_filled(
                Rect::from_center_size(center, vec2(r * 1.2, r * 0.42)),
                1.0,
                ring,
            );
        }
        "invisible" | "offline" => {
            ui.painter().circle_filled(center, r * 0.5, ring);
        }
        _ => {}
    }
}

/// Красная плашка с числом упоминаний.
pub fn badge(ui: &egui::Ui, center: egui::Pos2, count: u32, ring: Color32) {
    if count == 0 {
        return;
    }
    let text = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text, FontId::proportional(11.5), Color32::WHITE);
    let w = (galley.size().x + 10.0).max(18.0);
    let r = Rect::from_center_size(center, vec2(w, 18.0));
    ui.painter().rect_filled(r.expand(2.5), 11.0, ring);
    ui.painter()
        .rect_filled(r, 9.0, Color32::from_rgb(237, 66, 69));
    ui.painter()
        .galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

fn role_color(c: u32, fallback: Color32) -> Color32 {
    if c == 0 {
        fallback
    } else {
        Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
    }
}

/// Строка-код приглашения из ссылки (discord.gg/abc, discord.com/invite/abc).
fn invite_code(input: &str) -> String {
    let t = input.trim().trim_end_matches('/');
    let t = t.split(['?', '#']).next().unwrap_or(t);
    t.rsplit('/').next().unwrap_or(t).to_string()
}

// ======================= разметка сообщений =======================

/// Рисует текст сообщения с разметкой Discord.
pub fn rich_text(
    ui: &mut egui::Ui,
    cx: &mut MsgCtx<'_>,
    content: &str,
    mentions: &[(u64, String)],
) {
    let pal = cx.pal;
    let jumbo = emoji::is_jumbo(content);
    let spoiler_id = ui.id().with(("spoiler", egui::Id::new(content)));
    let revealed = ui.data(|d| d.get_temp::<bool>(spoiler_id)).unwrap_or(false);
    let mut reveal = false;
    for block in markdown::blocks(content) {
        match block {
            Block::Blank => ui.add_space(4.0),
            Block::Code { lang, code } => {
                egui::Frame::none()
                    .fill(pal.elevated)
                    .rounding(6.0)
                    .stroke(Stroke::new(1.0, pal.border))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        if !lang.is_empty() {
                            ui.label(RichText::new(lang).size(10.5).color(pal.faint));
                        }
                        ui.add(
                            egui::Label::new(RichText::new(code).monospace().color(pal.text))
                                .wrap(),
                        );
                    });
            }
            Block::Line {
                kind,
                number,
                quoted,
                text,
            } => {
                let draw = |ui: &mut egui::Ui, cx: &mut MsgCtx<'_>, reveal: &mut bool| match kind {
                    LineKind::Bullet(indent) => {
                        ui.horizontal_top(|ui| {
                            ui.add_space(4.0 + indent as f32 * 18.0);
                            let dot = if indent == 0 { "•" } else { "◦" };
                            ui.label(RichText::new(dot).color(pal.muted));
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                rich_line(ui, cx, text, kind, mentions, jumbo, revealed, reveal)
                            });
                        });
                    }
                    LineKind::Numbered => {
                        ui.horizontal_top(|ui| {
                            ui.add_space(4.0);
                            ui.label(RichText::new(number).color(pal.muted));
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                rich_line(ui, cx, text, kind, mentions, jumbo, revealed, reveal)
                            });
                        });
                    }
                    _ => rich_line(ui, cx, text, kind, mentions, jumbo, revealed, reveal),
                };
                if quoted {
                    let r = ui.horizontal_top(|ui| {
                        ui.add_space(12.0);
                        ui.vertical(|ui| draw(ui, cx, &mut reveal));
                    });
                    let rr = r.response.rect;
                    ui.painter().rect_filled(
                        Rect::from_min_size(rr.left_top(), vec2(4.0, rr.height())),
                        2.0,
                        pal.faint.gamma_multiply(0.7),
                    );
                } else {
                    draw(ui, cx, &mut reveal);
                }
            }
        }
    }
    if reveal {
        ui.data_mut(|d| d.insert_temp(spoiler_id, true));
    }
}

#[allow(clippy::too_many_arguments)]
fn rich_line(
    ui: &mut egui::Ui,
    cx: &mut MsgCtx<'_>,
    text: &str,
    kind: LineKind,
    mentions: &[(u64, String)],
    jumbo: bool,
    revealed: bool,
    reveal: &mut bool,
) {
    let pal = cx.pal;
    let (size, bold_line, color) = match kind {
        LineKind::H1 => (24.0, true, pal.text),
        LineKind::H2 => (20.0, true, pal.text),
        LineKind::H3 => (17.0, true, pal.text),
        LineKind::Sub => (12.0, false, pal.faint),
        _ => (14.5, false, pal.text),
    };
    if kind == LineKind::H1 || kind == LineKind::H2 {
        ui.add_space(4.0);
    }
    if markdown::is_plain(text) {
        let family = if bold_line {
            egui::FontFamily::Name("bold".into())
        } else {
            egui::FontFamily::Proportional
        };
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .font(FontId::new(size, family))
                    .color(color),
            )
            .wrap(),
        );
        return;
    }
    let emoji_size = if jumbo { 44.0 } else { size * 1.35 };
    let spans = markdown::inline(text);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut job = LayoutJob::default();
        let flush = |ui: &mut egui::Ui, job: &mut LayoutJob| {
            if !job.sections.is_empty() {
                let j = std::mem::take(job);
                ui.add(egui::Label::new(j).wrap());
            }
        };
        let format = |st: markdown::Style, mono: bool| -> TextFormat {
            let family = if mono {
                egui::FontFamily::Monospace
            } else if st.bold || bold_line {
                egui::FontFamily::Name("bold".into())
            } else {
                egui::FontFamily::Proportional
            };
            let mut f = TextFormat {
                font_id: FontId::new(if mono { size - 1.0 } else { size }, family),
                color,
                italics: st.italic,
                ..Default::default()
            };
            if st.underline {
                f.underline = Stroke::new(1.0, color);
            }
            if st.strike {
                f.strikethrough = Stroke::new(1.0, color);
            }
            if mono {
                f.background = pal.elevated;
            }
            if st.spoiler && !revealed {
                f.color = pal.faint.gamma_multiply(0.0);
                f.background = if pal.light {
                    Color32::from_gray(190)
                } else {
                    Color32::from_gray(20)
                };
            } else if st.spoiler {
                f.background = pal.elevated;
            }
            f
        };
        for span in &spans {
            let st = span.style();
            let hidden = st.spoiler && !revealed;
            match span {
                Inline::Text(t, _) if hidden => {
                    flush(ui, &mut job);
                    let mut j = LayoutJob::default();
                    j.append(t, 0.0, format(st, false));
                    let r = ui
                        .add(egui::Label::new(j).sense(Sense::click()))
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .on_hover_text("Спойлер — нажмите, чтобы показать");
                    if r.clicked() {
                        *reveal = true;
                    }
                }
                Inline::Text(t, _) => job.append(t, 0.0, format(st, false)),
                Inline::Code(t, _) => job.append(t, 0.0, format(st, true)),
                _ if hidden => {
                    // упоминания и ссылки внутри спойлера — тоже скрыты
                    flush(ui, &mut job);
                    let r = ui
                        .add(
                            egui::Label::new(
                                RichText::new("▮▮▮▮")
                                    .size(size)
                                    .color(Color32::from_gray(20))
                                    .background_color(Color32::from_gray(20)),
                            )
                            .sense(Sense::click()),
                        )
                        .on_hover_cursor(CursorIcon::PointingHand);
                    if r.clicked() {
                        *reveal = true;
                    }
                }
                Inline::Link { label, url, .. } => {
                    flush(ui, &mut job);
                    let r = ui.hyperlink_to(RichText::new(*label).size(size).color(pal.link), *url);
                    if label != url {
                        r.on_hover_text(*url);
                    }
                }
                Inline::Emoji(e, _) => {
                    flush(ui, &mut job);
                    emoji_image(ui, cx, &Emoji::unicode(e), emoji_size);
                }
                Inline::Custom {
                    name, id, animated, ..
                } => {
                    flush(ui, &mut job);
                    let em = Emoji {
                        id: Some(*id),
                        name: name.to_string(),
                        animated: *animated,
                    };
                    emoji_image(ui, cx, &em, emoji_size).on_hover_text(format!(":{name}:"));
                }
                Inline::User(id, _) => {
                    flush(ui, &mut job);
                    let name = mentions
                        .iter()
                        .find(|(u, _)| u == id)
                        .map(|(_, n)| n.clone())
                        .or_else(|| cx.users.get(id).cloned())
                        .unwrap_or_else(|| "неизвестный".into());
                    let me = *id == cx.me_id;
                    let fg = if me { pal.warn } else { pal.accent };
                    if mention_pill(ui, &format!("@{name}"), fg, size).clicked() {
                        cx.actions.push(Action::OpenProfile(*id));
                    }
                }
                Inline::Role(id, _) => {
                    flush(ui, &mut job);
                    let role = cx
                        .guild_id
                        .and_then(|g| cx.guilds.iter().find(|x| x.id == g))
                        .and_then(|g| g.roles.get(&id.to_string()));
                    let (name, col) = match role {
                        Some((n, c)) => (n.clone(), role_color(*c, pal.accent)),
                        None => ("роль".into(), pal.accent),
                    };
                    mention_pill(ui, &format!("@{name}"), col, size);
                }
                Inline::Channel(id, _) => {
                    flush(ui, &mut job);
                    let found = cx.guilds.iter().find_map(|g| {
                        g.channels
                            .iter()
                            .find(|c| c.id == *id)
                            .map(|c| (g.id, c.name.clone(), c.kind))
                    });
                    let label = match &found {
                        Some((_, n, k)) if is_voice(*k) => format!("🔊 {n}"),
                        Some((_, n, _)) => format!("# {n}"),
                        None => "# неизвестный-канал".into(),
                    };
                    if mention_pill(ui, &label, pal.accent, size).clicked() {
                        if let Some((g, _, _)) = found {
                            cx.actions.push(Action::Jump(Some(g), *id));
                        }
                    }
                }
                Inline::Everyone(t, _) => {
                    flush(ui, &mut job);
                    mention_pill(ui, t, pal.warn, size);
                }
                Inline::Time(secs, fmt, _) => {
                    flush(ui, &mut job);
                    let label = markdown::format_time(*secs, *fmt);
                    let full = markdown::format_time(*secs, 'F');
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!(" {label} "))
                                .size(size)
                                .color(pal.text)
                                .background_color(pal.elevated),
                        )
                        .sense(Sense::hover()),
                    )
                    .on_hover_text(full);
                    if *fmt == 'R' {
                        ui.ctx().request_repaint_after(Duration::from_secs(30));
                    }
                }
            }
        }
        flush(ui, &mut job);
    });
}

fn mention_pill(ui: &mut egui::Ui, text: &str, fg: Color32, size: f32) -> egui::Response {
    let r = ui.add(
        egui::Label::new(
            RichText::new(text)
                .size(size)
                .color(fg)
                .background_color(fg.gamma_multiply(0.22)),
        )
        .sense(Sense::click()),
    );
    r.on_hover_cursor(CursorIcon::PointingHand)
}

// ======================= окна =======================

fn window_frame(ctx: &egui::Context, pal: &Palette, solid: Color32) -> egui::Frame {
    egui::Frame::window(&ctx.style())
        .fill(solid)
        .rounding(pal.radius + 6.0)
        .inner_margin(Margin::same(18.0))
}

fn section_title(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).size(11.5).strong().color(pal.muted));
    ui.add_space(3.0);
}

fn plain_preview(content: &str, n: usize) -> String {
    let mut out = String::new();
    for span in markdown::inline(&content.replace('\n', " ")) {
        match span {
            Inline::Text(t, s) | Inline::Code(t, s) => {
                if s.spoiler {
                    out.push_str("▮▮▮");
                } else {
                    out.push_str(t);
                }
            }
            Inline::Link { label, .. } => out.push_str(label),
            Inline::Emoji(e, _) => out.push_str(e),
            Inline::Custom { name, .. } => {
                out.push(':');
                out.push_str(name);
                out.push(':');
            }
            Inline::User(..) => out.push_str("@…"),
            Inline::Role(..) => out.push_str("@роль"),
            Inline::Channel(..) => out.push_str("#канал"),
            Inline::Everyone(t, _) => out.push_str(t),
            Inline::Time(s, f, _) => out.push_str(&markdown::format_time(s, f)),
        }
    }
    out.chars().take(n).collect()
}

impl App {
    fn api(&self, method: &'static str, path: String, body: Option<Value>, tag: discord::ApiTag) {
        let _ = self.cmd.send(Cmd::Api {
            method,
            path,
            body,
            tag,
        });
    }

    fn is_bot(&self) -> bool {
        self.pending.as_ref().map_or(false, |a| a.bot)
    }

    fn avatar_tex(
        &mut self,
        ctx: &egui::Context,
        id: u64,
        hash: &Option<String>,
        size: u32,
    ) -> Option<(TextureId, Vec2)> {
        if !self.settings.load_avatars {
            return None;
        }
        let h = hash.as_ref()?;
        let url = discord::avatar_url(id, h);
        let url = if size > 64 {
            url.replace("size=64", &format!("size={size}"))
        } else {
            url
        };
        self.media.get(ctx, &self.cmd, &url, &url, size, false)
    }

    /// Ответы на универсальные REST-запросы.
    pub(crate) fn on_api(
        &mut self,
        ctx: &egui::Context,
        tag: discord::ApiTag,
        result: Result<String, String>,
    ) {
        use discord::ApiTag;
        match tag {
            ApiTag::Profile(uid) => {
                let Some(p) = self.profile.as_mut().filter(|p| p.user_id == uid) else {
                    return;
                };
                p.loading = false;
                match result {
                    Err(e) => p.error = Some(e),
                    Ok(body) => {
                        let v: Value = serde_json::from_str(&body).unwrap_or_default();
                        // /users/{id} отдаёт пользователя сразу, /profile — внутри "user"
                        let u = if v.get("user").is_some() {
                            &v["user"]
                        } else {
                            &v
                        };
                        let s =
                            |x: &Value| x.as_str().filter(|t| !t.is_empty()).map(str::to_string);
                        p.username = s(&u["username"]).unwrap_or_default();
                        if let Some(n) = s(&v["guild_member"]["nick"])
                            .or_else(|| s(&u["global_name"]))
                            .or_else(|| s(&u["username"]))
                        {
                            p.name = n;
                        }
                        p.avatar = s(&u["avatar"]).or(p.avatar.take());
                        p.banner = s(&u["banner"]);
                        p.accent = u["accent_color"]
                            .as_u64()
                            .or(v["user_profile"]["accent_color"].as_u64())
                            .map(|c| c as u32);
                        p.bio = s(&v["guild_member_profile"]["bio"])
                            .or_else(|| s(&v["user_profile"]["bio"]))
                            .or_else(|| s(&u["bio"]))
                            .unwrap_or_default();
                        p.pronouns = s(&v["guild_member_profile"]["pronouns"])
                            .or_else(|| s(&v["user_profile"]["pronouns"]))
                            .unwrap_or_default();
                        p.roles = v["guild_member"]["roles"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|r| r.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        p.mutual = v["mutual_guilds"].as_array().map_or(0, |a| a.len());
                        p.bot = u["bot"].as_bool().unwrap_or(false);
                        self.users.insert(uid, p.name.clone());
                    }
                }
            }
            ApiTag::Pins(cid) => {
                if self.pins.as_ref().map_or(false, |(c, _)| *c == cid) {
                    self.pins = Some((cid, Some(result.map(|b| discord::parse_messages(&b)))));
                }
            }
            ApiTag::Search => {
                if let Some(sv) = &mut self.search {
                    sv.loading = false;
                    match result {
                        Ok(b) => {
                            let (total, msgs) = discord::parse_search(&b);
                            sv.total = total;
                            sv.results = Some(Ok(msgs));
                        }
                        Err(e) => sv.results = Some(Err(e)),
                    }
                }
            }
            ApiTag::Threads(parent) => {
                if self.threads.as_ref().map_or(false, |(p, _)| *p == parent) {
                    self.threads = Some((parent, Some(result.map(|b| discord::parse_threads(&b)))));
                }
            }
            ApiTag::OpenDm => match result.map(|b| discord::parse_dm(&b)) {
                Ok(Some(dm)) => {
                    let id = dm.id;
                    if !self.dms.iter().any(|d| d.id == id) {
                        self.dms.insert(0, dm);
                    }
                    self.profile = None;
                    self.sel_guild = None;
                    self.open_channel(id);
                }
                Ok(None) => self.toast("Не удалось открыть диалог", true),
                Err(e) => self.toast(format!("Не удалось открыть диалог: {e}"), true),
            },
            ApiTag::Invite => match result {
                Ok(b) => {
                    let v: Value = serde_json::from_str(&b).unwrap_or_default();
                    if let Some(code) = v["code"].as_str() {
                        let link = format!("https://discord.gg/{code}");
                        ctx.copy_text(link.clone());
                        self.toast(format!("Приглашение скопировано: {link}"), false);
                    }
                }
                Err(e) => self.toast(format!("Не удалось создать приглашение: {e}"), true),
            },
            ApiTag::Done(text) => match result {
                Ok(_) => self.toast(text, false),
                Err(e) => self.toast(e, true),
            },
            ApiTag::Silent => {
                if let Err(e) = result {
                    self.toast(e, true);
                }
            }
        }
    }

    /// Упоминания, значки и системные уведомления о новом сообщении.
    pub(crate) fn notify_message(&mut self, m: &Msg) {
        if m.author_id == self.me_id || self.me_id == 0 {
            return;
        }
        let dm = m.guild_id.is_none();
        let mentioned = m.mention_everyone || m.mentions.iter().any(|(id, _)| *id == self.me_id);
        if !dm && !mentioned {
            return;
        }
        let focused = self.window_focused;
        if self.sel_channel == Some(m.channel_id) && focused {
            return;
        }
        *self.mentions.entry(m.channel_id).or_default() += 1;
        if self.my_status == "dnd" || !self.settings.notifications || focused {
            return;
        }
        let place = match m.guild_id {
            None => String::new(),
            Some(g) => self
                .guilds
                .iter()
                .find(|x| x.id == g)
                .map(|x| {
                    let ch = x
                        .channels
                        .iter()
                        .find(|c| c.id == m.channel_id)
                        .map(|c| c.name.as_str())
                        .unwrap_or("");
                    format!(" (#{ch}, {})", x.name)
                })
                .unwrap_or_default(),
        };
        let title = format!("{}{place}", m.author);
        let body = if m.content.is_empty() {
            if m.attachments.is_empty() {
                "Новое сообщение".to_string()
            } else {
                "📎 Вложение".to_string()
            }
        } else {
            plain_preview(&m.content, 180)
        };
        self.attention = true;
        std::thread::spawn(move || {
            let _ = notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .appname("Rustcord")
                .show();
        });
    }

    // ---------- действия ----------

    pub(crate) fn extra_action(&mut self, ctx: &egui::Context, action: Action) {
        use discord::ApiTag;
        match action {
            Action::OpenProfile(uid) => {
                let (name, avatar) = self.known_user(uid);
                self.profile = Some(ProfileView {
                    user_id: uid,
                    loading: true,
                    error: None,
                    name,
                    username: String::new(),
                    avatar,
                    banner: None,
                    accent: None,
                    bio: String::new(),
                    pronouns: String::new(),
                    roles: Vec::new(),
                    mutual: 0,
                    bot: false,
                });
                let path = if self.is_bot() {
                    format!("/users/{uid}")
                } else {
                    match self.sel_guild {
                        Some(g) => {
                            format!("/users/{uid}/profile?with_mutual_guilds=true&guild_id={g}")
                        }
                        None => format!("/users/{uid}/profile?with_mutual_guilds=true"),
                    }
                };
                self.api("GET", path, None, ApiTag::Profile(uid));
            }
            Action::MessageUser(uid) => {
                let existing = self
                    .dms
                    .iter()
                    .find(|d| !d.group && d.recipients.len() == 1 && d.recipients[0].0 == uid)
                    .map(|d| d.id);
                match existing {
                    Some(id) => {
                        self.profile = None;
                        self.sel_guild = None;
                        self.open_channel(id);
                    }
                    None => self.api(
                        "POST",
                        "/users/@me/channels".into(),
                        Some(serde_json::json!({ "recipients": [uid.to_string()] })),
                        ApiTag::OpenDm,
                    ),
                }
            }
            Action::OpenStatusMenu => self.status_menu = !self.status_menu,
            Action::SetStatus(st) => {
                self.my_status = st.clone();
                self.settings.status = st;
                settings::save(&self.settings);
                let _ = self.cmd.send(Cmd::SetPresence {
                    status: self.settings.status.clone(),
                    custom: self.settings.custom_status.clone(),
                });
            }
            Action::OpenFriends => {
                self.sel_guild = None;
                self.sel_channel = None;
                self.friends_open = true;
            }
            Action::FriendAdd(name) => {
                let name = name.trim().trim_start_matches('@').to_lowercase();
                if name.is_empty() {
                    return;
                }
                self.api(
                    "POST",
                    "/users/@me/relationships".into(),
                    Some(serde_json::json!({ "username": name, "discriminator": null })),
                    ApiTag::Done(format!("Заявка в друзья отправлена: {name}")),
                );
                self.friend_input.clear();
            }
            Action::FriendAccept(id) => self.api(
                "PUT",
                format!("/users/@me/relationships/{id}"),
                Some(serde_json::json!({})),
                ApiTag::Done("Заявка принята — теперь вы друзья".into()),
            ),
            Action::FriendRequest(id) => self.api(
                "PUT",
                format!("/users/@me/relationships/{id}"),
                Some(serde_json::json!({})),
                ApiTag::Done("Заявка в друзья отправлена".into()),
            ),
            Action::FriendRemove(id) => self.api(
                "DELETE",
                format!("/users/@me/relationships/{id}"),
                None,
                ApiTag::Silent,
            ),
            Action::FriendBlock(id) => self.api(
                "PUT",
                format!("/users/@me/relationships/{id}"),
                Some(serde_json::json!({ "type": 2 })),
                ApiTag::Done("Пользователь заблокирован".into()),
            ),
            Action::CopyId(id) => {
                ctx.copy_text(id.to_string());
                self.toast("ID скопирован", false);
            }
            Action::GuildMenu(g, pos) => {
                self.guild_menu = Some((g, pos));
                self.menu_frame = ctx.frame_nr();
            }
            Action::AskLeave(g) => self.confirm_leave = Some(g),
            Action::LeaveGuild(g) => {
                self.confirm_leave = None;
                self.api(
                    "DELETE",
                    format!("/users/@me/guilds/{g}"),
                    Some(serde_json::json!({ "lurking": false })),
                    ApiTag::Done("Вы покинули сервер".into()),
                );
            }
            Action::CopyInvite(g) => {
                let channel = self.guilds.iter().find(|x| x.id == g).and_then(|x| {
                    let last = self.last_channel.get(&Some(g)).copied();
                    last.filter(|id| x.channels.iter().any(|c| c.id == *id && is_text(c.kind)))
                        .or_else(|| x.channels.iter().find(|c| c.kind == 0).map(|c| c.id))
                });
                match channel {
                    Some(c) => self.api(
                        "POST",
                        format!("/channels/{c}/invites"),
                        Some(serde_json::json!({ "max_age": 604800, "max_uses": 0, "temporary": false })),
                        ApiTag::Invite,
                    ),
                    None => self.toast("На сервере нет текстового канала для приглашения", true),
                }
            }
            Action::OpenServerDialog => self.server_dialog = Some((0, String::new())),
            Action::JoinInvite(link) => {
                let code = invite_code(&link);
                if code.is_empty() {
                    return;
                }
                self.server_dialog = None;
                self.api(
                    "POST",
                    format!("/invites/{}", discord::percent_encode(&code)),
                    Some(serde_json::json!({})),
                    ApiTag::Done("Вы присоединились к серверу".into()),
                );
            }
            Action::CreateGuild(name) => {
                let name = name.trim().to_string();
                if name.len() < 2 {
                    self.toast("Название сервера — от 2 символов", true);
                    return;
                }
                self.server_dialog = None;
                self.api(
                    "POST",
                    "/guilds".into(),
                    Some(serde_json::json!({ "name": name })),
                    ApiTag::Done("Сервер создан".into()),
                );
            }
            Action::MessageMenu(cid, m, pos) => {
                self.msg_menu = Some((cid, *m, pos));
                self.menu_frame = ctx.frame_nr();
            }
            Action::Pin(cid, mid, pin) => {
                let method = if pin { "PUT" } else { "DELETE" };
                let text = if pin {
                    "Сообщение закреплено"
                } else {
                    "Сообщение откреплено"
                };
                self.api(
                    method,
                    format!("/channels/{cid}/pins/{mid}"),
                    None,
                    ApiTag::Done(text.into()),
                );
                if let Some(c) = self.cache.get_mut(&cid) {
                    if let Some(m) = c.msgs.iter_mut().find(|m| m.id == mid) {
                        m.pinned = pin;
                    }
                }
                if let Some((pc, Some(Ok(list)))) = &mut self.pins {
                    if *pc == cid && !pin {
                        list.retain(|m| m.id != mid);
                    }
                }
            }
            Action::OpenPins(cid) => {
                self.pins = Some((cid, None));
                self.api(
                    "GET",
                    format!("/channels/{cid}/pins"),
                    None,
                    ApiTag::Pins(cid),
                );
            }
            Action::OpenSearch => {
                if self.search.is_none() {
                    self.search = Some(SearchView {
                        query: String::new(),
                        loading: false,
                        total: 0,
                        results: None,
                    });
                } else {
                    self.search = None;
                }
            }
            Action::RunSearch(q) => {
                let q = q.trim().to_string();
                if q.is_empty() {
                    return;
                }
                let path = match (self.sel_guild, self.sel_channel) {
                    (Some(g), _) => format!(
                        "/guilds/{g}/messages/search?content={}",
                        discord::percent_encode(&q)
                    ),
                    (None, Some(c)) => format!(
                        "/channels/{c}/messages/search?content={}",
                        discord::percent_encode(&q)
                    ),
                    _ => return,
                };
                if let Some(sv) = &mut self.search {
                    sv.loading = true;
                    sv.results = None;
                }
                self.api("GET", path, None, ApiTag::Search);
            }
            Action::OpenThreads(parent) => {
                self.threads = Some((parent, None));
                let forum = self
                    .guilds
                    .iter()
                    .flat_map(|g| g.channels.iter())
                    .find(|c| c.id == parent)
                    .map_or(false, |c| matches!(c.kind, 15 | 16));
                let path = if forum {
                    format!("/channels/{parent}/threads/search?archived=false&sort_by=last_message_time&sort_order=desc&limit=25")
                } else {
                    format!("/channels/{parent}/threads/archived/public?limit=25")
                };
                self.api("GET", path, None, ApiTag::Threads(parent));
            }
            Action::OpenThread(guild, ch) => {
                self.threads = None;
                if let Some(gid) = guild {
                    if let Some(g) = self.guilds.iter_mut().find(|g| g.id == gid) {
                        if !g.channels.iter().any(|c| c.id == ch.id) {
                            g.channels.push(ch.clone());
                        }
                    }
                    if self.sel_guild != Some(gid) {
                        self.sel_guild = Some(gid);
                    }
                }
                self.open_channel(ch.id);
            }
            Action::Jump(guild, cid) => {
                self.switcher = None;
                let kind = guild
                    .and_then(|g| self.guilds.iter().find(|x| x.id == g))
                    .and_then(|g| g.channels.iter().find(|c| c.id == cid))
                    .map(|c| c.kind);
                if self.sel_guild != guild {
                    self.select_guild(guild);
                }
                match kind {
                    Some(k) if is_voice(k) => {
                        self.handle_action(ctx, Action::JoinVoice(guild, cid))
                    }
                    Some(15 | 16) => self.extra_action(ctx, Action::OpenThreads(cid)),
                    _ => self.open_channel(cid),
                }
            }
            Action::OpenSwitcher => {
                self.switcher = if self.switcher.is_some() {
                    None
                } else {
                    Some((String::new(), 0))
                };
            }
            _ => {}
        }
    }

    fn known_user(&self, uid: u64) -> (String, Option<String>) {
        if uid == self.me_id {
            return (self.me_name.clone(), self.me_avatar.clone());
        }
        if let Some(f) = self.friends.iter().find(|f| f.id == uid) {
            return (f.name.clone(), f.avatar.clone());
        }
        for d in &self.dms {
            if let Some((_, n, a)) = d.recipients.iter().find(|(id, _, _)| *id == uid) {
                return (n.clone(), a.clone());
            }
        }
        for c in self.cache.values() {
            if let Some(m) = c.msgs.iter().rev().find(|m| m.author_id == uid) {
                return (m.author.clone(), m.author_avatar.clone());
            }
        }
        for l in self.member_lists.values() {
            for it in l.items.iter().flatten() {
                if let ListItem::Member {
                    id, name, avatar, ..
                } = it
                {
                    if *id == uid {
                        return (name.clone(), avatar.clone());
                    }
                }
            }
        }
        (
            self.users
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| "Пользователь".into()),
            None,
        )
    }

    /// Варианты автодополнения для последнего слова в поле ввода: (начало слова, [(подпись, вставка)]).
    pub(crate) fn completions(&self, cid: u64) -> (usize, Vec<(String, String)>) {
        let draft = &self.draft;
        if draft.ends_with(char::is_whitespace) || draft.is_empty() {
            return (0, Vec::new());
        }
        let start = draft
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        let token = &draft[start..];
        let mut out: Vec<(String, String)> = Vec::new();
        let guild = self
            .sel_guild
            .and_then(|g| self.guilds.iter().find(|x| x.id == g));
        if let Some(q) = token.strip_prefix('@') {
            let q = q.to_lowercase();
            let mut seen = HashSet::new();
            let mut push = |id: u64, name: &str, out: &mut Vec<(String, String)>| {
                if id != 0 && name.to_lowercase().contains(&q) && seen.insert(id) {
                    out.push((format!("@{name}"), format!("<@{id}>")));
                }
            };
            if let Some(c) = self.cache.get(&cid) {
                for m in c.msgs.iter().rev() {
                    push(m.author_id, &m.author, &mut out);
                }
            }
            if let Some(d) = self.dms.iter().find(|d| d.id == cid) {
                for (id, n, _) in &d.recipients {
                    push(*id, n, &mut out);
                }
            }
            if let Some(g) = guild {
                if let Some(l) = self.member_lists.get(&g.id) {
                    for it in l.items.iter().flatten() {
                        if let ListItem::Member { id, name, .. } = it {
                            push(*id, name, &mut out);
                        }
                    }
                }
                for (id, (name, _)) in &g.roles {
                    if name != "@everyone" && name.to_lowercase().contains(&q) {
                        out.push((format!("@{name} (роль)"), format!("<@&{id}>")));
                    }
                }
            }
            for e in ["everyone", "here"] {
                if e.starts_with(&q) && guild.is_some() {
                    out.push((format!("@{e}"), format!("@{e}")));
                }
            }
        } else if let (Some(q), Some(g)) = (token.strip_prefix('#'), guild) {
            let q = q.to_lowercase();
            for c in &g.channels {
                if (is_text(c.kind) || is_voice(c.kind)) && c.name.to_lowercase().contains(&q) {
                    out.push((format!("#{}", c.name), format!("<#{}>", c.id)));
                }
            }
        } else if let (Some(q), Some(g)) = (token.strip_prefix(':'), guild) {
            if q.chars().count() >= 2 && !q.ends_with(':') {
                let q = q.to_lowercase();
                for e in &g.emojis {
                    if e.name.to_lowercase().contains(&q) {
                        let id = e.id.unwrap_or(0);
                        let insert = if e.animated {
                            format!("<a:{}:{id}>", e.name)
                        } else {
                            format!("<:{}:{id}>", e.name)
                        };
                        out.push((format!(":{}:", e.name), insert));
                    }
                }
            }
        }
        out.truncate(8);
        (start, out)
    }

    // ---------- все дополнительные окна за кадр ----------

    pub(crate) fn extra_ui(&mut self, ctx: &egui::Context) {
        self.window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if self.window_focused {
            // окно снова активно — упоминания в открытом канале прочитаны
            if let Some(c) = self.sel_channel {
                self.mentions.remove(&c);
            }
        }
        if std::mem::take(&mut self.attention) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        let total: u32 = self.mentions.values().sum();
        let title = if total > 0 {
            format!("({total}) Rustcord")
        } else {
            "Rustcord".to_string()
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        if self.screen != Screen::Main {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.extra_action(ctx, Action::OpenSwitcher);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F))
            && (self.sel_guild.is_some() || self.sel_channel.is_some())
        {
            self.extra_action(ctx, Action::OpenSearch);
        }
        let mut actions: Vec<Action> = Vec::new();
        self.status_ui(ctx, &mut actions);
        self.msg_menu_ui(ctx, &mut actions);
        self.guild_menu_ui(ctx, &mut actions);
        self.profile_ui(ctx, &mut actions);
        self.pins_ui(ctx, &mut actions);
        self.search_ui(ctx, &mut actions);
        self.threads_ui(ctx, &mut actions);
        self.switcher_ui(ctx, &mut actions);
        self.server_ui(ctx, &mut actions);
        for a in actions {
            self.handle_action(ctx, a);
        }
    }

    fn status_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.status_menu {
            return;
        }
        let pal = self.pal;
        let solid = self.solid();
        let screen = ctx.screen_rect();
        let mut close = false;
        let area = egui::Area::new(egui::Id::new("status_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(84.0, screen.bottom() - 330.0))
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(solid)
                    .rounding(pal.radius + 2.0)
                    .stroke(Stroke::new(1.0, pal.border))
                    .shadow(egui::epaint::Shadow {
                        offset: vec2(0.0, 6.0),
                        blur: 24.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(90),
                    })
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(250.0);
                        ui.label(RichText::new("СТАТУС").size(11.5).strong().color(pal.muted));
                        ui.add_space(4.0);
                        for (st, desc) in [
                            ("online", ""),
                            ("idle", ""),
                            ("dnd", "Без уведомлений на рабочем столе"),
                            ("invisible", "Вы будете казаться не в сети"),
                        ] {
                            let h = if desc.is_empty() { 34.0 } else { 46.0 };
                            let (rect, resp) = ui
                                .allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                            let sel = self.my_status == st;
                            if resp.hovered() || sel {
                                ui.painter().rect_filled(
                                    rect,
                                    8.0,
                                    if sel {
                                        pal.accent.gamma_multiply(0.18)
                                    } else {
                                        pal.hover
                                    },
                                );
                            }
                            let cy = if desc.is_empty() {
                                rect.center().y
                            } else {
                                rect.top() + 15.0
                            };
                            presence_dot(ui, &pal, pos2(rect.left() + 16.0, cy), 6.0, st, solid);
                            ui.painter().text(
                                pos2(rect.left() + 32.0, cy),
                                Align2::LEFT_CENTER,
                                presence_text(st),
                                FontId::proportional(14.0),
                                pal.text,
                            );
                            if !desc.is_empty() {
                                ui.painter().text(
                                    pos2(rect.left() + 32.0, rect.top() + 33.0),
                                    Align2::LEFT_CENTER,
                                    desc,
                                    FontId::proportional(11.5),
                                    pal.muted,
                                );
                            }
                            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                actions.push(Action::SetStatus(st.to_string()));
                                close = true;
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("СВОЙ СТАТУС")
                                .size(11.5)
                                .strong()
                                .color(pal.muted),
                        );
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.settings.custom_status)
                                .hint_text("Чем заняты?")
                                .char_limit(128)
                                .desired_width(f32::INFINITY),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            actions.push(Action::SetStatus(self.my_status.clone()));
                            close = true;
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button("Сохранить").clicked() {
                                actions.push(Action::SetStatus(self.my_status.clone()));
                                close = true;
                            }
                            if ui.button("Очистить").clicked() {
                                self.settings.custom_status.clear();
                                actions.push(Action::SetStatus(self.my_status.clone()));
                            }
                        });
                    });
            });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && !area.response.contains_pointer()
            && ctx.frame_nr() > self.menu_frame + 1;
        if close || clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.status_menu = false;
        }
    }

    fn msg_menu_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((cid, m, pos)) = self.msg_menu.clone() else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let own = m.author_id == self.me_id;
        let guild = self.sel_guild;
        let mut close = false;
        let area = egui::Area::new(egui::Id::new("msg_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain(true)
            .show(ctx, |ui| {
                menu_frame(&pal, solid).show(ui, |ui| {
                    ui.set_width(210.0);
                    let mut item = |ui: &mut egui::Ui, text: &str, danger: bool| -> bool {
                        let r = menu_item(ui, &pal, text, danger);
                        if r {
                            close = true;
                        }
                        r
                    };
                    if item(ui, "↩  Ответить", false) {
                        actions.push(Action::Reply(m.id, m.author.clone()));
                    }
                    if item(ui, "😊  Добавить реакцию", false) {
                        actions.push(Action::OpenPicker(
                            PickerTarget::React {
                                channel_id: cid,
                                message_id: m.id,
                            },
                            pos,
                        ));
                    }
                    if own && item(ui, "✏  Изменить", false) {
                        actions.push(Action::StartEdit(cid, m.id, m.content.clone()));
                    }
                    let pin_text = if m.pinned {
                        "📌  Открепить"
                    } else {
                        "📌  Закрепить"
                    };
                    if item(ui, pin_text, false) {
                        actions.push(Action::Pin(cid, m.id, !m.pinned));
                    }
                    if let Some((tid, name, _)) = &m.thread {
                        if item(ui, "🧵  Открыть ветку", false) {
                            actions.push(Action::OpenThread(
                                guild,
                                Channel {
                                    id: *tid,
                                    name: name.clone(),
                                    kind: 11,
                                    position: 0,
                                    parent: Some(cid),
                                },
                            ));
                        }
                    }
                    ui.separator();
                    if item(ui, "⧉  Копировать текст", false) {
                        actions.push(Action::CopyText(m.content.clone()));
                    }
                    if item(ui, "🔗  Копировать ссылку", false) {
                        let g = guild.map(|g| g.to_string()).unwrap_or_else(|| "@me".into());
                        actions.push(Action::CopyText(format!(
                            "https://discord.com/channels/{g}/{cid}/{}",
                            m.id
                        )));
                    }
                    if item(ui, "#  Копировать ID сообщения", false) {
                        actions.push(Action::CopyId(m.id));
                    }
                    ui.separator();
                    if item(ui, "👤  Профиль автора", false) {
                        actions.push(Action::OpenProfile(m.author_id));
                    }
                    if !own && item(ui, "💬  Написать автору", false) {
                        actions.push(Action::MessageUser(m.author_id));
                    }
                    ui.separator();
                    if own {
                        if item(ui, "🗑  Удалить", true) {
                            let preview: String = m.content.chars().take(160).collect();
                            actions.push(Action::AskDelete(cid, m.id, preview));
                        }
                    } else if item(ui, "⚑  Пожаловаться", true) {
                        let g = guild.map(|g| g.to_string()).unwrap_or_else(|| "@me".into());
                        actions.push(Action::Report(format!(
                            "https://discord.com/channels/{g}/{cid}/{}",
                            m.id
                        )));
                    }
                });
            });
        if close || self.menu_should_close(ctx, &area.response) {
            self.msg_menu = None;
        }
    }

    fn menu_should_close(&self, ctx: &egui::Context, resp: &egui::Response) -> bool {
        let pressed = ctx.input(|i| i.pointer.any_pressed());
        (pressed && !resp.contains_pointer() && ctx.frame_nr() > self.menu_frame + 1)
            || ctx.input(|i| i.key_pressed(egui::Key::Escape))
    }

    fn guild_menu_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let pal = self.pal;
        let solid = self.solid();
        if let Some((g, pos)) = self.guild_menu {
            let name = self
                .guilds
                .iter()
                .find(|x| x.id == g)
                .map(|x| x.name.clone())
                .unwrap_or_default();
            let mut close = false;
            let area = egui::Area::new(egui::Id::new("guild_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .constrain(true)
                .show(ctx, |ui| {
                    menu_frame(&pal, solid).show(ui, |ui| {
                        ui.set_width(220.0);
                        ui.add(
                            egui::Label::new(RichText::new(&name).strong().color(pal.text))
                                .truncate(),
                        );
                        ui.separator();
                        if menu_item(ui, &pal, "✉  Пригласить людей", false) {
                            actions.push(Action::CopyInvite(g));
                            close = true;
                        }
                        if menu_item(ui, &pal, "✓  Отметить как прочитанное", false)
                        {
                            self.unread.retain(|_, x| *x != g);
                            let chans: Vec<u64> = self
                                .guilds
                                .iter()
                                .find(|x| x.id == g)
                                .map(|x| x.channels.iter().map(|c| c.id).collect())
                                .unwrap_or_default();
                            for c in chans {
                                self.mentions.remove(&c);
                            }
                            close = true;
                        }
                        if menu_item(ui, &pal, "#  Копировать ID сервера", false) {
                            actions.push(Action::CopyId(g));
                            close = true;
                        }
                        ui.separator();
                        if menu_item(ui, &pal, "⎋  Покинуть сервер", true) {
                            actions.push(Action::AskLeave(g));
                            close = true;
                        }
                    });
                });
            if close || self.menu_should_close(ctx, &area.response) {
                self.guild_menu = None;
            }
        }
        if let Some(g) = self.confirm_leave {
            let name = self
                .guilds
                .iter()
                .find(|x| x.id == g)
                .map(|x| x.name.clone())
                .unwrap_or_default();
            let mut open = true;
            egui::Window::new(RichText::new("Покинуть сервер?").strong())
                .id(egui::Id::new("leave_win"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(window_frame(ctx, &pal, solid))
                .show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.label(
                        RichText::new(format!(
                            "Вы уверены, что хотите покинуть «{name}»? Вернуться можно будет только по приглашению."
                        ))
                        .color(pal.text),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Покинуть").color(Color32::WHITE).strong())
                                    .fill(pal.danger)
                                    .rounding(8.0),
                            )
                            .clicked()
                        {
                            actions.push(Action::LeaveGuild(g));
                        }
                        if ui.button("Отмена").clicked() {
                            self.confirm_leave = None;
                        }
                    });
                });
            if !open {
                self.confirm_leave = None;
            }
        }
    }

    fn profile_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(p) = &self.profile else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let uid = p.user_id;
        let me = uid == self.me_id;
        let avatar = p.avatar.clone();
        let banner_url = p
            .banner
            .as_ref()
            .map(|b| format!("https://cdn.discordapp.com/banners/{uid}/{b}.png?size=480"));
        let avatar_tex = self.avatar_tex(ctx, uid, &avatar, 128);
        let banner_tex = match (&banner_url, self.settings.load_images) {
            (Some(u), true) => self.media.get(ctx, &self.cmd, u, u, 480, false),
            _ => None,
        };
        let status = if me {
            self.my_status.clone()
        } else {
            self.presence.get(&uid).cloned().unwrap_or_default()
        };
        let friend = self.friends.iter().find(|f| f.id == uid).map(|f| f.kind);
        let guild = self
            .sel_guild
            .and_then(|g| self.guilds.iter().find(|x| x.id == g))
            .cloned();
        let p = self.profile.as_ref().unwrap();
        let mut open = true;
        let since = {
            let ms = snowflake_ms(uid) as i64;
            markdown::format_time(ms / 1000, 'D')
        };
        // для «О себе» нужна разметка — рисуем через тот же контекст, что и сообщения
        let mut inner_actions: Vec<Action> = Vec::new();
        let bio = p.bio.clone();
        let name = p.name.clone();
        let username = p.username.clone();
        let pronouns = p.pronouns.clone();
        let roles = p.roles.clone();
        let mutual = p.mutual;
        let loading = p.loading;
        let error = p.error.clone();
        let accent = p.accent;
        let bot = p.bot;

        egui::Window::new("profile")
            .id(egui::Id::new("profile_win"))
            .title_bar(false)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(solid)
                    .rounding(pal.radius + 8.0)
                    .inner_margin(Margin::same(0.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(360.0);
                let (banner, _) = ui.allocate_exact_size(vec2(360.0, 120.0), Sense::hover());
                let r = pal.radius + 8.0;
                let rounding = Rounding {
                    nw: r,
                    ne: r,
                    sw: 0.0,
                    se: 0.0,
                };
                match banner_tex {
                    Some((tex, _)) => {
                        egui::Image::from_texture(SizedTexture::new(tex, banner.size()))
                            .rounding(rounding)
                            .paint_at(ui, banner);
                    }
                    None => {
                        let c = accent
                            .map(|c| role_color(c, pal.accent))
                            .unwrap_or_else(|| name_color(uid).gamma_multiply(0.85));
                        ui.painter().rect_filled(banner, rounding, c);
                    }
                }
                // закрыть
                let close =
                    Rect::from_min_size(banner.right_top() + vec2(-40.0, 10.0), vec2(30.0, 30.0));
                let cr = ui.interact(close, ui.id().with("close_profile"), Sense::click());
                ui.painter().circle_filled(
                    close.center(),
                    15.0,
                    Color32::from_black_alpha(if cr.hovered() { 170 } else { 110 }),
                );
                ui.painter().text(
                    close.center(),
                    Align2::CENTER_CENTER,
                    "✕",
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
                if cr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    self.profile = None;
                }
                // аватар поверх баннера
                let ar = Rect::from_center_size(
                    banner.left_bottom() + vec2(64.0, 4.0),
                    vec2(92.0, 92.0),
                );
                ui.painter().circle_filled(ar.center(), 50.0, solid);
                paint_avatar(ui, ar, avatar_tex, uid, &name);
                presence_dot(
                    ui,
                    &pal,
                    ar.center() + vec2(33.0, 33.0),
                    9.0,
                    &status,
                    solid,
                );
                ui.add_space(52.0);
                egui::Frame::none()
                    .inner_margin(Margin {
                        left: 18.0,
                        right: 18.0,
                        top: 0.0,
                        bottom: 18.0,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&name)
                                    .font(FontId::new(21.0, egui::FontFamily::Name("bold".into())))
                                    .color(pal.text),
                            );
                            if bot {
                                ui.label(
                                    RichText::new(" БОТ ")
                                        .size(10.5)
                                        .strong()
                                        .color(Color32::WHITE)
                                        .background_color(pal.accent),
                                );
                            }
                        });
                        let mut sub = String::new();
                        if !username.is_empty() {
                            sub.push_str(&username);
                        }
                        if !pronouns.is_empty() {
                            if !sub.is_empty() {
                                sub.push_str(" • ");
                            }
                            sub.push_str(&pronouns);
                        }
                        if !sub.is_empty() {
                            ui.label(RichText::new(sub).color(pal.muted));
                        }
                        if !status.is_empty() {
                            ui.label(
                                RichText::new(presence_text(&status))
                                    .small()
                                    .color(pal.faint),
                            );
                        }
                        egui::Frame::none()
                            .fill(pal.elevated)
                            .rounding(pal.radius)
                            .inner_margin(Margin::same(12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if loading {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new("Загрузка профиля…").color(pal.muted),
                                        );
                                    });
                                }
                                if let Some(e) = &error {
                                    ui.label(RichText::new(e).small().color(pal.danger));
                                }
                                if !bio.is_empty() {
                                    section_title(ui, &pal, "О СЕБЕ");
                                    let mut mcx = MsgCtx {
                                        pal,
                                        settings: &self.settings,
                                        media: &mut self.media,
                                        cmd: &self.cmd,
                                        actions: &mut inner_actions,
                                        me_id: self.me_id,
                                        guild_id: self.sel_guild,
                                        channel_id: 0,
                                        toolbar_allowed: false,
                                        guilds: &self.guilds,
                                        users: &self.users,
                                    };
                                    rich_text(ui, &mut mcx, &bio, &[]);
                                }
                                section_title(ui, &pal, "В DISCORD С");
                                ui.label(RichText::new(&since).color(pal.text));
                                if let Some(g) = &guild {
                                    let named: Vec<(String, u32)> = roles
                                        .iter()
                                        .filter_map(|r| g.roles.get(r).cloned())
                                        .collect();
                                    if !named.is_empty() {
                                        section_title(ui, &pal, "РОЛИ");
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                                            for (n, c) in named {
                                                let col = role_color(c, pal.muted);
                                                egui::Frame::none()
                                                    .fill(pal.side)
                                                    .rounding(6.0)
                                                    .inner_margin(Margin::symmetric(7.0, 3.0))
                                                    .show(ui, |ui| {
                                                        ui.horizontal(|ui| {
                                                            let (d, _) = ui.allocate_exact_size(
                                                                vec2(10.0, 10.0),
                                                                Sense::hover(),
                                                            );
                                                            ui.painter().circle_filled(
                                                                d.center(),
                                                                5.0,
                                                                col,
                                                            );
                                                            ui.label(
                                                                RichText::new(n)
                                                                    .small()
                                                                    .color(pal.text),
                                                            );
                                                        });
                                                    });
                                            }
                                        });
                                    }
                                }
                                if mutual > 0 && !me {
                                    section_title(ui, &pal, "ОБЩИЕ СЕРВЕРЫ");
                                    ui.label(RichText::new(mutual.to_string()).color(pal.text));
                                }
                            });
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if !me {
                                if accent_button(ui, &pal, "💬 Написать", 150.0, 36.0).clicked()
                                {
                                    actions.push(Action::MessageUser(uid));
                                }
                                match friend {
                                    Some(1) => {}
                                    Some(3) => {
                                        if ui
                                            .add_sized(
                                                [150.0, 36.0],
                                                egui::Button::new("✓ Принять заявку"),
                                            )
                                            .clicked()
                                        {
                                            actions.push(Action::FriendAccept(uid));
                                        }
                                    }
                                    Some(4) => {
                                        ui.add_enabled(
                                            false,
                                            egui::Button::new("Заявка отправлена")
                                                .min_size(vec2(150.0, 36.0)),
                                        );
                                    }
                                    _ if !self.is_bot() => {
                                        if ui
                                            .add_sized(
                                                [150.0, 36.0],
                                                egui::Button::new("➕ В друзья"),
                                            )
                                            .clicked()
                                        {
                                            actions.push(Action::FriendRequest(uid));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if ui
                                .add(egui::Button::new("#").min_size(vec2(36.0, 36.0)))
                                .on_hover_text("Копировать ID")
                                .clicked()
                            {
                                actions.push(Action::CopyId(uid));
                            }
                        });
                    });
            });
        actions.extend(inner_actions);
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.profile = None;
        }
    }

    fn message_list_window(
        &mut self,
        ui: &mut egui::Ui,
        msgs: &[Msg],
        actions: &mut Vec<Action>,
        show_channel: bool,
    ) {
        let pal = self.pal;
        let channel_names: Vec<Option<(Option<u64>, String)>> = msgs
            .iter()
            .map(|m| {
                if !show_channel {
                    return None;
                }
                let g = m.guild_id.or(self.sel_guild);
                let name = g
                    .and_then(|g| self.guilds.iter().find(|x| x.id == g))
                    .and_then(|x| x.channels.iter().find(|c| c.id == m.channel_id))
                    .map(|c| format!("# {}", c.name))
                    .or_else(|| {
                        self.dms
                            .iter()
                            .find(|d| d.id == m.channel_id)
                            .map(|d| format!("@ {}", d.name))
                    })
                    .unwrap_or_else(|| "канал".into());
                Some((g, name))
            })
            .collect();
        let mut mcx = MsgCtx {
            pal,
            settings: &self.settings,
            media: &mut self.media,
            cmd: &self.cmd,
            actions,
            me_id: self.me_id,
            guild_id: self.sel_guild,
            channel_id: 0,
            toolbar_allowed: false,
            guilds: &self.guilds,
            users: &self.users,
        };
        egui::ScrollArea::vertical()
            .max_height(520.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (m, ch) in msgs.iter().zip(channel_names) {
                    egui::Frame::none()
                        .fill(pal.elevated)
                        .rounding(pal.radius)
                        .inner_margin(Margin::symmetric(0.0, 6.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if let Some((g, name)) = ch {
                                ui.horizontal(|ui| {
                                    ui.add_space(14.0);
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(name)
                                                    .small()
                                                    .strong()
                                                    .color(pal.muted),
                                            )
                                            .sense(Sense::click()),
                                        )
                                        .on_hover_cursor(CursorIcon::PointingHand)
                                        .on_hover_text("Перейти в канал")
                                        .clicked()
                                    {
                                        mcx.actions.push(Action::Jump(g, m.channel_id));
                                    }
                                });
                            }
                            mcx.channel_id = m.channel_id;
                            mcx.guild_id = m.guild_id.or(self.sel_guild);
                            message_ui(ui, &mut mcx, m, false);
                        });
                    ui.add_space(6.0);
                }
            });
    }

    fn pins_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((cid, state)) = self.pins.take() else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let mut open = true;
        egui::Window::new(RichText::new("📌 Закреплённые сообщения").strong())
            .id(egui::Id::new("pins_win"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::RIGHT_TOP, [-24.0, 70.0])
            .frame(window_frame(ctx, &pal, solid))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                match &state {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Загрузка…").color(pal.muted));
                        });
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(pal.danger));
                    }
                    Some(Ok(list)) if list.is_empty() => {
                        ui.label(
                            RichText::new("В этом канале ничего не закреплено.").color(pal.muted),
                        );
                    }
                    Some(Ok(list)) => {
                        ui.label(
                            RichText::new("Правый клик по сообщению — открепить, ответить и т.д.")
                                .small()
                                .color(pal.faint),
                        );
                        ui.add_space(6.0);
                        let list = list.clone();
                        self.message_list_window(ui, &list, actions, false);
                    }
                }
            });
        let closed = !open || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if !closed && self.pins.is_none() {
            self.pins = Some((cid, state));
        }
    }

    fn search_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut sv) = self.search.take() else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let mut open = true;
        let scope = match self.sel_guild {
            Some(g) => self
                .guilds
                .iter()
                .find(|x| x.id == g)
                .map(|x| format!("Поиск по серверу «{}»", x.name))
                .unwrap_or_default(),
            None => "Поиск в этом диалоге".into(),
        };
        egui::Window::new(RichText::new("🔍 Поиск").strong())
            .id(egui::Id::new("search_win"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::RIGHT_TOP, [-24.0, 70.0])
            .frame(window_frame(ctx, &pal, solid))
            .show(ctx, |ui| {
                ui.set_width(480.0);
                ui.label(RichText::new(scope).small().color(pal.muted));
                let r = ui.add(
                    egui::TextEdit::singleline(&mut sv.query)
                        .id(egui::Id::new("search_input"))
                        .hint_text("Что ищем? Enter — найти")
                        .desired_width(f32::INFINITY),
                );
                if sv.results.is_none() && !sv.loading {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(Action::RunSearch(sv.query.clone()));
                }
                ui.add_space(8.0);
                if sv.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Ищу…").color(pal.muted));
                    });
                }
                match &sv.results {
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(pal.danger));
                    }
                    Some(Ok(list)) => {
                        ui.label(
                            RichText::new(format!("Найдено: {}", sv.total))
                                .small()
                                .strong()
                                .color(pal.muted),
                        );
                        ui.add_space(4.0);
                        let list = list.clone();
                        self.message_list_window(ui, &list, actions, true);
                    }
                    None => {}
                }
            });
        let closed = !open || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if !closed && self.search.is_none() {
            self.search = Some(sv);
        }
    }

    fn threads_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((parent, state)) = &self.threads else {
            return;
        };
        let parent = *parent;
        let pal = self.pal;
        let solid = self.solid();
        let guild = self
            .guilds
            .iter()
            .find(|g| g.channels.iter().any(|c| c.id == parent));
        let gid = guild.map(|g| g.id);
        let parent_name = guild
            .and_then(|g| g.channels.iter().find(|c| c.id == parent))
            .map(|c| c.name.clone())
            .unwrap_or_default();
        // активные ветки, о которых мы уже знаем, плюс найденные запросом
        let mut list: Vec<(Channel, u32)> = guild
            .map(|g| {
                g.channels
                    .iter()
                    .filter(|c| c.parent == Some(parent) && is_thread(c.kind))
                    .map(|c| (c.clone(), 0))
                    .collect()
            })
            .unwrap_or_default();
        let mut error = None;
        let loading = state.is_none();
        if let Some(res) = state {
            match res {
                Ok(found) => {
                    for (_, c, n) in found {
                        match list.iter_mut().find(|(x, _)| x.id == c.id) {
                            Some(x) => x.1 = *n,
                            None => list.push((c.clone(), *n)),
                        }
                    }
                }
                Err(e) => error = Some(e.clone()),
            }
        }
        let mut open = true;
        egui::Window::new(RichText::new(format!("🧵 Ветки · {parent_name}")).strong())
            .id(egui::Id::new("threads_win"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(window_frame(ctx, &pal, solid))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Загрузка веток…").color(pal.muted));
                    });
                }
                if let Some(e) = &error {
                    ui.label(RichText::new(e).small().color(pal.danger));
                }
                if !loading && list.is_empty() {
                    ui.label(RichText::new("Веток пока нет.").color(pal.muted));
                }
                egui::ScrollArea::vertical()
                    .max_height(480.0)
                    .show(ui, |ui| {
                        for (c, n) in &list {
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 46.0),
                                Sense::click(),
                            );
                            let h = ui
                                .ctx()
                                .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                            ui.painter()
                                .rect_filled(rect, 10.0, mix(pal.elevated, pal.hover, h));
                            ui.painter().text(
                                rect.left_center() + vec2(14.0, 0.0),
                                Align2::LEFT_CENTER,
                                "🧵",
                                FontId::proportional(16.0),
                                pal.muted,
                            );
                            ui.painter().text(
                                rect.left_center() + vec2(42.0, if *n > 0 { -8.0 } else { 0.0 }),
                                Align2::LEFT_CENTER,
                                &c.name,
                                FontId::proportional(14.5),
                                pal.text,
                            );
                            if *n > 0 {
                                ui.painter().text(
                                    rect.left_center() + vec2(42.0, 10.0),
                                    Align2::LEFT_CENTER,
                                    format!("{n} сообщений"),
                                    FontId::proportional(11.5),
                                    pal.muted,
                                );
                            }
                            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                actions.push(Action::OpenThread(gid, c.clone()));
                            }
                            ui.add_space(4.0);
                        }
                    });
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.threads = None;
        }
    }

    fn switcher_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((mut query, mut sel)) = self.switcher.take() else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let q = query.trim().to_lowercase();
        // (подпись, подсказка, сервер, канал; канал 0 = сам сервер)
        let mut items: Vec<(String, String, Option<u64>, u64)> = Vec::new();
        for d in &self.dms {
            if q.is_empty() || d.name.to_lowercase().contains(&q) {
                items.push((
                    format!("@ {}", d.name),
                    "Личные сообщения".into(),
                    None,
                    d.id,
                ));
            }
        }
        for g in &self.guilds {
            if !q.is_empty() && g.name.to_lowercase().contains(&q) {
                items.push((format!("★ {}", g.name), "Сервер".into(), Some(g.id), 0));
            }
            for c in &g.channels {
                let glyph = if is_voice(c.kind) {
                    "🔊"
                } else if is_thread(c.kind) {
                    "🧵"
                } else if matches!(c.kind, 15 | 16) {
                    "💬"
                } else if is_text(c.kind) {
                    "#"
                } else {
                    continue;
                };
                if !q.is_empty() && c.name.to_lowercase().contains(&q) {
                    items.push((
                        format!("{glyph} {}", c.name),
                        g.name.clone(),
                        Some(g.id),
                        c.id,
                    ));
                }
            }
        }
        // точное начало названия — выше
        items.sort_by_key(|(l, _, _, _)| {
            let name = l
                .split_once(' ')
                .map(|(_, n)| n.to_lowercase())
                .unwrap_or_default();
            (!name.starts_with(&q), name.len())
        });
        items.truncate(10);
        sel = sel.min(items.len().saturating_sub(1));
        let mut open = true;
        let mut choose: Option<usize> = None;
        egui::Window::new("switcher")
            .id(egui::Id::new("switcher_win"))
            .title_bar(false)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 110.0])
            .frame(window_frame(ctx, &pal, solid))
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.label(
                    RichText::new("Куда перейти?")
                        .font(FontId::new(18.0, egui::FontFamily::Name("bold".into())))
                        .color(pal.text),
                );
                ui.add_space(6.0);
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                    sel = (sel + 1).min(items.len().saturating_sub(1));
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                    sel = sel.saturating_sub(1);
                }
                let r = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .id(egui::Id::new("switcher_input"))
                        .hint_text("Название канала, сервера или собеседника")
                        .desired_width(f32::INFINITY)
                        .font(FontId::proportional(16.0)),
                );
                r.request_focus();
                if r.changed() {
                    sel = 0;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !items.is_empty() {
                    choose = Some(sel);
                }
                ui.add_space(8.0);
                if items.is_empty() {
                    ui.label(
                        RichText::new(if q.is_empty() {
                            "Начните вводить — например, «общий»"
                        } else {
                            "Ничего не найдено"
                        })
                        .color(pal.muted),
                    );
                }
                for (i, (label, hint, _, _)) in items.iter().enumerate() {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                    if i == sel || resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            8.0,
                            if i == sel {
                                pal.accent.gamma_multiply(0.22)
                            } else {
                                pal.hover
                            },
                        );
                    }
                    ui.painter().text(
                        rect.left_center() + vec2(12.0, 0.0),
                        Align2::LEFT_CENTER,
                        label,
                        FontId::proportional(14.5),
                        pal.text,
                    );
                    ui.painter().text(
                        rect.right_center() - vec2(12.0, 0.0),
                        Align2::RIGHT_CENTER,
                        hint,
                        FontId::proportional(12.0),
                        pal.faint,
                    );
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        choose = Some(i);
                    }
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("↑↓ — выбор · Enter — перейти · Esc — закрыть")
                        .small()
                        .color(pal.faint),
                );
            });
        if let Some(i) = choose {
            let (_, _, g, c) = items[i].clone();
            if c == 0 {
                actions.push(Action::SelectGuild(g));
            } else {
                actions.push(Action::Jump(g, c));
            }
            return;
        }
        if open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.switcher.is_none() {
            self.switcher = Some((query, sel));
        }
    }

    fn server_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((mut tab, mut input)) = self.server_dialog.take() else {
            return;
        };
        let pal = self.pal;
        let solid = self.solid();
        let mut open = true;
        egui::Window::new(RichText::new("Добавить сервер").strong())
            .id(egui::Id::new("server_win"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(window_frame(ctx, &pal, solid))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.horizontal(|ui| {
                    if toggle_chip(ui, &pal, "Присоединиться", tab == 0).clicked() {
                        tab = 0;
                        input.clear();
                    }
                    if toggle_chip(ui, &pal, "Создать свой", tab == 1).clicked() {
                        tab = 1;
                        input.clear();
                    }
                });
                ui.add_space(10.0);
                let (hint, help, button) = if tab == 0 {
                    (
                        "https://discord.gg/abcdef",
                        "Вставьте ссылку-приглашение или её код.",
                        "Присоединиться",
                    )
                } else {
                    (
                        "Сервер моих друзей",
                        "Ваш сервер — место, где вы общаетесь с друзьями. Название можно поменять потом.",
                        "Создать",
                    )
                };
                ui.label(RichText::new(help).color(pal.muted));
                ui.add_space(6.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut input)
                        .hint_text(hint)
                        .desired_width(f32::INFINITY),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(12.0);
                let w = ui.available_width();
                if accent_button(ui, &pal, button, w, 38.0).clicked() || enter {
                    if tab == 0 {
                        actions.push(Action::JoinInvite(input.clone()));
                    } else {
                        actions.push(Action::CreateGuild(input.clone()));
                    }
                }
            });
        if open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.server_dialog.is_none()
        {
            self.server_dialog = Some((tab, input));
        }
    }
}

fn menu_frame(pal: &Palette, solid: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(solid)
        .rounding(pal.radius)
        .stroke(Stroke::new(1.0, pal.border))
        .shadow(egui::epaint::Shadow {
            offset: vec2(0.0, 6.0),
            blur: 22.0,
            spread: 0.0,
            color: Color32::from_black_alpha(90),
        })
        .inner_margin(Margin::same(6.0))
}

fn menu_item(ui: &mut egui::Ui, pal: &Palette, text: &str, danger: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, 6.0, if danger { pal.danger } else { pal.accent });
    }
    let color = if hovered {
        Color32::WHITE
    } else if danger {
        pal.danger
    } else {
        pal.text
    };
    ui.painter().text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.5),
        color,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

pub fn is_thread(kind: u8) -> bool {
    matches!(kind, 10 | 11 | 12)
}

// ======================= друзья =======================

#[allow(clippy::too_many_arguments)]
pub fn friends_view(
    ui: &mut egui::Ui,
    pal: &Palette,
    friends: &[discord::Friend],
    presence: &HashMap<u64, String>,
    tab: &mut u8,
    input: &mut String,
    media: &mut MediaCache,
    cmd: &UnboundedSender<Cmd>,
    load_avatars: bool,
    actions: &mut Vec<Action>,
) {
    egui::Frame::none()
        .inner_margin(Margin::symmetric(22.0, 16.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("👥 Друзья")
                        .font(FontId::new(18.0, egui::FontFamily::Name("bold".into())))
                        .color(pal.text),
                );
                ui.add_space(14.0);
                let pending = friends.iter().filter(|f| f.kind == 3).count();
                for (i, label) in ["В сети", "Все", "Ожидание", "Заблокированные"]
                    .iter()
                    .enumerate()
                {
                    let text = if i == 2 && pending > 0 {
                        format!("{label} ({pending})")
                    } else {
                        label.to_string()
                    };
                    if toggle_chip(ui, pal, &text, *tab == i as u8).clicked() {
                        *tab = i as u8;
                    }
                    ui.add_space(4.0);
                }
                ui.add_space(6.0);
                let add = *tab == 4;
                let r = ui.add(
                    egui::Button::new(RichText::new("Добавить в друзья").strong().color(if add {
                        pal.ok
                    } else {
                        Color32::WHITE
                    }))
                    .fill(if add {
                        pal.ok.gamma_multiply(0.15)
                    } else {
                        pal.ok.gamma_multiply(0.85)
                    })
                    .rounding(12.0),
                );
                if r.clicked() {
                    *tab = 4;
                }
            });
            ui.add_space(12.0);
            if *tab == 4 {
                ui.label(
                    RichText::new("ДОБАВИТЬ В ДРУЗЬЯ")
                        .font(FontId::new(15.0, egui::FontFamily::Name("bold".into())))
                        .color(pal.text),
                );
                ui.label(
                    RichText::new("Вы можете добавить друга по его имени пользователя.")
                        .color(pal.muted),
                );
                ui.add_space(8.0);
                egui::Frame::none()
                    .fill(pal.elevated)
                    .rounding(10.0)
                    .inner_margin(Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - 200.0).max(120.0);
                            let r = ui.add(
                                egui::TextEdit::singleline(input)
                                    .hint_text("Введите имя пользователя")
                                    .frame(false)
                                    .desired_width(w),
                            );
                            let enter =
                                r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let send = ui
                                .add_enabled(
                                    !input.trim().is_empty(),
                                    egui::Button::new(
                                        RichText::new("Отправить заявку")
                                            .strong()
                                            .color(Color32::WHITE),
                                    )
                                    .fill(pal.accent)
                                    .rounding(8.0),
                                )
                                .clicked();
                            if (enter || send) && !input.trim().is_empty() {
                                actions.push(Action::FriendAdd(input.clone()));
                            }
                        });
                    });
                return;
            }
            let list: Vec<&discord::Friend> = friends
                .iter()
                .filter(|f| match *tab {
                    0 => {
                        f.kind == 1
                            && presence
                                .get(&f.id)
                                .map_or(false, |s| s != "offline" && s != "invisible")
                    }
                    1 => f.kind == 1,
                    2 => f.kind == 3 || f.kind == 4,
                    _ => f.kind == 2,
                })
                .collect();
            let title = match *tab {
                0 => "В СЕТИ",
                1 => "ВСЕ ДРУЗЬЯ",
                2 => "ОЖИДАНИЕ",
                _ => "ЗАБЛОКИРОВАННЫЕ",
            };
            ui.label(
                RichText::new(format!("{title} — {}", list.len()))
                    .size(11.5)
                    .strong()
                    .color(pal.muted),
            );
            ui.add_space(6.0);
            if list.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🦀").size(48.0));
                    ui.label(
                        RichText::new(match *tab {
                            0 => "Никого из друзей нет в сети. Самое время написать кому-нибудь!",
                            2 => "Нет заявок в друзья.",
                            3 => "Вы никого не заблокировали.",
                            _ => "Друзей пока нет — добавьте кого-нибудь.",
                        })
                        .color(pal.muted),
                    );
                });
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    for f in list {
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
                        let h = ui
                            .ctx()
                            .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                        if h > 0.0 {
                            ui.painter()
                                .rect_filled(rect, 10.0, pal.hover.gamma_multiply(h));
                        }
                        ui.painter().hline(
                            rect.left() + 8.0..=rect.right() - 8.0,
                            rect.top(),
                            Stroke::new(1.0, pal.border.gamma_multiply(0.6)),
                        );
                        let ar = Rect::from_center_size(
                            rect.left_center() + vec2(28.0, 0.0),
                            vec2(38.0, 38.0),
                        );
                        let tex = match (&f.avatar, load_avatars) {
                            (Some(hash), true) => {
                                let url = discord::avatar_url(f.id, hash);
                                media.get(ui.ctx(), cmd, &url, &url, 64, false)
                            }
                            _ => None,
                        };
                        paint_avatar(ui, ar, tex, f.id, &f.name);
                        let status = presence.get(&f.id).map(String::as_str).unwrap_or("offline");
                        if f.kind == 1 {
                            presence_dot(
                                ui,
                                pal,
                                ar.center() + vec2(14.0, 14.0),
                                5.5,
                                status,
                                pal.main,
                            );
                        }
                        ui.painter().text(
                            pos2(ar.right() + 12.0, rect.center().y - 9.0),
                            Align2::LEFT_CENTER,
                            &f.name,
                            FontId::new(15.0, egui::FontFamily::Name("bold".into())),
                            pal.text,
                        );
                        let sub = match f.kind {
                            3 => "Входящая заявка в друзья".to_string(),
                            4 => "Исходящая заявка в друзья".to_string(),
                            2 => "Заблокирован".to_string(),
                            _ => presence_text(status).to_string(),
                        };
                        ui.painter().text(
                            pos2(ar.right() + 12.0, rect.center().y + 10.0),
                            Align2::LEFT_CENTER,
                            sub,
                            FontId::proportional(12.5),
                            pal.muted,
                        );
                        // кнопки справа
                        let mut x = rect.right() - 26.0;
                        let mut btn =
                            |ui: &mut egui::Ui, glyph: &str, tip: &str, danger: bool| -> bool {
                                let c = pos2(x, rect.center().y);
                                x -= 44.0;
                                let r = Rect::from_center_size(c, vec2(36.0, 36.0));
                                let br = ui
                                    .interact(r, resp.id.with(glyph), Sense::click())
                                    .on_hover_text(tip)
                                    .on_hover_cursor(CursorIcon::PointingHand);
                                let fill = if br.hovered() {
                                    if danger {
                                        pal.danger
                                    } else {
                                        pal.accent
                                    }
                                } else {
                                    pal.elevated
                                };
                                ui.painter().circle_filled(c, 18.0, fill);
                                ui.painter().text(
                                    c,
                                    Align2::CENTER_CENTER,
                                    glyph,
                                    FontId::proportional(15.0),
                                    if br.hovered() {
                                        Color32::WHITE
                                    } else {
                                        pal.muted
                                    },
                                );
                                br.clicked()
                            };
                        match f.kind {
                            1 => {
                                if btn(ui, "✕", "Удалить из друзей", true) {
                                    actions.push(Action::FriendRemove(f.id));
                                }
                                if btn(ui, "💬", "Написать", false) {
                                    actions.push(Action::MessageUser(f.id));
                                }
                            }
                            3 => {
                                if btn(ui, "✕", "Отклонить", true) {
                                    actions.push(Action::FriendRemove(f.id));
                                }
                                if btn(ui, "✓", "Принять", false) {
                                    actions.push(Action::FriendAccept(f.id));
                                }
                            }
                            4 => {
                                if btn(ui, "✕", "Отменить заявку", true) {
                                    actions.push(Action::FriendRemove(f.id));
                                }
                            }
                            _ => {
                                if btn(ui, "↺", "Разблокировать", false) {
                                    actions.push(Action::FriendRemove(f.id));
                                }
                            }
                        }
                        if resp.clicked() {
                            actions.push(Action::OpenProfile(f.id));
                        }
                    }
                });
        });
}
