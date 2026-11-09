//! Разметка сообщений Discord: **жирный**, *курсив*, __подчёркнутый__, ~~зачёркнутый~~,
//! `код`, ||спойлер||, заголовки, цитаты, списки, маленький текст (-#),
//! [ссылки](https://…), упоминания <@id> <@&id> <#id>, время <t:…:R> и эмодзи.
//!
//! Разбор ничего не копирует: все куски — срезы исходной строки.

use crate::emoji;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub spoiler: bool,
}

#[derive(Debug)]
pub enum Inline<'a> {
    Text(&'a str, Style),
    Code(&'a str, Style),
    Link {
        label: &'a str,
        url: &'a str,
        style: Style,
    },
    Emoji(&'a str, Style),
    Custom {
        name: &'a str,
        id: u64,
        animated: bool,
        style: Style,
    },
    User(u64, Style),
    Role(u64, Style),
    Channel(u64, Style),
    Everyone(&'a str, Style),
    /// Время: секунды Unix и формат (t, T, d, D, f, F, R).
    Time(i64, char, Style),
}

impl Inline<'_> {
    pub fn style(&self) -> Style {
        match self {
            Inline::Text(_, s)
            | Inline::Code(_, s)
            | Inline::Emoji(_, s)
            | Inline::User(_, s)
            | Inline::Role(_, s)
            | Inline::Channel(_, s)
            | Inline::Everyone(_, s)
            | Inline::Time(_, _, s) => *s,
            Inline::Link { style, .. } | Inline::Custom { style, .. } => *style,
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LineKind {
    Plain,
    H1,
    H2,
    H3,
    Sub,
    Quote,
    Bullet(u8),
    Numbered,
}

#[derive(Debug)]
pub enum Block<'a> {
    Line {
        kind: LineKind,
        /// префикс для нумерованного списка («1.»)
        number: &'a str,
        quoted: bool,
        text: &'a str,
    },
    Code {
        lang: &'a str,
        code: &'a str,
    },
    Blank,
}

/// Делит текст на строки и блоки кода.
pub fn blocks(content: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut quote_rest = false;
    for (i, part) in content.split("```").enumerate() {
        if i % 2 == 1 {
            let (lang, code) = match part.split_once('\n') {
                Some((l, rest)) if !l.trim().contains(' ') && !l.trim().is_empty() => {
                    (l.trim(), rest)
                }
                Some((l, rest)) if l.trim().is_empty() => ("", rest),
                _ => ("", part),
            };
            out.push(Block::Code {
                lang,
                code: code.trim_end_matches('\n'),
            });
            continue;
        }
        let part = if i > 0 {
            part.strip_prefix('\n').unwrap_or(part)
        } else {
            part
        };
        let part = part.strip_suffix('\n').unwrap_or(part);
        if part.is_empty() {
            continue;
        }
        for line in part.split('\n') {
            let mut text = line;
            let mut quoted = quote_rest;
            if let Some(rest) = text.strip_prefix(">>> ") {
                quote_rest = true;
                quoted = true;
                text = rest;
            } else if let Some(rest) = text.strip_prefix("> ") {
                quoted = true;
                text = rest;
            } else if text == ">" {
                quoted = true;
                text = "";
            }
            if text.trim().is_empty() {
                if quoted {
                    out.push(Block::Line {
                        kind: LineKind::Quote,
                        number: "",
                        quoted,
                        text: "",
                    });
                } else {
                    out.push(Block::Blank);
                }
                continue;
            }
            let (kind, number, text) = line_kind(text);
            out.push(Block::Line {
                kind,
                number,
                quoted,
                text,
            });
        }
    }
    out
}

fn line_kind(text: &str) -> (LineKind, &str, &str) {
    if let Some(r) = text.strip_prefix("### ") {
        return (LineKind::H3, "", r);
    }
    if let Some(r) = text.strip_prefix("## ") {
        return (LineKind::H2, "", r);
    }
    if let Some(r) = text.strip_prefix("# ") {
        return (LineKind::H1, "", r);
    }
    if let Some(r) = text.strip_prefix("-# ") {
        return (LineKind::Sub, "", r);
    }
    let trimmed = text.trim_start();
    let indent = ((text.len() - trimmed.len()) / 2).min(3) as u8;
    if let Some(r) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return (LineKind::Bullet(indent), "", r);
    }
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits < 4 && trimmed[digits..].starts_with(". ") {
        return (
            LineKind::Numbered,
            &trimmed[..digits + 1],
            &trimmed[digits + 2..],
        );
    }
    (LineKind::Plain, "", text)
}

/// Есть ли в строке что-то, кроме простого текста.
pub fn is_plain(line: &str) -> bool {
    !line.bytes().any(|b| {
        matches!(
            b,
            b'*' | b'_' | b'~' | b'|' | b'`' | b'<' | b'[' | b'@' | b'\\' | b':'
        )
    }) && !emoji::needs_rich(line)
}

/// `<…>`: упоминание, канал, эмодзи, время или ссылка.
fn angle<'a>(rest: &'a str, style: Style) -> Option<(usize, Inline<'a>)> {
    let close = rest.find('>')?;
    let inner = &rest[1..close];
    let len = close + 1;
    let num = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    if let Some(id) = inner.strip_prefix("@&") {
        return Some((len, Inline::Role(num(id)?, style)));
    }
    if let Some(id) = inner.strip_prefix("@!").or_else(|| inner.strip_prefix('@')) {
        return Some((len, Inline::User(num(id)?, style)));
    }
    if let Some(id) = inner.strip_prefix('#') {
        return Some((len, Inline::Channel(num(id)?, style)));
    }
    if let Some(ts) = inner.strip_prefix("t:") {
        let (secs, fmt) = match ts.split_once(':') {
            Some((s, f)) => (s, f.chars().next().unwrap_or('f')),
            None => (ts, 'f'),
        };
        return Some((len, Inline::Time(secs.parse().ok()?, fmt, style)));
    }
    if inner.starts_with(':') || inner.starts_with("a:") {
        let animated = inner.starts_with("a:");
        let body = inner.trim_start_matches("a:").trim_start_matches(':');
        let (name, id) = body.split_once(':')?;
        if name.is_empty() || name.contains(' ') {
            return None;
        }
        return Some((
            len,
            Inline::Custom {
                name,
                id: id.parse().ok()?,
                animated,
                style,
            },
        ));
    }
    if inner.starts_with("http://") || inner.starts_with("https://") {
        if inner.contains(char::is_whitespace) {
            return None;
        }
        return Some((
            len,
            Inline::Link {
                label: inner,
                url: inner,
                style,
            },
        ));
    }
    None
}

/// Маркеры оформления в порядке проверки (длинные раньше коротких).
const MARKERS: [&str; 6] = ["**", "__", "~~", "||", "*", "_"];

fn toggle(style: &mut Style, marker: &str) {
    match marker {
        "**" => style.bold = !style.bold,
        "__" => style.underline = !style.underline,
        "~~" => style.strike = !style.strike,
        "||" => style.spoiler = !style.spoiler,
        _ => style.italic = !style.italic,
    }
}

fn is_on(style: &Style, marker: &str) -> bool {
    match marker {
        "**" => style.bold,
        "__" => style.underline,
        "~~" => style.strike,
        "||" => style.spoiler,
        _ => style.italic,
    }
}

/// Разбирает одну строку на куски с оформлением.
pub fn inline(line: &str) -> Vec<Inline<'_>> {
    let mut out = Vec::new();
    let mut style = Style::default();
    // какой из маркеров курсива открыл курсив
    let mut italic_marker: &str = "";
    let mut text_start = 0;
    let mut i = 0;
    let bytes = line.as_bytes();

    macro_rules! flush {
        ($end:expr) => {
            if text_start < $end {
                out.push(Inline::Text(&line[text_start..$end], style));
            }
        };
    }

    while i < line.len() {
        let rest = &line[i..];
        let b = bytes[i];
        // экранирование: \* → *
        if b == b'\\' && rest.len() > 1 {
            let next = rest[1..].chars().next().unwrap_or(' ');
            if next.is_ascii_punctuation() {
                flush!(i);
                text_start = i + 1;
                i += 1 + next.len_utf8();
                continue;
            }
        }
        if b == b'`' {
            let ticks = if rest.starts_with("``") { 2 } else { 1 };
            let pat = &rest[..ticks];
            if let Some(end) = rest[ticks..].find(pat) {
                flush!(i);
                let code = &rest[ticks..ticks + end];
                out.push(Inline::Code(code.trim_matches(' '), style));
                i += ticks + end + ticks;
                text_start = i;
                continue;
            }
        }
        if b == b'<' {
            if let Some((len, item)) = angle(rest, style) {
                flush!(i);
                out.push(item);
                i += len;
                text_start = i;
                continue;
            }
        }
        if b == b'[' {
            // [подпись](https://ссылка)
            if let Some(mid) = rest.find("](") {
                let label = &rest[1..mid];
                let after = &rest[mid + 2..];
                if !label.is_empty()
                    && !label.contains('\n')
                    && (after.starts_with("http://") || after.starts_with("https://"))
                {
                    if let Some(close) = after.find(')') {
                        let url = after[..close].trim_matches(|c| c == '<' || c == '>');
                        if !url.contains(char::is_whitespace) {
                            flush!(i);
                            out.push(Inline::Link { label, url, style });
                            i += mid + 2 + close + 1;
                            text_start = i;
                            continue;
                        }
                    }
                }
            }
        }
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let mut len = rest.find(char::is_whitespace).unwrap_or(rest.len());
            // разметка вокруг ссылки (||спойлер||, **жирный**) — не часть адреса
            for m in ["||", "**", "__", "~~", ">"] {
                if let Some(p) = rest[..len].find(m) {
                    len = len.min(p);
                }
            }
            // хвостовые знаки препинания не часть ссылки
            while len > 0
                && matches!(
                    rest.as_bytes()[len - 1],
                    b'.' | b','
                        | b')'
                        | b'!'
                        | b'?'
                        | b'*'
                        | b'_'
                        | b'~'
                        | b'|'
                        | b':'
                        | b';'
                        | b'"'
                        | b'\''
                )
            {
                len -= 1;
            }
            flush!(i);
            out.push(Inline::Link {
                label: &rest[..len],
                url: &rest[..len],
                style,
            });
            i += len;
            text_start = i;
            continue;
        }
        if b == b'@' && (rest.starts_with("@everyone") || rest.starts_with("@here")) {
            let len = if rest.starts_with("@everyone") { 9 } else { 5 };
            flush!(i);
            out.push(Inline::Everyone(&rest[..len], style));
            i += len;
            text_start = i;
            continue;
        }
        if matches!(b, b'*' | b'_' | b'~' | b'|') {
            let mut matched = None;
            for m in MARKERS {
                if !rest.starts_with(m) {
                    continue;
                }
                let single = m.len() == 1;
                if single && is_on(&style, m) && italic_marker != m {
                    continue;
                }
                let after = &rest[m.len()..];
                let opening = !is_on(&style, m);
                if opening {
                    // открываем только если есть закрывающий и после маркера не пробел
                    if after.starts_with(' ') || after.is_empty() || !after.contains(m) {
                        if m.len() == 2 {
                            break; // «**» без пары — это просто звёздочки, не курсив
                        }
                        continue;
                    }
                    if m == "_" {
                        let prev = line[..i].chars().last();
                        if prev.map_or(false, |c| c.is_alphanumeric()) {
                            continue;
                        }
                    }
                } else if m == "_" {
                    let next = after.chars().next();
                    if next.map_or(false, |c| c.is_alphanumeric()) {
                        continue;
                    }
                }
                matched = Some(m);
                break;
            }
            if let Some(m) = matched {
                flush!(i);
                if m.len() == 1 {
                    italic_marker = if style.italic { "" } else { m };
                }
                toggle(&mut style, m);
                i += m.len();
                text_start = i;
                continue;
            }
            // одинокие маркеры остаются текстом целиком
            i += rest.bytes().take_while(|x| *x == b).count();
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        if emoji::is_emoji_start(c) || is_regional(c) {
            let segs = emoji::segment(rest);
            if let Some(emoji::Seg::Emoji(e)) = segs.first() {
                flush!(i);
                out.push(Inline::Emoji(e, style));
                i += e.len();
                text_start = i;
                continue;
            }
        }
        i += c.len_utf8();
    }
    flush!(line.len());
    out
}

fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Подпись для времени `<t:…:fmt>`.
pub fn format_time(secs: i64, fmt: char) -> String {
    use chrono::{Datelike, TimeZone};
    let Some(dt) = chrono::Local.timestamp_opt(secs, 0).single() else {
        return "?".into();
    };
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    const DAYS: [&str; 7] = [
        "понедельник",
        "вторник",
        "среда",
        "четверг",
        "пятница",
        "суббота",
        "воскресенье",
    ];
    let month = MONTHS[dt.month0() as usize];
    let long_date = format!("{} {} {} г.", dt.day(), month, dt.year());
    match fmt {
        't' => dt.format("%H:%M").to_string(),
        'T' => dt.format("%H:%M:%S").to_string(),
        'd' => dt.format("%d.%m.%Y").to_string(),
        'D' => long_date,
        'F' => format!(
            "{}, {} {}",
            DAYS[dt.weekday().num_days_from_monday() as usize],
            long_date,
            dt.format("%H:%M")
        ),
        'R' => relative(secs),
        _ => format!("{} {}", long_date, dt.format("%H:%M")),
    }
}

fn plural(n: i64, one: &str, few: &str, many: &str) -> String {
    let (n10, n100) = (n % 10, n % 100);
    let word = if n10 == 1 && n100 != 11 {
        one
    } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
        few
    } else {
        many
    };
    format!("{n} {word}")
}

