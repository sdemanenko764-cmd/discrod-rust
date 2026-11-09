//! Оформление: темы (в т.ч. в стиле GNOME / libadwaita) и стиль виджетов.

use eframe::egui::{
    self, epaint::Shadow, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle,
};

use crate::settings::{Settings, Theme};

/// Акцентные цвета: первый — фирменный цвет Rustcord, дальше — палитра акцентов GNOME и ещё несколько.
pub const ACCENTS: [(&str, Color32); 14] = [
    ("Ржавчина", Color32::from_rgb(232, 117, 58)),
    ("Синий", Color32::from_rgb(53, 132, 228)),
    ("Бирюзовый", Color32::from_rgb(33, 144, 164)),
    ("Зелёный", Color32::from_rgb(58, 148, 74)),
    ("Жёлтый", Color32::from_rgb(200, 136, 0)),
    ("Оранжевый", Color32::from_rgb(237, 91, 0)),
    ("Красный", Color32::from_rgb(230, 45, 66)),
    ("Розовый", Color32::from_rgb(213, 97, 153)),
    ("Фиолетовый", Color32::from_rgb(145, 65, 172)),
    ("Сланец", Color32::from_rgb(111, 131, 150)),
    ("Индиго", Color32::from_rgb(88, 101, 242)),
    ("Мята", Color32::from_rgb(16, 185, 129)),
    ("Коралл", Color32::from_rgb(255, 107, 107)),
    ("Лаванда", Color32::from_rgb(167, 139, 250)),
];

/// Темы в порядке показа в настройках, по группам.
pub const THEME_GROUPS: [(&str, &[(Theme, &str)]); 3] = [
    (
        "Живые — с плавающим светящимся фоном",
        &[
            (Theme::Aurora, "Аврора"),
            (Theme::Sunset, "Закат"),
            (Theme::Ocean, "Океан"),
            (Theme::Neon, "Неон"),
            (Theme::Lavender, "Лаванда"),
            (Theme::Forest, "Лес"),
            (Theme::Cosmos, "Космос"),
        ],
    ),
    (
        "Тёмные",
        &[
            (Theme::Classic, "Классика"),
            (Theme::AdwaitaDark, "GNOME тёмная"),
            (Theme::Catppuccin, "Catppuccin"),
            (Theme::TokyoNight, "Tokyo Night"),
            (Theme::Dracula, "Dracula"),
            (Theme::RosePine, "Rosé Pine"),
            (Theme::Gruvbox, "Gruvbox"),
            (Theme::Nord, "Nord"),
            (Theme::Dark, "Rustcord"),
            (Theme::Midnight, "AMOLED"),
        ],
    ),
    (
        "Светлые",
        &[
            (Theme::Light, "Светлая"),
            (Theme::AdwaitaLight, "GNOME светлая"),
            (Theme::Sakura, "Сакура"),
            (Theme::Mint, "Мята"),
        ],
    ),
];

