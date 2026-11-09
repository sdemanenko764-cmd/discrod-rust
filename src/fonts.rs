//! Шрифты. Базовый шрифт egui не знает японского, китайского, «красивых»
//! математических букв (𝓐𝓑𝓒), многих символов — отсюда квадратики □□□.
//! Подключаем системные шрифты Windows. Файлы не читаются целиком в память,
//! а отображаются (mmap): ОС подгружает только реально нужные кусочки.

use std::path::{Path, PathBuf};

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

/// (имя, варианты файлов, индекс шрифта в .ttc, основной шрифт интерфейса?)
const WINDOWS_FONTS: &[(&str, &[&str], u32, bool)] = &[
    ("segoe-ui", &["segoeui.ttf"], 0, true),
    ("segoe-symbol", &["seguisym.ttf"], 0, false),
    ("cambria-math", &["cambria.ttc"], 1, false),
    ("yahei", &["msyh.ttc", "msyh.ttf"], 0, false),
    (
        "yu-gothic",
        &["YuGothM.ttc", "yugothm.ttc", "meiryo.ttc", "msgothic.ttc"],
        0,
        false,
    ),
    ("malgun", &["malgun.ttf"], 0, false),
    ("nirmala", &["Nirmala.ttc", "Nirmala.ttf"], 0, false),
    ("ebrima", &["ebrima.ttf"], 0, false),
    ("segoe-historic", &["seguihis.ttf"], 0, false),
    ("tahoma", &["tahoma.ttf"], 0, false),
    ("segoe-emoji", &["seguiemj.ttf"], 0, false),
];

fn fonts_dir() -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("Fonts")
}

/// Отображает файл шрифта в память и проверяет, что egui сможет его прочитать
/// (иначе egui упадёт при старте).
fn map_font(path: &Path, index: u32) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: системные шрифты не меняются, пока программа работает.
    let mmap = unsafe { memmap2::Mmap::map(&file).ok()? };
    ab_glyph::FontRef::try_from_slice_and_index(&mmap[..], index).ok()?;
    let leaked: &'static memmap2::Mmap = Box::leak(Box::new(mmap));
    Some(&leaked[..])
}

/// Inter — современный шрифт интерфейса с полной кириллицей (SIL OFL 1.1,
/// лицензия в assets/fonts/Inter-LICENSE.txt). Встроен в программу.
static INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
static INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

/// `inter` — использовать встроенный Inter (иначе системный Segoe UI).
pub fn install(ctx: &egui::Context, inter: bool) {
    let mut fonts = FontDefinitions::default();
    let dir = fonts_dir();
    let mut primary: Vec<String> = Vec::new();
    let mut fallback: Vec<String> = Vec::new();

    for (name, files, index, is_primary) in WINDOWS_FONTS {
        for file in *files {
            if let Some(bytes) = map_font(&dir.join(file), *index) {
                let mut data = FontData::from_static(bytes);
                data.index = *index;
                fonts.font_data.insert((*name).to_string(), data);
                if *is_primary {
                    primary.push((*name).to_string());
                } else {
                    fallback.push((*name).to_string());
                }
                break;
            }
        }
    }

    if inter {
        fonts
            .font_data
            .insert("inter".into(), FontData::from_static(INTER_REGULAR));
        fonts.font_data.insert(
            "inter-semibold".into(),
            FontData::from_static(INTER_SEMIBOLD),
        );
        primary.insert(0, "inter".into());
    }
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        for (i, name) in primary.iter().enumerate() {
            list.insert(i, name.clone());
        }
        list.extend(fallback.iter().cloned());
    }
    // Жирное начертание для **текста** и заголовков: Segoe UI Bold, а если его нет —
    // обычный шрифт (семейство должно существовать всегда, иначе egui упадёт).
    let mut bold = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if inter {
        bold.insert(0, "inter-semibold".into());
    } else {
        for file in ["seguisb.ttf", "segoeuib.ttf"] {
            if let Some(bytes) = map_font(&dir.join(file), 0) {
                fonts
                    .font_data
                    .insert("segoe-bold".into(), FontData::from_static(bytes));
                bold.insert(0, "segoe-bold".into());
                break;
            }
        }
    }
    fonts.families.insert(FontFamily::Name("bold".into()), bold);
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        // Inter в моноширинном семействе не нужен — только запасные глифы
        list.extend(primary.iter().filter(|n| *n != "inter").cloned());
        list.extend(fallback.iter().cloned());
    }
    ctx.set_fonts(fonts);
}
