//! Оформление: темы (в т.ч. в стиле GNOME / libadwaita) и стиль виджетов.

use eframe::egui::{
    self, epaint::Shadow, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle,
};

use crate::settings::{Settings, Theme};

/// Акцентные цвета: первый — фирменный цвет Rustcord, остальные — палитра акцентов GNOME.
pub const ACCENTS: [(&str, Color32); 10] = [
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
];

/// Темы в порядке показа в настройках.
pub const THEMES: [(Theme, &str); 7] = [
    (Theme::AdwaitaDark, "GNOME тёмная"),
    (Theme::AdwaitaLight, "GNOME светлая"),
    (Theme::Catppuccin, "Catppuccin"),
    (Theme::Nord, "Nord"),
    (Theme::Dark, "Rustcord"),
    (Theme::Midnight, "AMOLED"),
    (Theme::Light, "Светлая"),
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
    palette_for(s.theme, ACCENTS[s.accent.min(ACCENTS.len() - 1)].1)
}

pub fn palette_for(theme: Theme, accent: Color32) -> Palette {
    match theme {
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
    v.selection.bg_fill = pal.accent.gamma_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0, pal.text);
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
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, pal.text);
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.rounding = r;
    v.widgets.active.bg_fill = pal.accent;
    v.widgets.active.weak_bg_fill = pal.accent;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);

    v.widgets.open.rounding = r;
    v.widgets.open.bg_fill = pal.selected;
    v.widgets.open.weak_bg_fill = pal.selected;

    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.interact_size.y = 32.0;
        s.animation_time = 0.16;
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
