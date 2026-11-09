//! Обновление из самого приложения: проверка последнего релиза на GitHub,
//! загрузка нового rustcord.exe и замена запущенного файла.
//!
//! Номер сборки вшивается при сборке в GitHub Actions (переменная RUSTCORD_BUILD
//! = номер запуска сборки), релизы называются build-<номер>. Запущенный .exe в
//! Windows нельзя перезаписать, но можно переименовать: старый файл становится
//! rustcord.old.exe (удаляется при следующем запуске), новый встаёт на его место.

use std::path::PathBuf;

use serde::Deserialize;

/// Репозиторий, из которого берутся обновления.
pub const REPO: &str = "sdemanenko764-cmd/discrod-rust";

/// Номер этой сборки (None — собрано вручную, не в GitHub Actions).
pub fn current_build() -> Option<u64> {
    option_env!("RUSTCORD_BUILD").and_then(|b| b.trim().parse().ok())
}

pub fn current_label() -> String {
    match current_build() {
        Some(b) => format!("сборка {b}"),
        None => format!("версия {} (собрана вручную)", env!("CARGO_PKG_VERSION")),
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub build: u64,
    pub url: String,
    pub page: String,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

/// Последний релиз с rustcord.exe, если он новее текущей сборки.
pub async fn check(http: &reqwest::Client) -> Result<Option<Release>, String> {
    let r: RawRelease = http
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("User-Agent", "Rustcord-updater")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("нет связи с GitHub: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GitHub ответил ошибкой: {e}"))?
        .json()
        .await
        .map_err(|e| format!("неожиданный ответ GitHub: {e}"))?;
    let build: u64 = r
        .tag_name
        .trim_start_matches("build-")
        .parse()
        .map_err(|_| format!("непонятный номер релиза «{}»", r.tag_name))?;
    let Some(asset) = r.assets.iter().find(|a| a.name == "rustcord.exe") else {
        return Ok(None);
    };
    let newer = current_build().map_or(true, |cur| build > cur);
    Ok(newer.then(|| Release {
        build,
        url: asset.browser_download_url.clone(),
        page: r.html_url,
    }))
}

fn exe_paths() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe
        .parent()
        .ok_or("не найдена папка программы")?
        .to_path_buf();
    Ok((
        exe,
        dir.join("rustcord.new.exe"),
        dir.join("rustcord.old.exe"),
    ))
}

/// Скачивает новую версию и ставит её на место текущей. После успеха нужно перезапуститься.
pub async fn download_and_replace(http: &reqwest::Client, url: &str) -> Result<PathBuf, String> {
    let (exe, new_path, old_path) = exe_paths()?;
    let bytes = http
        .get(url)
        .header("User-Agent", "Rustcord-updater")
        .send()
        .await
        .map_err(|e| format!("не скачалось: {e}"))?
        .error_for_status()
        .map_err(|e| format!("не скачалось: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("не скачалось: {e}"))?;
    // простая проверка, что это действительно программа Windows, а не страница с ошибкой
    if bytes.len() < 1_000_000 || &bytes[..2] != b"MZ" {
        return Err("скачанный файл не похож на программу".into());
    }
    std::fs::write(&new_path, &bytes).map_err(|e| {
        format!(
            "нет прав записи в папку программы ({e}). Если Rustcord установлен в Program Files, \
             скачайте установщик со страницы релиза"
        )
    })?;
    let _ = std::fs::remove_file(&old_path);
    std::fs::rename(&exe, &old_path).map_err(|e| format!("не удалось заменить файл: {e}"))?;
    if let Err(e) = std::fs::rename(&new_path, &exe) {
        // откатываем, чтобы программа не пропала
        let _ = std::fs::rename(&old_path, &exe);
        return Err(format!("не удалось заменить файл: {e}"));
    }
    Ok(exe)
}

/// Удаляет файл предыдущей версии, оставшийся после обновления.
pub fn cleanup_old() {
    if let Ok((_, new_path, old_path)) = exe_paths() {
        let _ = std::fs::remove_file(old_path);
        let _ = std::fs::remove_file(new_path);
    }
}

/// Запускает новую версию и завершает текущую.
pub fn restart(exe: &std::path::Path) -> ! {
    let _ = std::process::Command::new(exe).spawn();
    std::process::exit(0);
}