/// «через 5 минут», «3 часа назад».
pub fn relative(secs: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = secs - now;
    let a = diff.abs();
    let text = if a < 45 {
        return if diff >= 0 {
            "через несколько секунд".into()
        } else {
            "только что".into()
        };
    } else if a < 3600 {
        plural((a + 30) / 60, "минуту", "минуты", "минут")
    } else if a < 86_400 {
        plural((a + 1800) / 3600, "час", "часа", "часов")
    } else if a < 30 * 86_400 {
        plural((a + 43_200) / 86_400, "день", "дня", "дней")
    } else if a < 365 * 86_400 {
        plural(a / (30 * 86_400), "месяц", "месяца", "месяцев")
    } else {
        plural(a / (365 * 86_400), "год", "года", "лет")
    };
    if diff >= 0 {
        format!("через {text}")
    } else {
        format!("{text} назад")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles() {
        let v = inline("a **b** *c* __d__ ~~e~~ ||f|| `g` snake_case 2*3");
        let bold = v
            .iter()
            .any(|x| matches!(x, Inline::Text("b", s) if s.bold));
        assert!(bold);
        assert!(v
            .iter()
            .any(|x| matches!(x, Inline::Text("c", s) if s.italic)));
        assert!(v.iter().any(|x| matches!(x, Inline::Code("g", _))));
        assert!(v
            .iter()
            .any(|x| matches!(x, Inline::Text(t, s) if t.contains("snake_case") && !s.italic)));
    }

    #[test]
    fn mentions() {
        let v = inline("hi <@123> <@&5> <#9> <t:0:R> @everyone");
        assert!(v.iter().any(|x| matches!(x, Inline::User(123, _))));
        assert!(v.iter().any(|x| matches!(x, Inline::Role(5, _))));
        assert!(v.iter().any(|x| matches!(x, Inline::Channel(9, _))));
        assert!(v.iter().any(|x| matches!(x, Inline::Time(0, 'R', _))));
        assert!(v.iter().any(|x| matches!(x, Inline::Everyone(_, _))));
    }

    #[test]
    fn link_inside_markup() {
        // ссылка в спойлере и жирном: маркеры не должны попадать в адрес
        let v = inline("**tg - ||https://t.me/heavenless97||**");
        assert!(v.iter().any(
            |x| matches!(x, Inline::Link { url, style, .. } if *url == "https://t.me/heavenless97" && style.spoiler && style.bold)
        ));
        assert!(!v
            .iter()
            .any(|x| matches!(x, Inline::Text(t, _) if t.contains('|') || t.contains('*'))));
        let v = inline("смотри https://example.com/a_b.");
        assert!(v
            .iter()
            .any(|x| matches!(x, Inline::Link { url, .. } if *url == "https://example.com/a_b")));
    }
}