#[derive(Clone, Copy)]
pub struct Palette {
    pub rail: Color32,
    pub side: Color32,
    pub main: Color32,
    pub header: Color32,
    pub elevated: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub msg_hover: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub link: Color32,
    pub danger: Color32,
    pub ok: Color32,
    pub warn: Color32,
    /// Скругление углов (у GNOME — крупнее).
    pub radius: f32,
    pub light: bool,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub fn palette(s: &Settings) -> Palette {
    let pal = palette_for(s.theme, ACCENTS[s.accent.min(ACCENTS.len() - 1)].1);
    if s.bg_image
        .as_deref()
        .map_or(false, |p| !p.trim().is_empty())
    {
        glassify(pal, s.panel_opacity)
    } else {
        pal
    }
}

/// Панели становятся полупрозрачными, чтобы под ними был виден свой фон.
fn glassify(p: Palette, opacity: f32) -> Palette {
    let a = |c: Color32, k: f32| {
        let [r, g, b, _] = c.to_srgba_unmultiplied();
        Color32::from_rgba_unmultiplied(r, g, b, (255.0 * (opacity * k).clamp(0.0, 1.0)) as u8)
    };
    let veil = if p.light { 0 } else { 255 };
    Palette {
        rail: a(p.rail, 1.15),
        side: a(p.side, 1.0),
        main: a(p.main, 0.75),
        header: a(p.header, 0.9),
        hover: Color32::from_rgba_unmultiplied(veil, veil, veil, 22),
        selected: Color32::from_rgba_unmultiplied(veil, veil, veil, 36),
        msg_hover: Color32::from_rgba_unmultiplied(veil, veil, veil, 10),
        elevated: a(p.elevated, 1.1),
        border: Color32::from_rgba_unmultiplied(veil, veil, veil, 22),
        ..p
    }
}

/// Живой фон анимированных тем: базовый цвет и три светящихся пятна.
#[derive(Clone, Copy)]
pub struct Backdrop {
    pub base: Color32,
    pub glows: [Color32; 3],
    /// Мерцающие звёзды поверх пятен (тема «Космос»).
    pub stars: bool,
}

pub fn backdrop(theme: Theme) -> Option<Backdrop> {
    Some(match theme {
        Theme::Aurora => Backdrop {
            base: rgb(6, 10, 20),
            glows: [rgb(20, 184, 166), rgb(99, 102, 241), rgb(34, 197, 94)],
            stars: false,
        },
        Theme::Sunset => Backdrop {
            base: rgb(18, 8, 20),
            glows: [rgb(249, 115, 22), rgb(236, 72, 153), rgb(139, 92, 246)],
            stars: false,
        },
        Theme::Ocean => Backdrop {
            base: rgb(3, 14, 24),
            glows: [rgb(14, 165, 233), rgb(6, 182, 212), rgb(59, 90, 246)],
            stars: false,
        },
        Theme::Neon => Backdrop {
            base: rgb(7, 6, 13),
            glows: [rgb(217, 70, 239), rgb(34, 211, 238), rgb(163, 230, 53)],
            stars: false,
        },
        Theme::Lavender => Backdrop {
            base: rgb(14, 10, 26),
            glows: [rgb(167, 139, 250), rgb(244, 114, 182), rgb(96, 165, 250)],
            stars: false,
        },
        Theme::Forest => Backdrop {
            base: rgb(5, 14, 10),
            glows: [rgb(34, 197, 94), rgb(20, 184, 166), rgb(234, 179, 8)],
            stars: false,
        },
        Theme::Cosmos => Backdrop {
            base: rgb(4, 3, 14),
            glows: [rgb(124, 58, 237), rgb(37, 99, 235), rgb(219, 39, 119)],
            stars: true,
        },
        _ => return None,
    })
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Полупрозрачная «стеклянная» палитра поверх живого фона.
fn glass(
    tint: (u8, u8, u8),
    accent: Color32,
    link: Color32,
    text: Color32,
    muted: Color32,
) -> Palette {
    let (r, g, b) = tint;
    Palette {
        rail: rgba(r, g, b, 215),
        side: rgba(r, g, b, 175),
        main: rgba(r, g, b, 105),
        header: rgba(r, g, b, 150),
        elevated: rgba(255, 255, 255, 20),
        hover: rgba(255, 255, 255, 26),
        selected: rgba(255, 255, 255, 40),
        msg_hover: rgba(255, 255, 255, 9),
        text,
        muted,
        faint: muted.gamma_multiply(0.7),
        border: rgba(255, 255, 255, 24),
        accent,
        link,
        danger: rgb(255, 99, 122),
        ok: rgb(74, 222, 128),
        warn: rgb(250, 204, 21),
        radius: 14.0,
        light: false,
    }
}

/// Обычная (непрозрачная) тёмная палитра по ключевым цветам.
#[allow(clippy::too_many_arguments)]
fn solid_dark(
    rail: Color32,
    side: Color32,
    main: Color32,
    elevated: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    link: Color32,
    danger: Color32,
    ok: Color32,
    warn: Color32,
    radius: f32,
) -> Palette {
    Palette {
        rail,
        side,
        main,
        header: main,
        elevated,
        hover: mix(elevated, text, 0.06),
        selected: mix(elevated, text, 0.12),
        msg_hover: mix(main, elevated, 0.45),
        text,
        muted,
        faint: mix(muted, main, 0.35),
        border: mix(main, text, 0.08),
        accent,
        link,
        danger,
        ok,
        warn,
        radius,
        light: false,
    }
}

/// Обычная светлая палитра по ключевым цветам.
#[allow(clippy::too_many_arguments)]
fn solid_light(
    rail: Color32,
    side: Color32,
    main: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    link: Color32,
    danger: Color32,
    ok: Color32,
    warn: Color32,
    radius: f32,
) -> Palette {
    Palette {
        rail,
        side,
        main,
        header: main,
        elevated: mix(side, main, 0.4),
        hover: mix(side, text, 0.06),
        selected: mix(side, text, 0.11),
        msg_hover: mix(main, side, 0.5),
        text,
        muted,
        faint: mix(muted, main, 0.35),
        border: mix(side, text, 0.09),
        accent,
        link,
        danger,
        ok,
        warn,
        radius,
        light: true,
    }
}

/// Плавное смешивание двух цветов (t = 0 → a, t = 1 → b).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

pub fn palette_for(theme: Theme, accent: Color32) -> Palette {
    match theme {
        Theme::Lavender => glass(
            (16, 12, 30),
            accent,
            rgb(196, 181, 253),
            rgb(245, 243, 255),
            rgb(190, 180, 220),
        ),
        Theme::Forest => glass(
            (6, 18, 12),
            accent,
            rgb(134, 239, 172),
            rgb(236, 253, 245),
            rgb(160, 200, 180),
        ),
        Theme::Cosmos => glass(
            (6, 5, 20),
            accent,
            rgb(165, 180, 252),
            rgb(238, 240, 255),
            rgb(170, 172, 210),
        ),
        Theme::Classic => Palette {
            rail: rgb(30, 31, 34),
            side: rgb(43, 45, 49),
            main: rgb(49, 51, 56),
            header: rgb(49, 51, 56),
            elevated: rgb(56, 58, 64),
            hover: rgb(53, 55, 60),
            selected: rgb(64, 66, 73),
            msg_hover: rgb(46, 48, 53),
            text: rgb(242, 243, 245),
            muted: rgb(181, 186, 193),
            faint: rgb(128, 132, 142),
            border: rgb(38, 40, 44),
            accent,
            link: rgb(0, 168, 252),
            danger: rgb(242, 63, 66),
            ok: rgb(35, 165, 90),
            warn: rgb(240, 178, 50),
            radius: 8.0,
            light: false,
        },
        Theme::TokyoNight => solid_dark(
            rgb(22, 22, 30),
            rgb(26, 27, 38),
            rgb(31, 33, 48),
            rgb(41, 46, 66),
            rgb(192, 202, 245),
            rgb(154, 165, 206),
            accent,
            rgb(122, 162, 247),
            rgb(247, 118, 142),
            rgb(158, 206, 106),
            rgb(224, 175, 104),
            12.0,
        ),
        Theme::Dracula => solid_dark(
            rgb(25, 26, 33),
            rgb(33, 34, 44),
            rgb(40, 42, 54),
            rgb(55, 57, 74),
            rgb(248, 248, 242),
            rgb(189, 190, 210),
            accent,
            rgb(139, 233, 253),
            rgb(255, 85, 85),
            rgb(80, 250, 123),
            rgb(241, 250, 140),
            10.0,
        ),
        Theme::RosePine => solid_dark(
            rgb(16, 14, 23),
            rgb(25, 23, 36),
            rgb(31, 29, 46),
            rgb(38, 35, 58),
            rgb(224, 222, 244),
            rgb(144, 140, 170),
            accent,
            rgb(156, 207, 216),
            rgb(235, 111, 146),
            rgb(49, 116, 143),
            rgb(246, 193, 119),
            14.0,
        ),
        Theme::Gruvbox => solid_dark(
            rgb(24, 24, 24),
            rgb(32, 32, 32),
            rgb(40, 40, 40),
            rgb(60, 56, 54),
            rgb(235, 219, 178),
            rgb(189, 174, 147),
            accent,
            rgb(131, 165, 152),
            rgb(251, 73, 52),
            rgb(184, 187, 38),
            rgb(250, 189, 47),
            6.0,
        ),
        Theme::Sakura => solid_light(
            rgb(246, 222, 230),
            rgb(253, 236, 242),
            rgb(255, 250, 252),
            rgb(60, 32, 46),
            rgb(128, 92, 110),
            accent,
            rgb(199, 54, 120),
            rgb(208, 36, 72),
            rgb(30, 150, 100),
            rgb(176, 112, 0),
            14.0,
        ),
        Theme::Mint => solid_light(
            rgb(213, 238, 228),
            rgb(232, 246, 240),
            rgb(250, 254, 252),
            rgb(20, 44, 36),
            rgb(78, 110, 98),
            accent,
            rgb(4, 120, 87),
            rgb(200, 40, 50),
            rgb(16, 150, 100),
            rgb(170, 110, 0),
            12.0,
        ),
        Theme::Aurora => glass(
            (8, 14, 26),
            accent,
            rgb(103, 232, 249),
            rgb(238, 242, 255),
            rgb(165, 180, 200),
        ),
        Theme::Sunset => glass(
            (22, 10, 24),
            accent,
            rgb(253, 186, 116),
            rgb(255, 241, 242),
            rgb(205, 170, 190),
        ),
        Theme::Ocean => glass(
            (4, 18, 30),
            accent,
            rgb(125, 211, 252),
            rgb(236, 254, 255),
            rgb(150, 190, 210),
        ),
        Theme::Neon => glass(
            (10, 8, 18),
            accent,
            rgb(240, 171, 252),
            rgb(250, 245, 255),
            rgb(180, 170, 210),
        ),
        Theme::AdwaitaDark => Palette {
            rail: rgb(26, 26, 29),
            side: rgb(46, 46, 50),
            main: rgb(34, 34, 38),
            header: rgb(46, 46, 50),
            elevated: rgb(58, 58, 63),
            hover: rgb(62, 62, 67),
            selected: rgb(70, 70, 76),
            msg_hover: rgb(40, 40, 44),
            text: rgb(246, 246, 247),
            muted: rgb(170, 170, 176),
            faint: rgb(122, 122, 128),
            border: rgb(56, 56, 61),
            accent,
            link: rgb(120, 174, 237),
            danger: rgb(255, 123, 99),
            ok: rgb(143, 240, 164),
            warn: rgb(248, 228, 92),
            radius: 12.0,
            light: false,
        },
        Theme::AdwaitaLight => Palette {
            rail: rgb(222, 222, 226),
            side: rgb(235, 235, 237),
            main: rgb(250, 250, 251),
            header: rgb(235, 235, 237),
            elevated: rgb(255, 255, 255),
            hover: rgb(224, 224, 227),
            selected: rgb(214, 214, 218),
            msg_hover: rgb(242, 242, 244),
            text: rgb(30, 30, 33),
            muted: rgb(94, 94, 100),
            faint: rgb(139, 139, 144),
            border: rgb(218, 218, 222),
            accent,
            link: rgb(28, 113, 216),
            danger: rgb(192, 28, 40),
            ok: rgb(38, 162, 105),
            warn: rgb(156, 110, 3),
            radius: 12.0,
            light: true,
        },
        Theme::Catppuccin => Palette {
            rail: rgb(17, 17, 27),
            side: rgb(24, 24, 37),
            main: rgb(30, 30, 46),
            header: rgb(24, 24, 37),
            elevated: rgb(49, 50, 68),
            hover: rgb(49, 50, 68),
            selected: rgb(69, 71, 90),
            msg_hover: rgb(36, 36, 54),
            text: rgb(205, 214, 244),
            muted: rgb(166, 173, 200),
            faint: rgb(108, 112, 134),
            border: rgb(49, 50, 68),
            accent,
            link: rgb(137, 180, 250),
            danger: rgb(243, 139, 168),
            ok: rgb(166, 227, 161),
            warn: rgb(249, 226, 175),
            radius: 10.0,
            light: false,
        },
        Theme::Nord => Palette {
            rail: rgb(36, 41, 51),
            side: rgb(46, 52, 64),
            main: rgb(51, 57, 70),
            header: rgb(46, 52, 64),
            elevated: rgb(59, 66, 82),
            hover: rgb(67, 76, 94),
            selected: rgb(76, 86, 106),
            msg_hover: rgb(55, 62, 76),
            text: rgb(236, 239, 244),
            muted: rgb(180, 188, 204),
            faint: rgb(118, 129, 150),
            border: rgb(59, 66, 82),
            accent,
            link: rgb(136, 192, 208),
            danger: rgb(191, 97, 106),
            ok: rgb(163, 190, 140),
            warn: rgb(235, 203, 139),
            radius: 10.0,
            light: false,
        },
        Theme::Dark => Palette {
            rail: rgb(16, 17, 21),
            side: rgb(23, 24, 30),
            main: rgb(29, 30, 37),
            header: rgb(29, 30, 37),
            elevated: rgb(38, 39, 48),
            hover: rgb(44, 45, 56),
            selected: rgb(52, 53, 66),
            msg_hover: rgb(33, 34, 42),
            text: rgb(230, 231, 236),
            muted: rgb(150, 152, 165),
            faint: rgb(102, 104, 117),
            border: rgb(44, 45, 54),
            accent,
            link: rgb(96, 170, 255),
            danger: rgb(239, 83, 80),
            ok: rgb(52, 199, 120),
            warn: rgb(240, 178, 50),
            radius: 8.0,
            light: false,
        },
        Theme::Midnight => Palette {
            rail: rgb(0, 0, 0),
            side: rgb(8, 8, 11),
            main: rgb(12, 12, 16),
            header: rgb(12, 12, 16),
            elevated: rgb(24, 24, 31),
            hover: rgb(30, 30, 39),
            selected: rgb(38, 38, 49),
            msg_hover: rgb(18, 18, 24),
            text: rgb(234, 234, 240),
            muted: rgb(140, 142, 154),
            faint: rgb(92, 94, 106),
            border: rgb(30, 30, 38),
            accent,
            link: rgb(96, 170, 255),
            danger: rgb(239, 83, 80),
            ok: rgb(52, 199, 120),
            warn: rgb(240, 178, 50),
            radius: 8.0,
            light: false,
        },
        Theme::Light => Palette {
            rail: rgb(227, 229, 235),
            side: rgb(241, 242, 246),
            main: rgb(255, 255, 255),
            header: rgb(255, 255, 255),
            elevated: rgb(240, 241, 245),
            hover: rgb(231, 233, 239),
            selected: rgb(221, 224, 232),
            msg_hover: rgb(247, 248, 250),
            text: rgb(28, 29, 35),
            muted: rgb(96, 99, 112),
            faint: rgb(140, 143, 155),
            border: rgb(222, 224, 230),
            accent,
            link: rgb(0, 102, 214),
            danger: rgb(214, 48, 49),
            ok: rgb(30, 160, 90),
            warn: rgb(196, 130, 0),
            radius: 8.0,
            light: true,
        },
    }
}

pub fn apply(ctx: &egui::Context, pal: &Palette, _theme: Theme) {
    let mut v = if pal.light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.panel_fill = pal.main;
    v.window_fill = pal.side;
    v.window_stroke = Stroke::new(1.0, pal.border);
    v.extreme_bg_color = pal.elevated;
    v.faint_bg_color = pal.elevated;
    v.code_bg_color = pal.elevated;
    v.hyperlink_color = pal.link;
    v.selection.bg_fill = pal.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, pal.text);
    v.text_cursor.stroke = Stroke::new(2.0, pal.accent);
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.warn_fg_color = pal.warn;
    v.error_fg_color = pal.danger;
    v.window_rounding = Rounding::same(pal.radius + 4.0);
    v.menu_rounding = Rounding::same(pal.radius);
    let shadow_alpha = if pal.light { 40 } else { 110 };
    v.window_shadow = Shadow {
        offset: egui::vec2(0.0, 10.0),
        blur: 32.0,
        spread: 0.0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.popup_shadow = Shadow {
        offset: egui::vec2(0.0, 6.0),
        blur: 20.0,
        spread: 0.0,
        color: Color32::from_black_alpha(shadow_alpha),
    };

    let r = Rounding::same((pal.radius - 2.0).max(6.0));
    v.widgets.noninteractive.rounding = r;
    v.widgets.noninteractive.bg_fill = pal.main;
    v.widgets.noninteractive.weak_bg_fill = pal.main;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);

    v.widgets.inactive.rounding = r;
    v.widgets.inactive.bg_fill = pal.elevated;
    v.widgets.inactive.weak_bg_fill = pal.elevated;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.text);

