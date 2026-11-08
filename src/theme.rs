//! Оформление: палитры тем и стиль виджетов.

use eframe::egui::{self, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

use crate::settings::{Settings, Theme};

pub const ACCENTS: [(&str, Color32); 8] = [
    ("Ржавчина", Color32::from_rgb(232, 117, 58)),
    ("Фиалка", Color32::from_rgb(139, 92, 246)),
    ("Небо", Color32::from_rgb(59, 130, 246)),
    ("Мята", Color32::from_rgb(34, 197, 140)),
    ("Фуксия", Color32::from_rgb(236, 72, 153)),
    ("Коралл", Color32::from_rgb(244, 88, 88)),
    ("Бирюза", Color32::from_rgb(20, 184, 196)),
    ("Золото", Color32::from_rgb(234, 179, 8)),
];

#[derive(Clone, Copy)]
pub struct Palette {
    pub rail: Color32,
    pub side: Color32,
    pub main: Color32,
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
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub fn palette(s: &Settings) -> Palette {
    let accent = ACCENTS[s.accent.min(ACCENTS.len() - 1)].1;
    match s.theme {
        Theme::Dark => Palette {
            rail: rgb(16, 17, 21),
            side: rgb(23, 24, 30),
            main: rgb(29, 30, 37),
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
        },
        Theme::Midnight => Palette {
            rail: rgb(0, 0, 0),
            side: rgb(8, 8, 11),
            main: rgb(12, 12, 16),
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
        },
        Theme::Light => Palette {
            rail: rgb(227, 229, 235),
            side: rgb(241, 242, 246),
            main: rgb(255, 255, 255),
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
        },
    }
}

pub fn apply(ctx: &egui::Context, pal: &Palette, theme: Theme) {
    let mut v = if theme == Theme::Light {
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
    v.window_rounding = Rounding::same(14.0);
    v.menu_rounding = Rounding::same(10.0);

    let r = Rounding::same(8.0);
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
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.accent.gamma_multiply(0.6));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, pal.text);

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
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
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
