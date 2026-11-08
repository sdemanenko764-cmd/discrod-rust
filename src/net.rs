//! Подключение к серверам Discord напрямую или через прокси (SOCKS5 / HTTP CONNECT).
//! Нужно там, где Discord доступен только через VPN-клиент с локальным прокси.

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub type BoxErr = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Debug)]
pub enum Proxy {
    Socks5(String),
    Http(String),
}

impl Proxy {
    /// `socks5://host:port`, `http://host:port` или просто `host:port` (= SOCKS5).
    pub fn parse(s: &str) -> Option<Proxy> {
        let s = s.trim().trim_end_matches('/');
        if s.is_empty() {
            return None;
        }
        let lower = s.to_ascii_lowercase();
        for (prefix, socks) in [
            ("socks5h://", true),
            ("socks5://", true),
            ("socks://", true),
            ("http://", false),
            ("https://", false),
        ] {
            if lower.starts_with(prefix) {
                let addr = s[prefix.len()..].to_string();
                return Some(if socks {
                    Proxy::Socks5(addr)
                } else {
                    Proxy::Http(addr)
                });
            }
        }
        Some(Proxy::Socks5(s.to_string()))
    }

    pub fn reqwest_url(&self) -> String {
        match self {
            Proxy::Socks5(a) => format!("socks5h://{a}"),
            Proxy::Http(a) => format!("http://{a}"),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Proxy::Socks5(a) => format!("SOCKS5-прокси {a}"),
            Proxy::Http(a) => format!("HTTP-прокси {a}"),
        }
    }
}

/// Что ввёл пользователь: пусто — системный прокси (если включён), «нет» — напрямую.
pub fn resolve(input: &str) -> Option<Proxy> {
    match input.trim().to_lowercase().as_str() {
        "" | "auto" | "авто" => system_proxy(),
        "direct" | "none" | "нет" | "-" => None,
        other => Proxy::parse(other),
    }
}

/// Системный прокси Windows (его включают v2rayN, Hiddify, Nekoray, Clash и др.).
#[cfg(windows)]
pub fn system_proxy() -> Option<Proxy> {
    use std::os::windows::process::CommandExt;
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let query = |name: &str| -> Option<String> {
        let out = std::process::Command::new("reg")
            .args(["query", KEY, "/v", name])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        text.lines()
            .find(|l| l.trim_start().starts_with(name))
            .and_then(|l| l.split_whitespace().last().map(str::to_string))
    };
    if query("ProxyEnable")?.trim() != "0x1" {
        return None;
    }
    parse_windows_proxy(&query("ProxyServer")?)
}

#[cfg(not(windows))]
pub fn system_proxy() -> Option<Proxy> {
    ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy"]
        .iter()
        .find_map(|v| std::env::var(v).ok().and_then(|s| Proxy::parse(&s)))
}

/// `127.0.0.1:10809` или `http=127.0.0.1:10809;https=...;socks=127.0.0.1:10808`
#[allow(dead_code)]
fn parse_windows_proxy(s: &str) -> Option<Proxy> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if !s.contains('=') {
        return Some(Proxy::Http(s.to_string()));
    }
    let (mut https, mut http, mut socks) = (None, None, None);
    for part in s.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "https" => https = Some(v),
                "http" => http = Some(v),
                "socks" => socks = Some(v),
                _ => {}
            }
        }
    }
    https.or(http).map(Proxy::Http).or(socks.map(Proxy::Socks5))
}

/// TCP-соединение с host:port — напрямую или через прокси. Таймаут 15 секунд.
pub async fn connect(proxy: Option<&Proxy>, host: &str, port: u16) -> Result<TcpStream, BoxErr> {
    let attempt = async {
        match proxy {
            None => Ok::<TcpStream, BoxErr>(TcpStream::connect((host, port)).await?),
            Some(Proxy::Socks5(addr)) => {
                let s = tokio_socks::tcp::Socks5Stream::connect(addr.as_str(), (host, port))
                    .await
                    .map_err(|e| format!("SOCKS5-прокси {addr}: {e}"))?;
                Ok(s.into_inner())
            }
            Some(Proxy::Http(addr)) => {
                let mut s = TcpStream::connect(addr.as_str())
                    .await
                    .map_err(|e| format!("HTTP-прокси {addr} недоступен: {e}"))?;
                let req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
                s.write_all(req.as_bytes()).await?;
                let mut buf: Vec<u8> = Vec::with_capacity(256);
                let mut byte = [0u8; 1];
                while !buf.ends_with(b"\r\n\r\n") {
                    if buf.len() > 8192 {
                        return Err("слишком длинный ответ прокси".into());
                    }
                    if s.read(&mut byte).await? == 0 {
                        return Err(format!("HTTP-прокси {addr} закрыл соединение").into());
                    }
                    buf.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&buf);
                let status_line = head.lines().next().unwrap_or("").to_string();
                if status_line.split_whitespace().nth(1) != Some("200") {
                    return Err(format!("HTTP-прокси {addr} ответил: {status_line}").into());
                }
                Ok(s)
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(15), attempt)
        .await
        .map_err(|_| -> BoxErr {
            "превышено время ожидания подключения (15 с)".into()
        })?
}
