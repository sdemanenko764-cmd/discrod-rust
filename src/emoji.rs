//! Эмодзи: цветные картинки Twemoji (CC-BY 4.0) вместо чёрно-белых символов шрифта,
//! разбор строки на текст / ссылки / эмодзи и набор для панели выбора.

pub const CATEGORIES: &[(&str, &str)] = &[
    (
        "Смайлы",
        "😀 😃 😄 😁 😆 😅 🤣 😂 🙂 🙃 😉 😊 😇 🥰 😍 🤩 😘 😗 😚 😙 😋 😛 😜 🤪 😝 🤑 🤗 🤭 🤫 🤔 \
         🤐 🤨 😐 😑 😶 😏 😒 🙄 😬 😮‍💨 🤥 😌 😔 😪 🤤 😴 😷 🤒 🤕 🤢 🤮 🥵 🥶 🥴 😵 🤯 🤠 🥳 😎 🤓 \
         🧐 😕 😟 🙁 😮 😯 😲 😳 🥺 😦 😧 😨 😰 😥 😢 😭 😱 😖 😣 😞 😓 😩 😫 🥱 😤 😡 😠 🤬 😈 👿 \
         💀 ☠️ 💩 🤡 👹 👺 👻 👽 🤖 😺 😸 😹 😻 😼 😽 🙀 😿 😾",
    ),
    (
        "Жесты",
        "👍 👎 👌 🤌 🤏 ✌️ 🤞 🤟 🤘 🤙 👈 👉 👆 👇 ☝️ ✋ 🤚 🖐️ 🖖 👋 👏 🙌 👐 🤲 🤝 🙏 ✍️ 💪 🦾 🫶 \
         👀 👁️ 👅 👄 🧠 🫡 🫠 🫣 🫢 🤷 🤦 🙋 🙇 💃 🕺",
    ),
    (
        "Сердечки",
        "❤️ 🧡 💛 💚 💙 💜 🖤 🤍 🤎 💔 ❤️‍🔥 ❣️ 💕 💞 💓 💗 💖 💘 💝 💟 💯 💢 💥 💫 💦 💨 🔥 ✨ ⭐ 🌟 \
         ⚡ 💤 💬 🗯️ 💭",
    ),
    (
        "Животные и еда",
        "🐶 🐱 🐭 🐹 🐰 🦊 🐻 🐼 🐨 🐯 🦁 🐮 🐷 🐸 🐵 🙈 🙉 🙊 🐔 🐧 🐦 🦆 🦉 🐺 🐗 🐴 🦄 🐝 🦋 🐌 \
         🐍 🐢 🐙 🦈 🐬 🐳 🍏 🍎 🍌 🍉 🍇 🍓 🍒 🍑 🍍 🥑 🍆 🌶️ 🍞 🧀 🍔 🍟 🍕 🌭 🌮 🍜 🍣 🍩 🍪 🎂 \
         🍫 🍿 ☕ 🍵 🧃 🥤 🍺 🍷 🥂",
    ),
    (
        "Разное",
        "🎉 🎊 🎁 🎈 🏆 🥇 ⚽ 🏀 🎮 🕹️ 🎲 🎯 🎵 🎶 🎧 🎤 📱 💻 ⌨️ 🖥️ 📷 💡 📚 ✏️ 📌 📎 🔒 🔑 💰 💸 \
         🚀 ✈️ 🚗 🏠 🌍 🌙 ☀️ ⛅ 🌧️ ❄️ 🌈 🌸 🌹 🌻 🍀 ✅ ❌ ❓ ❗ ⚠️ 🚫 ➕ ➖ 🆗 🆒 🆕 🔞 🇷🇺 🏳️‍🌈",
    ),
];

/// Ссылка на цветную картинку эмодзи (набор Twemoji, лицензия CC-BY 4.0).
pub fn twemoji_url(emoji: &str) -> String {
    let normalized = if emoji.contains('\u{200D}') {
        emoji.to_string()
    } else {
        emoji.replace('\u{FE0F}', "")
    };
    let code: Vec<String> = normalized
        .chars()
        .map(|c| format!("{:x}", c as u32))
        .collect();
    format!(
        "https://cdn.jsdelivr.net/gh/jdecked/twemoji@latest/assets/72x72/{}.png",
        code.join("-")
    )
}

fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

fn is_skin_tone(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

/// Начинает ли символ эмодзи (текстовые ©, ®, ™ и т.п. оставляем текстом).
pub fn is_emoji_start(c: char) -> bool {
    matches!(c as u32,
        0x1F300..=0x1FAFF
        | 0x1F000..=0x1F2FF
        | 0x2600..=0x27BF
        | 0x2B05..=0x2B07
        | 0x2B1B | 0x2B1C | 0x2B50 | 0x2B55
        | 0x231A | 0x231B | 0x2328 | 0x23CF
        | 0x23E9..=0x23FA
        | 0x2934 | 0x2935 | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

pub enum Seg<'a> {
    Text(&'a str),
    Link(&'a str),
    Emoji(&'a str),
    Custom {
        name: &'a str,
        id: u64,
        animated: bool,
    },
}

/// Нужно ли рисовать строку по кусочкам (есть ссылки или эмодзи).
pub fn needs_rich(line: &str) -> bool {
    line.contains("http://")
        || line.contains("https://")
        || line.contains("<:")
        || line.contains("<a:")
        || line.chars().any(|c| is_emoji_start(c) || is_regional(c))
}

/// Длина (в байтах) одного эмодзи-кластера, начинающегося в начале строки.
fn cluster_len(s: &str) -> usize {
    let mut chars = s.char_indices().peekable();
    let Some((_, first)) = chars.next() else {
        return 0;
    };
    let mut end = first.len_utf8();
    if is_regional(first) {
        if let Some((i, c)) = chars.peek().copied() {
            if is_regional(c) {
                return i + c.len_utf8();
            }
        }
        return end;
    }
    while let Some((i, c)) = chars.peek().copied() {
        if c == '\u{FE0F}'
            || c == '\u{20E3}'
            || is_skin_tone(c)
            || ('\u{E0020}'..='\u{E007F}').contains(&c)
        {
            chars.next();
            end = i + c.len_utf8();
        } else if c == '\u{200D}' {
            chars.next();
            match chars.next() {
                Some((j, n)) => end = j + n.len_utf8(),
                None => {
                    end = i + c.len_utf8();
                    break;
                }
            }
        } else {
            break;
        }
    }
    end
}

/// `<:name:123>` / `<a:name:123>` в начале строки → (длина, имя, id, анимированный)
fn parse_custom(s: &str) -> Option<(usize, &str, u64, bool)> {
    let close = s.find('>')?;
    let inner = &s[1..close];
    let (animated, body) = match inner.strip_prefix("a:") {
        Some(b) => (true, b),
        None => (false, inner.strip_prefix(':')?),
    };
    let (name, id) = body.split_once(':')?;
    let id: u64 = id.parse().ok()?;
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    Some((close + 1, name, id, animated))
}

pub fn segment(line: &str) -> Vec<Seg<'_>> {
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let (len, seg) = if rest.starts_with("http://") || rest.starts_with("https://") {
            let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (len, Some(Seg::Link(&rest[..len])))
        } else if rest.starts_with("<:") || rest.starts_with("<a:") {
            match parse_custom(rest) {
                Some((len, name, id, animated)) => (len, Some(Seg::Custom { name, id, animated })),
                None => (c.len_utf8(), None),
            }
        } else if is_emoji_start(c) || is_regional(c) {
            let len = cluster_len(rest).max(c.len_utf8());
            (len, Some(Seg::Emoji(&rest[..len])))
        } else {
            (c.len_utf8(), None)
        };
        if let Some(seg) = seg {
            if text_start < i {
                out.push(Seg::Text(&line[text_start..i]));
            }
            out.push(seg);
            text_start = i + len;
        }
        i += len;
    }
    if text_start < line.len() {
        out.push(Seg::Text(&line[text_start..]));
    }
    out
}

/// Сообщение только из эмодзи (до 20 штук) — показываем крупно, как в Discord.
pub fn is_jumbo(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return false;
    }
    let segs = segment(trimmed);
    let mut count = 0;
    for s in &segs {
        match s {
            Seg::Emoji(_) | Seg::Custom { .. } => count += 1,
            Seg::Text(t) if t.trim().is_empty() => {}
            _ => return false,
        }
    }
    count > 0 && count <= 20
}
