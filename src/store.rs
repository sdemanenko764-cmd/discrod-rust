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

fn log_path() -> Option<PathBuf> {
    Some(path()?.with_file_name("log.txt"))
}

pub fn log_path_display() -> String {
    log_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "недоступен".into())
}

/// Дописывает строку в лог (%APPDATA%\Rustcord\log.txt). Лог не растёт больше ~1 МБ.
/// Токен в лог никогда не пишется.
pub fn log(line: &str) {
    use std::io::Write;
    let Some(p) = log_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&p)
        .map(|m| m.len() > 1_000_000)
        .unwrap_or(false)
    {
        let _ = fs::remove_file(&p);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "[{now}] {line}");
    }
}

fn proxy_path() -> Option<PathBuf> {
    Some(path()?.with_file_name("proxy.txt"))
}

pub fn load_proxy() -> String {
    proxy_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn save_proxy(proxy: &str) {
    if let Some(p) = proxy_path() {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(p, proxy);
    }
}
