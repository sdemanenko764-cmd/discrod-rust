//! Сохранение сессии между запусками.
//! Windows: %APPDATA%\Rustcord\session.txt, Linux/macOS: ~/.config/Rustcord/session.txt

use std::{fs, path::PathBuf};

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("Rustcord").join("session.txt"))
}

/// (токен, это_бот)
pub fn load() -> Option<(String, bool)> {
    let text = fs::read_to_string(path()?).ok()?;
    let mut lines = text.lines();
    let kind = lines.next()?.trim().to_string();
    let token = lines.next()?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some((token, kind == "bot"))
}

pub fn save(token: &str, bot: bool) {
    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let kind = if bot { "bot" } else { "user" };
        let _ = fs::write(p, format!("{kind}\n{token}\n"));
    }
}

pub fn clear() {
    if let Some(p) = path() {
        let _ = fs::remove_file(p);
    }
}
