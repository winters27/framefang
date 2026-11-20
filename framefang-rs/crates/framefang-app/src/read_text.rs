//! TEXT and QR modes: reading what a picked region says, off the event loop.
//!
//! TEXT reads words with the system's OCR (`framefang_platform::ocr`). QR
//! reads QR codes with `rqrr`; a code drawn light on dark (dark-mode pages)
//! is read inverted, and a small one is read again twice as large.

use image::{GrayImage, RgbaImage};

/// What to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind { Text, Qr }

/// What was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadAnswer {
    /// The text, one line per line.
    Text(String),
    /// Each QR code's contents.
    Qr(Vec<String>),
    /// Nothing to read there.
    Nothing,
    /// The system couldn't read: the message says why, and how to fix it.
    Failed(String),
}

/// What one QR code holds, as far as what to do with it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrContent {
    /// A web page: `http` or `https` only, the one kind that may be opened.
    Link { url: String, host: String },
    /// A Wi-Fi network's name and password (the password is what is copied).
    Wifi { ssid: String, password: String },
    /// An email address, from a `mailto:` code.
    Email(String),
    /// A phone number, from a `tel:` code.
    Phone(String),
    /// Anything else, as it is.
    Text(String),
}

impl QrContent {
    /// Sort what a QR code says.
    pub fn of(raw: &str) -> QrContent {
        let t = raw.trim();
        let lower = t.to_ascii_lowercase();
        if let Some(host) = web_host(t) {
            return QrContent::Link { url: t.to_owned(), host };
        }
        if lower.starts_with("wifi:") {
            let fields = wifi_fields(&t[5..]);
            let get = |k: &str| fields.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
            if let Some(ssid) = get("S") {
                return QrContent::Wifi { ssid, password: get("P").unwrap_or_default() };
            }
        }
        if lower.starts_with("mailto:") {
            let addr = t[7..].split('?').next().unwrap_or("").trim();
            if !addr.is_empty() { return QrContent::Email(addr.to_owned()); }
        }
        if lower.starts_with("tel:") {
            let num = t[4..].trim();
            if !num.is_empty() { return QrContent::Phone(num.to_owned()); }
        }
        QrContent::Text(raw.to_owned())
    }

    /// What goes on the clipboard for it.
    pub fn clipboard_text(&self) -> String {
        match self {
            QrContent::Link { url, .. } => url.clone(),
            // An open network has no password: its name is what is useful.
            QrContent::Wifi { ssid, password } if password.is_empty() => ssid.clone(),
            QrContent::Wifi { password, .. } => password.clone(),
            QrContent::Email(a) | QrContent::Phone(a) | QrContent::Text(a) => a.clone(),
        }
    }

    /// The pill once it is copied.
    pub fn copied_notice(&self) -> String {
        match self {
            QrContent::Link { host, .. } => format!("Copied link to {}", for_pill(host)),
            QrContent::Wifi { ssid, password } if password.is_empty() =>
                format!("Copied Wi-Fi name {}", for_pill(ssid)),
            QrContent::Wifi { ssid, .. } => format!("Copied Wi-Fi password for {}", for_pill(ssid)),
            QrContent::Email(_) => "Copied email address".to_owned(),
            QrContent::Phone(_) => "Copied phone number".to_owned(),
            QrContent::Text(_) => "Copied QR code text".to_owned(),
        }
    }
}

/// The host of an `http` or `https` link with nothing odd in it (no spaces
/// or control characters), lowercased, without a login or port.
fn web_host(t: &str) -> Option<String> {
    let lower = t.to_ascii_lowercase();
    let rest = ["https://", "http://"].iter().find_map(|p| lower.strip_prefix(p))?;
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) { return None; }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// A Wi-Fi code's `K:value;` fields, with `\;` `\,` `\:` `\\` `\"` unescaped.
fn wifi_fields(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut chars = body.chars();
    let mut push = |f: &mut String| {
        if let Some((k, v)) = f.split_once(':') {
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
            out.push((k.to_owned(), v.to_owned()));
        }
        f.clear();
    };
    while let Some(c) = chars.next() {
        match c {
            // An escaped character: keep it, marked so `:` inside a value
            // does not split the key.
            '\\' => if let Some(n) = chars.next() { field.push(if n == ':' { '\u{1}' } else { n }); },
            ';' => push(&mut field),
            _ => field.push(c),
        }
    }
    push(&mut field);
    out.into_iter().map(|(k, v)| (k, v.replace('\u{1}', ":"))).collect()
}