    v.widgets.hovered.rounding = r;
    v.widgets.hovered.bg_fill = pal.hover;
    v.widgets.hovered.weak_bg_fill = pal.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, mix(pal.hover, pal.accent, 0.35));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, pal.text);
    v.widgets.hovered.expansion = 1.0;

    v.widgets.active.rounding = r;
    v.widgets.active.bg_fill = pal.accent;
    v.widgets.active.weak_bg_fill = pal.accent;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);

    v.widgets.active.expansion = 0.5;

    v.widgets.open.rounding = r;
    v.widgets.open.bg_fill = pal.selected;
    v.widgets.open.weak_bg_fill = pal.selected;
    v.widgets.open.bg_stroke = Stroke::new(1.0, pal.accent.gamma_multiply(0.6));

    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.interact_size.y = 32.0;
        s.animation_time = 0.18;
        // тонкие «плавающие» полосы прокрутки, как в современном Discord
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.spacing.scroll.bar_width = 8.0;
        s.spacing.scroll.floating_allocated_width = 0.0;
        s.spacing.menu_margin = egui::Margin::same(8.0);
        s.spacing.window_margin = egui::Margin::same(14.0);
        s.spacing.text_edit_width = 280.0;
        s.text_styles
            .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
        s.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.5, FontFamily::Proportional),
        );
        s.text_styles.insert(
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        );
        s.text_styles.insert(
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Proportional),
        );
        s.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.5, FontFamily::Monospace),
        );
    });
}