/// A name for the pill: its font has ASCII only, and the pill one line.
fn for_pill(s: &str) -> String {
    const MAX: usize = 28;
    let ascii: String = s.chars().map(|c| if (' '..='~').contains(&c) { c } else { '?' }).collect();
    if ascii.chars().count() <= MAX { return ascii; }
    let cut: String = ascii.chars().take(MAX - 3).collect();
    format!("{cut}...")
}

impl ReadAnswer {
    /// What goes on the clipboard, if anything.
    pub fn clipboard_text(&self) -> Option<String> {
        match self {
            ReadAnswer::Text(t) => Some(t.clone()),
            ReadAnswer::Qr(codes) if codes.len() == 1 => Some(QrContent::of(&codes[0]).clipboard_text()),
            ReadAnswer::Qr(codes) => Some(codes.join("\n")),
            _ => None,
        }
    }

    /// The link to open instead of copying, when links open
    /// (`open_links`): one code, a web link.
    pub fn link_to_open(&self, open_links: bool) -> Option<(String, String)> {
        let ReadAnswer::Qr(codes) = self else { return None };
        if !open_links || codes.len() != 1 { return None; }
        match QrContent::of(&codes[0]) {
            QrContent::Link { url, host } => Some((url, host)),
            _ => None,
        }
    }

    /// The pill shown after a copy: what was copied, in a few words.
    pub fn notice(&self, kind: ReadKind) -> String {
        match (self, kind) {
            (ReadAnswer::Text(t), _) => match t.lines().count() {
                1 => "Copied 1 line of text".to_owned(),
                n => format!("Copied {n} lines of text"),
            },
            (ReadAnswer::Qr(c), _) if c.len() > 1 => format!("Copied {} QR codes", c.len()),
            (ReadAnswer::Qr(c), _) => QrContent::of(&c[0]).copied_notice(),
            (ReadAnswer::Nothing, ReadKind::Text) => "No text found there".to_owned(),
            (ReadAnswer::Nothing, ReadKind::Qr) => "No QR code found there".to_owned(),
            (ReadAnswer::Failed(_), _) => "Couldn't read it".to_owned(),
        }
    }
}

/// The pill once a QR link has been opened.
pub fn opened_notice(host: &str) -> String {
    format!("Opened {}", for_pill(host))
}

/// Read `img`. Blocking.
pub fn read(img: &RgbaImage, kind: ReadKind) -> ReadAnswer {
    match kind {
        ReadKind::Qr => {
            let codes = qr_codes(img);
            if codes.is_empty() { ReadAnswer::Nothing } else { ReadAnswer::Qr(codes) }
        }
        ReadKind::Text => match framefang_platform::ocr::read_text(img) {
            Ok(t) if t.trim().is_empty() => ReadAnswer::Nothing,
            Ok(t) => ReadAnswer::Text(t),
            Err(e) => ReadAnswer::Failed(e.to_string()),
        },
    }
}

/// Every QR code in `img`: as it is, then light-on-dark, then (for a small
/// picture) at twice the size.
pub fn qr_codes(img: &RgbaImage) -> Vec<String> {
    let luma: GrayImage = image::DynamicImage::ImageRgba8(img.clone()).to_luma8();
    let mut tries = vec![luma.clone(), inverted(&luma)];
    if luma.width().min(luma.height()) < 400 {
        let big = image::imageops::resize(&luma, luma.width() * 2, luma.height() * 2, image::imageops::FilterType::Nearest);
        tries.push(inverted(&big));
        tries.push(big);
    }
    for t in tries {
        let mut prepared = rqrr::PreparedImage::prepare(t);
        let found: Vec<String> = prepared.detect_grids().into_iter()
            .filter_map(|g| g.decode().ok().map(|(_, s)| s)).collect();
        if !found.is_empty() { return found; }
    }
    Vec::new()
}

fn inverted(g: &GrayImage) -> GrayImage {
    let mut out = g.clone();
    for p in out.pixels_mut() { p.0[0] = 255 - p.0[0]; }
    out
}

/// A read running on its own thread.
pub struct ReadJob {
    /// Which request this is: only the newest one's answer is used.
    pub gen: u64,
    pub kind: ReadKind,
    pub rx: std::sync::mpsc::Receiver<ReadAnswer>,
}

/// Start reading `img` on its own thread.
pub fn start(img: RgbaImage, kind: ReadKind, gen: u64) -> ReadJob {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(read(&img, kind)); });
    ReadJob { gen, kind, rx }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str, module: u32) -> RgbaImage {
        let q = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let g: GrayImage = q.render::<image::Luma<u8>>().module_dimensions(module, module).build();
        image::DynamicImage::ImageLuma8(g).to_rgba8()
    }

    #[test]
    fn reads_plain_light_on_dark_and_tiny_qr_codes() {
        let url = "https://example.com/frame-fang?x=1";
        assert_eq!(qr_codes(&code(url, 4)), vec![url.to_owned()]);
        // Dark mode: light modules on a dark page.
        let mut dark = code(url, 4);
        for p in dark.pixels_mut() { for c in 0..3 { p.0[c] = 255 - p.0[c]; } }
        assert_eq!(qr_codes(&dark), vec![url.to_owned()], "the inverted retry finds it");
        // One pixel a module.
        assert_eq!(qr_codes(&code(url, 1)), vec![url.to_owned()]);
        // Nothing there.
        let blank = RgbaImage::from_pixel(200, 200, image::Rgba([240, 240, 240, 255]));
        assert!(qr_codes(&blank).is_empty());
        assert_eq!(read(&blank, ReadKind::Qr), ReadAnswer::Nothing);
    }

    #[test]
    fn notices_say_what_was_copied() {
        assert_eq!(ReadAnswer::Text("a\nb".into()).notice(ReadKind::Text), "Copied 2 lines of text");
        assert_eq!(ReadAnswer::Text("a".into()).notice(ReadKind::Text), "Copied 1 line of text");
        assert_eq!(ReadAnswer::Qr(vec!["x".into()]).notice(ReadKind::Qr), "Copied QR code text");
        assert_eq!(ReadAnswer::Nothing.notice(ReadKind::Qr), "No QR code found there");
        assert_eq!(ReadAnswer::Nothing.clipboard_text(), None);
        assert_eq!(ReadAnswer::Qr(vec!["a".into(), "b".into()]).clipboard_text().as_deref(), Some("a\nb"));
    }

    #[test]
    fn a_qr_code_says_what_it_held() {
        let one = |s: &str| ReadAnswer::Qr(vec![s.to_owned()]);
        let link = one("https://user@GitHub.com:443/winters27/framefang?x=1");
        assert_eq!(link.notice(ReadKind::Qr), "Copied link to github.com");
        assert_eq!(link.clipboard_text().as_deref(), Some("https://user@GitHub.com:443/winters27/framefang?x=1"));
        let wifi = one(r#"WIFI:T:WPA;S:Home\;Net;P:"pa\:ss";;"#);
        assert_eq!(wifi.notice(ReadKind::Qr), "Copied Wi-Fi password for Home;Net");
        assert_eq!(wifi.clipboard_text().as_deref(), Some("pa:ss"));
        let open_wifi = one("WIFI:S:Cafe;T:nopass;;");
        assert_eq!(open_wifi.notice(ReadKind::Qr), "Copied Wi-Fi name Cafe");
        assert_eq!(open_wifi.clipboard_text().as_deref(), Some("Cafe"));
        let mail = one("mailto:hi@example.com?subject=Yo");
        assert_eq!((mail.notice(ReadKind::Qr).as_str(), mail.clipboard_text().as_deref()),
                   ("Copied email address", Some("hi@example.com")));
        let tel = one("TEL:+1 555 0100");
        assert_eq!((tel.notice(ReadKind::Qr).as_str(), tel.clipboard_text().as_deref()),
                   ("Copied phone number", Some("+1 555 0100")));
        // A long or non-ASCII name stays one line in the pill's font.
        let long = one("https://a-really-very-long-subdomain.example-domain.com/");
        assert_eq!(long.notice(ReadKind::Qr), "Copied link to a-really-very-long-subdom...");
    }

    #[test]
    fn only_a_lone_web_link_is_ever_opened() {
        let one = |s: &str| ReadAnswer::Qr(vec![s.to_owned()]);
        let link = one("https://example.com/a");
        assert_eq!(link.link_to_open(true), Some(("https://example.com/a".to_owned(), "example.com".to_owned())));
        assert_eq!(link.link_to_open(false), None, "the setting says copy");
        // Nothing that isn't a plain web page: other schemes, odd characters.
        for s in ["javascript:alert(1)", "file:///C:/Windows/notepad.exe", "ms-settings:display",
                  "https://exa mple.com", "https://", "ftp://example.com", "mailto:a@b.c"] {
            assert_eq!(one(s).link_to_open(true), None, "{s}");
        }
        // Two codes: copied together, not opened.
        assert_eq!(ReadAnswer::Qr(vec!["https://a.com".into(), "https://b.com".into()]).link_to_open(true), None);
        assert_eq!(ReadAnswer::Text("https://a.com".into()).link_to_open(true), None, "TEXT only copies");
        assert_eq!(opened_notice("example.com"), "Opened example.com");
    }
}
