//! The Updates dialog: what is installed, what GitHub's newest release is,
//! that release's notes, and a way to get it.
//!
//! The release is looked up on a worker thread as soon as the window opens
//! (with `curl`, which every desktop OS we ship for has: macOS 10.15+,
//! Windows 10 1803+, any current Linux). Until it answers the LATEST line
//! says it is fetching; if it fails the reason is shown, and the button to
//! the releases page always works regardless.
//!
//! How a newer release is offered depends on how this copy was installed
//! (`framefang_core::install_channel`):
//!
//! - an install the app owns, with an asset it knows how to swap in (the
//!   MSI, the Windows portable exe, a Linux tarball, an AppImage): Update
//!   now downloads, checks and installs in place, and the dialog follows
//!   along until the app restarts itself;
//! - an install it owns but can't swap (a Mac app bundle, say): Download
//!   opens the asset in the browser;
//! - an install a package manager owns: no download at all, but the exact
//!   upgrade command, which a button copies.

use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Instant;

use anyhow::Result;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::NamedKey;
use winit::window::WindowId;

use framefang_core::install_channel::{InstallChannel, UpdateAction};

use crate::dialog::{fit_end, inside, Input, Page, Rect, Shell};
use crate::bitmap_font::{self, GLYPH_H};
use crate::palette::*;

const CARD_W: u32 = 560;
const CARD_H: u32 = 374;
const MARGIN: i32 = 22;
/// The INSTALLED line, the body's first; the title strip has the heading.
const TOP_Y: i32 = 22;
/// Where the values of the INSTALLED / LATEST / RELEASED lines start, and
/// how far apart those lines are.
const VALUE_X: i32 = MARGIN + 120;
const FACT_STEP: i32 = 22;
/// One line of release notes.
const LINE_H: i32 = GLYPH_H + 5;
/// The release notes pane, under the version lines and above the buttons.
const NOTES: Rect = (MARGIN, TOP_Y + 100, CARD_W as i32 - 2 * MARGIN, CARD_H as i32 - (TOP_Y + 100) - 70);
const SCROLL_W: i32 = 6;
/// How wide a notes line may run: the pane, less its scrollbar and insets.
const NOTES_TEXT_W: i32 = NOTES.2 - SCROLL_W - 8;
const BTN_H: i32 = 36;
const BTN_GAP: i32 = 12;

/// Where the newest release is described.
const LATEST_RELEASE: &str = "https://api.github.com/repos/winters27/framefang/releases/latest";
const USER_AGENT: &str = "framefang-updater";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action { Releases, Download, CopyCommand }

/// The newest release, ready to show.
struct Release {
    tag: String,
    /// `YYYY-MM-DD`, or empty when GitHub gave none.
    date: String,
    /// Newer than the running build.
    newer: bool,
    /// The notes, as plain text already wrapped to the pane.
    notes: Vec<String>,
    /// The file for this machine and install, if the release has one.
    asset_url: Option<String>,
    /// That file's SHA-256 from the release's `SHA256SUMS`. `None` when
    /// the release has no sums file (older tags) or doesn't list it: the
    /// installer then warns and goes on, and the MSI's own signature is
    /// checked after that.
    sha256: Option<String>,
    /// That file's size in bytes, for the progress bar.
    asset_size: Option<u64>,
}

/// Set once Update now is pressed: the download being watched, and what
/// stopped it if it failed.
struct Updating {
    url: String,
    error: Option<String>,
}

enum Fetch {
    Waiting,
    Got(Release),
    Failed(String),
}

pub enum UpdatesOutcome {
    Closed,
    OpenReleases,
    /// Open this asset in the browser (an install the app can't replace in
    /// place).
    OpenAsset(String),
    /// Download, check and install this asset in place, then restart. The
    /// tray loop hands it to `self_updater::download_and_install`. No hash
    /// means the release shipped no `SHA256SUMS` listing it.
    DownloadAndInstall {
        asset_url:       String,
        expected_sha256: Option<String>,
    },
    /// A package manager owns this install: put this upgrade command on the
    /// clipboard.
    CopyCommand(String),
}

pub struct UpdatesView {
    shell: Shell,
    opened: Instant,
    fetch: Fetch,
    answer: Option<mpsc::Receiver<Result<GithubRelease, String>>>,
    /// How far the notes are scrolled, in pixels.
    scroll: i32,
    /// How this copy was installed and so what an update may do to it.
    /// Worked out once: nothing it depends on changes while the app runs.
    install: InstallChannel,
    action: UpdateAction,
    updating: Option<Updating>,
    hover: Option<Action>,
    pub outcome: Option<UpdatesOutcome>,
}

impl UpdatesView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let shell = Shell::open(loop_target, "Frame Fang: Updates", "// UPDATES", "updates", (CARD_W, CARD_H))?;
        // Ask straight away, so the answer is usually in by the time the
        // window has been read.
        let (send, answer) = mpsc::channel();
        std::thread::spawn(move || { let _ = send.send(ask_github()); });
        let (install, action) = crate::install_source::detected().clone();
        let mut view = UpdatesView {
            shell, install, action,
            opened: Instant::now(),
            fetch: Fetch::Waiting,
            answer: Some(answer),
            scroll: 0, updating: None, hover: None, outcome: None,
        };
        view.paint();
        Ok(view)
    }

    pub fn window_id(&self) -> WindowId { self.shell.id() }

    /// Called on the tray loop's poll tick: takes the release when it
    /// arrives, and keeps the waiting dots and the progress bar moving.
    pub fn tick(&mut self) {
        if let Some(Ok(reply)) = self.answer.as_ref().map(mpsc::Receiver::try_recv) {
            self.fetch = match reply {
                Ok(raw) => Fetch::Got(Release::from_raw(raw, self.install)),
                Err(why) => Fetch::Failed(why),
            };
            self.answer = None;
            self.shell.repaint();
        }
        let downloading = self.updating.as_ref().is_some_and(|u| u.error.is_none());
        if matches!(self.fetch, Fetch::Waiting) || downloading {
            self.shell.repaint();
        }
    }

    /// The update stopped (download, checksum or installer): show why, and
    /// offer it again.
    pub fn update_failed(&mut self, msg: String) {
        if let Some(u) = &mut self.updating { u.error = Some(msg); }
        self.shell.repaint();
    }

    /// Whether Update now can install `url` here, rather than leaving it to
    /// the browser: this install is ours to replace, and we know how to
    /// replace it with that kind of file.
    fn installs_in_place(&self, url: &str) -> bool {
        matches!(self.action, UpdateAction::SelfInstall) && asset_supports_self_install(url, self.install)
    }

    fn release(&self) -> Option<&Release> {
        match &self.fetch {
            Fetch::Got(r) => Some(r),
            _ => None,
        }
    }

    fn shown_buttons(&self) -> Vec<(Action, &'static str, Rect)> {
        let release = self.release();
        let in_place = release.and_then(|r| r.asset_url.as_deref()).is_some_and(|u| self.installs_in_place(u));
        buttons(release, self.updating.as_ref(), &self.action, in_place)
    }

    fn action_at(&self, p: (i32, i32)) -> Option<Action> {
        self.shown_buttons().into_iter().find(|&(_, _, r)| inside(r, p)).map(|(a, _, _)| a)
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match self.shell.input(event) {
            Input::Close | Input::Key(NamedKey::Escape | NamedKey::Enter) => self.outcome = Some(UpdatesOutcome::Closed),
            Input::Key(NamedKey::PageDown) => self.scroll_by(LINE_H * 6),
            Input::Key(NamedKey::PageUp) => self.scroll_by(-LINE_H * 6),
            Input::Key(NamedKey::ArrowDown) => self.scroll_by(LINE_H),
            Input::Key(NamedKey::ArrowUp) => self.scroll_by(-LINE_H),
            Input::Key(NamedKey::Home) => self.scroll_to(0),
            Input::Key(NamedKey::End) => self.scroll_to(self.scroll_limit()),
            Input::Wheel(delta) => {
                let up = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => (lines * LINE_H as f32 * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => p.y as i32,
                };
                // Turning the wheel towards you shows later lines, as
                // everywhere else on the desktop.
                self.scroll_by(-up);
            }
            Input::Moved => {
                let now = self.action_at(self.shell.cursor);
                self.shell.track_hover(&mut self.hover, now, now.is_some());
            }
            Input::Press => {
                if let Some(action) = self.action_at(self.shell.cursor) {
                    self.outcome = Some(self.press(action));
                }
            }
            Input::Paint => self.paint(),
            _ => {}
        }
    }

    fn press(&mut self, action: Action) -> UpdatesOutcome {
        match action {
            Action::Releases => UpdatesOutcome::OpenReleases,
            Action::CopyCommand => match &self.action {
                UpdateAction::Managed { command: Some(cmd), .. } => UpdatesOutcome::CopyCommand(cmd.clone()),
                _ => UpdatesOutcome::OpenReleases,
            },
            Action::Download => {
                let picked = self.release().and_then(|r| Some((r.asset_url.clone()?, r.sha256.clone())));
                let Some((url, expected_sha256)) = picked else { return UpdatesOutcome::OpenReleases };
                if !self.installs_in_place(&url) {
                    return UpdatesOutcome::OpenAsset(url);
                }
                // The bar measures the download file as it grows, so clear
                // out what an earlier attempt left behind.
                let _ = std::fs::remove_file(crate::self_updater::temp_path_for_url(&url));
                // The dialog stays up through the download and the install;
                // the app restarts on the new version by itself.
                self.updating = Some(Updating { url: url.clone(), error: None });
                self.shell.repaint();
                UpdatesOutcome::DownloadAndInstall { asset_url: url, expected_sha256 }
            }
        }
    }

    fn scroll_to(&mut self, y: i32) {
        self.scroll = y.clamp(0, self.scroll_limit());
        self.shell.repaint();
    }

    fn scroll_by(&mut self, dy: i32) { self.scroll_to(self.scroll + dy); }

    fn scroll_limit(&self) -> i32 {
        self.release().map_or(0, |r| notes_overflow(r.notes.len()))
    }

    fn paint(&mut self) {
        let Some(mut page) = self.shell.page() else { return };
        let mut y = TOP_Y;
        page.text(MARGIN, y, "INSTALLED", SECTION_TINT());
        page.text(VALUE_X, y, &format!("v{}", env!("CARGO_PKG_VERSION")), TEXT_BRIGHT);
        y += FACT_STEP;
        page.text(MARGIN, y, "LATEST", SECTION_TINT());

        match &self.fetch {
            Fetch::Waiting => {
                let dots = ".".repeat((self.opened.elapsed().as_millis() / 400 % 4) as usize);
                page.text(VALUE_X, y, &format!("fetching{dots}"), TEXT_MUTED());
            }
            Fetch::Failed(why) => {
                page.text(VALUE_X, y, "couldn't check", DANGER());
                page.text(MARGIN, y + 28, &format!("Couldn't check GitHub: {why}"), TEXT_MUTED());
            }
            Fetch::Got(release) => {
                page.text(VALUE_X, y, &release.tag, TEXT_BRIGHT);
                y += FACT_STEP;
                if !release.date.is_empty() {
                    page.text(MARGIN, y, "RELEASED", SECTION_TINT());
                    page.text(VALUE_X, y, &release.date, TEXT_BRIGHT);
                    y += FACT_STEP;
                }
                match &self.updating {
                    Some(Updating { error: Some(why), .. }) => {
                        page.text(MARGIN, y, "The update didn't finish.", DANGER());
                        page.text(MARGIN, y + 18, &fit_end(why, CARD_W as i32 - 2 * MARGIN), TEXT_MUTED());
                    }
                    Some(Updating { url, error: None }) => {
                        let got = std::fs::metadata(crate::self_updater::temp_path_for_url(url)).map_or(0, |m| m.len());
                        let (line, done) = update_progress(got, release.asset_size, &release.tag);
                        page.text(MARGIN, y, &line, LASER());
                        draw_progress_bar(&mut page, (MARGIN, y + 18, CARD_W as i32 - 2 * MARGIN, 8), done, self.opened);
                    }
                    None if release.newer => page.text(MARGIN, y, "There is a newer version.", LASER()),
                    None => page.text(MARGIN, y, "This is the newest version.", TEXT_MUTED()),
                }
                // A package manager's install: say which, and give the
                // exact command where a download would be explained.
                if let (true, UpdateAction::Managed { hint, command }) = (release.newer, &self.action) {
                    y += 20;
                    page.text(MARGIN, y, hint, TEXT_MUTED());
                    if let Some(cmd) = command {
                        page.text(MARGIN, y + 16, cmd, SECTION_TINT());
                    }
                }
                draw_notes(&mut page, &release.notes, self.scroll);
            }
        }

        for (action, caption, r) in self.shown_buttons() {
            let primary = matches!(action, Action::Download | Action::CopyCommand);
            page.button(r, caption, primary, self.hover == Some(action));
        }
        self.shell.show(page);
    }
}

/// The buttons along the bottom for this state of the dialog. There is no
/// Close: the title strip has one.
///
/// While an update is downloading there is nothing to press. Otherwise
/// the releases page is always there, and a newer release adds the main
/// action beside it: Update now (or Try again after a failure) when it
/// installs in place, Download when the browser fetches it, or the
/// command button for a package manager's install. A managed install with
/// no single command gets no main action; neither does the version that is
/// already running, so nobody is invited to reinstall it.
fn buttons(
    release: Option<&Release>,
    updating: Option<&Updating>,
    action: &UpdateAction,
    in_place: bool,
) -> Vec<(Action, &'static str, Rect)> {
    let failed = match updating {
        Some(Updating { error: None, .. }) => return Vec::new(),
        Some(_) => true,
        None => false,
    };
    let newer = release.is_some_and(|r| r.newer);
    let has_asset = release.is_some_and(|r| r.asset_url.is_some());
    let main = match action {
        UpdateAction::Managed { command: Some(_), .. } if newer => Some((Action::CopyCommand, "Copy the update command")),
        UpdateAction::Managed { .. } => None,
        _ if newer && has_asset => {
            Some((Action::Download, if failed { "Try again" } else if in_place { "Update now" } else { "Download" }))
        }
        _ => None,
    };
    let y = CARD_H as i32 - MARGIN - BTN_H;
    let releases = "See all releases";
    match main {
        Some((kind, caption)) => {
            let (main_w, side_w) = (200, 180);
            let x = (CARD_W as i32 - (main_w + BTN_GAP + side_w)) / 2;
            vec![
                (kind, caption, (x, y, main_w, BTN_H)),
                (Action::Releases, releases, (x + main_w + BTN_GAP, y, side_w, BTN_H)),
            ]
        }
        None => {
            let w = 220;
            vec![(Action::Releases, releases, ((CARD_W as i32 - w) / 2, y, w, BTN_H))]
        }
    }
}

/// The bar under the progress line, `done` full; a block sweeping across
/// when the size isn't known.
fn draw_progress_bar(page: &mut Page, bar: Rect, done: Option<f32>, clock: Instant) {
    page.fill(bar, PANEL_FILL());
    page.outline(bar, PANEL_BORDER());
    let room = bar.2 - 2;
    let (x, w) = match done {
        Some(f) => (bar.0 + 1, (room as f32 * f) as i32),
        None => {
            let w = room / 4;
            let t = (clock.elapsed().as_millis() % 1400) as f32 / 1400.0;
            (bar.0 + 1 + ((room - w) as f32 * t) as i32, w)
        }
    };
    page.fill((x, bar.1 + 1, w.max(0), bar.3 - 2), LASER());
}

/// How far notes `count` lines long can scroll in the pane.
fn notes_overflow(count: usize) -> i32 {
    (count as i32 * LINE_H - NOTES.3 + 8).max(0)
}

/// The release notes pane: the visible lines (`# ` ones as headings in the
/// section tint) and a scrollbar along the right.
fn draw_notes(page: &mut Page, lines: &[String], scroll: i32) {
    let (x, y, w, h) = NOTES;
    page.fill(NOTES, PANEL_FILL());
    page.outline(NOTES, PANEL_BORDER());

    let (text_x, text_top, text_h) = (x + 8, y + 6, h - 12);
    let first = (scroll / LINE_H).max(0);
    let end = ((scroll + text_h) / LINE_H + 1).min(lines.len() as i32);
    for i in first..end {
        let line_y = text_top + i * LINE_H - scroll;
        if line_y + GLYPH_H < text_top || line_y > text_top + text_h { continue; }
        let line = lines[i as usize].as_str();
        match line.strip_prefix("# ") {
            Some(heading) => page.text(text_x, line_y, heading, SECTION_TINT()),
            None if !line.is_empty() => page.text(text_x, line_y, line, TEXT_BRIGHT),
            None => {}
        }
    }

    let track = (x + w - SCROLL_W - 2, y + 4, SCROLL_W, h - 8);
    page.fill(track, SCROLLBAR_BG());
    let overflow = notes_overflow(lines.len());
    if overflow > 0 {
        let all_lines_h = (lines.len() as i32 * LINE_H).max(1);
        let thumb_h = ((track.3 as f32 * (text_h as f32 / all_lines_h as f32)).max(18.0) as i32).min(track.3);
        let thumb_y = track.1 + ((track.3 - thumb_h) as f32 * (scroll as f32 / overflow as f32)) as i32;
        page.fill((track.0, thumb_y, SCROLL_W, thumb_h), SCROLLBAR_FG());
    }
}

// ── The release, from GitHub ────────────────────────────────────────────────

/// One file attached to a release, under GitHub's own field names.
struct RawAsset { name: String, browser_download_url: String, size: u64 }

/// A release as GitHub describes it, trimmed to what the dialog uses.
struct GithubRelease {
    tag_name: String,
    /// The notes, in markdown.
    body: String,
    /// When it went out, ISO-8601.
    published: String,
    assets: Vec<RawAsset>,
    /// Where the release's `SHA256SUMS` asset is, if it has one.
    sums_url: Option<String>,
    /// That file's text (`sha256sum` output, made by the release build), or
    /// empty when there is none or it couldn't be fetched.
    sha256sums: String,
}

impl Release {
    fn from_raw(raw: GithubRelease, install: InstallChannel) -> Release {
        let asset_url = asset_for_this_machine(&raw.assets, install);
        let sha256 = asset_url.as_deref()
            .and_then(|url| crate::self_updater::parse_sha256sums(&raw.sha256sums, file_name_of(url)));
        let asset_size = asset_url.as_deref()
            .and_then(|url| raw.assets.iter().find(|a| a.browser_download_url == url))
            .map(|a| a.size)
            .filter(|&n| n > 0);
        Release {
            newer: !same_version(&raw.tag_name, env!("CARGO_PKG_VERSION")),
            date: date_of(&raw.published),
            notes: wrap_notes(&plain_text(&raw.body), NOTES_TEXT_W),
            tag: raw.tag_name,
            asset_url,
            sha256,
            asset_size,
        }
    }
}

fn curl(args: &[&str]) -> std::io::Result<Output> {
    crate::self_updater::hide_console(Command::new("curl").args(args)).output()
}

/// Ask GitHub for the newest release, then (best effort) its checksums.
fn ask_github() -> Result<GithubRelease, String> {
    let reply = curl(&[
        "-sS", "-A", USER_AGENT, "--max-time", "8",
        "-H", "Accept: application/vnd.github+json",
        LATEST_RELEASE,
    ])
    .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !reply.status.success() {
        return Err(format!("curl failed ({})", reply.status));
    }
    let mut release = parse_release(&String::from_utf8_lossy(&reply.stdout))?;
    // Missing checksums never stop the dialog: the notes still show, and
    // the installer copes without a hash.
    if let Some(url) = &release.sums_url {
        if let Ok(sums) = curl(&["-fsSL", "-A", USER_AGENT, "--max-time", "8", url]) {
            if sums.status.success() {
                release.sha256sums = String::from_utf8_lossy(&sums.stdout).into_owned();
            }
        }
    }
    Ok(release)
}

/// The fields we use from GitHub's release JSON.
fn parse_release(json: &str) -> Result<GithubRelease, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("the reply wasn't JSON: {e}"))?;
    let text = |obj: &serde_json::Value, key: &str| obj.get(key).and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let tag_name = v.get("tag_name").and_then(|x| x.as_str()).ok_or("the reply had no tag_name")?.to_owned();
    let mut assets = Vec::new();
    let mut sums_url = None;
    for a in v.get("assets").and_then(|x| x.as_array()).into_iter().flatten() {
        let (name, url) = (text(a, "name"), text(a, "browser_download_url"));
        if name.is_empty() || url.is_empty() { continue; }
        if name.eq_ignore_ascii_case("SHA256SUMS") { sums_url = Some(url.clone()); }
        let size = a.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
        assets.push(RawAsset { name, browser_download_url: url, size });
    }
    Ok(GithubRelease {
        tag_name,
        body: text(&v, "body"),
        // Set on drafts too, so it is the date to show.
        published: text(&v, "published_at"),
        assets,
        sums_url,
        sha256sums: String::new(),
    })
}

/// The line over the progress bar while Update now runs, and how full the
/// bar is (`None` when the size is unknown): megabytes so far, then
/// installing once it is all in.
fn update_progress(got: u64, total: Option<u64>, tag: &str) -> (String, Option<f32>) {
    let mb = |bytes: u64| bytes as f64 / 1_048_576.0;
    match total {
        None => (format!("Downloading {tag}..."), None),
        Some(all) if got >= all => (format!("Installing {tag}... Frame Fang restarts by itself."), Some(1.0)),
        Some(all) => (format!("Downloading {tag}  {:.1} / {:.1} MB", mb(got), mb(all)), Some(got as f32 / all as f32)),
    }
}

/// The date part of an ISO-8601 time: `2026-05-17T18:22:11Z` is
/// `2026-05-17`. Empty when there is too little to be a date.
fn date_of(iso: &str) -> String {
    iso.get(..10).unwrap_or("").to_owned()
}

/// Whether a release tag names the running version. Tags come as `v1.3`,
/// `1.3` or `v1.3.0` while the build's own version is always `1.3.0`, so a
/// leading `v` and trailing `.0` parts don't count.
fn same_version(tag: &str, version: &str) -> bool {
    fn parts(s: &str) -> Vec<&str> {
        let mut parts: Vec<&str> = s.trim().trim_start_matches('v').trim_start_matches('V').split('.').collect();
        while parts.len() > 1 && parts.last() == Some(&"0") { parts.pop(); }
        parts
    }
    parts(tag) == parts(version)
}

/// The Windows installer.
const MSI: &str = "Framefang.msi";
/// The Windows portable build, the app itself, published loose since
/// 1.3.1 (1.3.0 and earlier put it in a zip).
const PORTABLE_EXE: &str = "Framefang-portable.exe";

/// Each OS and architecture's release file (named as in
/// `.github/workflows/release.yml`), found anywhere in an asset's name,
/// case aside. Macs get the `.dmg`, which updates through the browser and
/// is the same file a first download gets.
const PLATFORM_ASSETS: [(&str, &str, &str); 5] = [
    ("linux", "x86_64", "framefang-linux-x86_64-portable.tar.gz"),
    ("linux", "aarch64", "framefang-linux-arm64-portable.tar.gz"),
    ("macos", "aarch64", "framefang-macos-arm64.dmg"),
    ("macos", "x86_64", "framefang-macos-x64.dmg"),
    ("windows", "x86_64", "framefang-windows-x86_64.zip"),
];

/// The release file to offer this machine, given how it was installed.
fn asset_for_this_machine(assets: &[RawAsset], install: InstallChannel) -> Option<String> {
    pick_for(assets, install, std::env::consts::OS, std::env::consts::ARCH)
}

fn pick_for(assets: &[RawAsset], install: InstallChannel, os: &str, arch: &str) -> Option<String> {
    let url = |a: &RawAsset| a.browser_download_url.clone();
    // A running AppImage lives in a read-only mount, so only another
    // AppImage (for this architecture) can replace it. Without one, offer
    // nothing rather than a file that can't be installed.
    if install == InstallChannel::AppImage {
        let tokens = arch_tokens(arch);
        return assets.iter().find(|a| {
            let name = a.name.to_ascii_lowercase();
            name.ends_with(".appimage") && tokens.iter().any(|t| name.contains(t))
        }).map(url);
    }
    // On 64-bit Windows, exact names come first: the MSI for an MSI install
    // (msiexec upgrades it in place and keeps Add/Remove Programs right),
    // then the loose portable exe. An MSI install on a release without an
    // MSI still updates through the portable file; only the installed
    // version shown in Windows' list stays old until the next MSI.
    let exact: &[&str] = match (os, arch) {
        ("windows", "x86_64") if install == InstallChannel::WindowsMsi => &[MSI, PORTABLE_EXE],
        ("windows", "x86_64") => &[PORTABLE_EXE],
        _ => &[],
    };
    if let Some(a) = exact.iter().find_map(|want| assets.iter().find(|a| a.name.eq_ignore_ascii_case(want))) {
        return Some(url(a));
    }
    let (_, _, needle) = PLATFORM_ASSETS.iter().find(|(o, a, _)| *o == os && *a == arch)?;
    assets.iter().find(|a| a.name.to_ascii_lowercase().contains(needle)).map(url)
}

/// How asset names spell an architecture, most specific first, so an ARM
/// machine never takes an x86_64 AppImage.
fn arch_tokens(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[],
    }
}

/// The file name at the end of a download URL, without any query.
fn file_name_of(url: &str) -> &str {
    let last = url.rsplit('/').next().unwrap_or(url);
    last.split('?').next().unwrap_or(last)
}

/// Whether we know how to install a file shaped like `url` over an install
/// of `install`. The install kind says whether it may be replaced at all;
/// this says whether we can do it with this file. When not (an MSI install
/// offered only a zip, say), the browser downloads it instead.
fn asset_supports_self_install(url: &str, install: InstallChannel) -> bool {
    let name = file_name_of(url).to_ascii_lowercase();
    let endings: &[&str] = match install {
        InstallChannel::WindowsMsi => &[".msi"],
        InstallChannel::AppImage => &[".appimage"],
        // The portable exe, or the zip older releases put it in: the swap
        // renames the running exe out of the way.
        InstallChannel::WindowsPortable => &[".exe", ".zip"],
        // A loose binary from the tarball or install.sh: the one portable
        // layout whose swap is proven, and only on Linux.
        InstallChannel::Portable if cfg!(target_os = "linux") => &[".tar.gz", ".tgz"],
        _ => &[],
    };
    endings.iter().any(|e| name.ends_with(e))
}

/// Release notes markdown as plain text for the pixel font: headings
/// (`##` and `###`) become `# ` lines the pane tints, `- ` and `* ` bullets
/// become `> ` (the font has only printable ASCII, no bullet), and the
/// inline marks go (see [`plain_inline`]). Line breaks stay.
fn plain_text(md: &str) -> String {
    let mut text = String::with_capacity(md.len());
    for line in md.lines() {
        let line = line.trim_end_matches('\r');
        let line = match line.strip_prefix("### ").or_else(|| line.strip_prefix("## ")) {
            Some(heading) => format!("# {heading}"),
            None => line.to_owned(),
        };
        let body = line.trim_start();
        let line = match body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
            Some(item) => format!("{}> {item}", " ".repeat(line.len() - body.len())),
            None => line,
        };
        text += &plain_inline(&line);
        text += "\n";
    }
    text
}

/// One line of markdown without its inline marks: every `*`, `_` and
/// backquote is dropped, `[text](url)` reads `text (url)`, and the
/// typographic characters GitHub bodies are full of get ASCII stand-ins.
/// Anything else outside ASCII is kept whole, as one character.
fn plain_inline(s: &str) -> String {
    let mut flat = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' | '_' | '`' => {}
            '\u{2014}' | '\u{2013}' => flat.push('-'),
            '\u{2022}' => flat.push('>'),
            '\u{201C}' | '\u{201D}' => flat.push('"'),
            '\u{2018}' | '\u{2019}' => flat.push('\''),
            '\u{2026}' => flat.push_str("..."),
            c => flat.push(c),
        }
    }
    let mut unlinked = String::with_capacity(flat.len());
    let mut rest = flat.as_str();
    while let Some(open) = rest.find('[') {
        unlinked += &rest[..open];
        rest = &rest[open + 1..];
        let link = rest.split_once(']')
            .and_then(|(text, after)| Some((text, after.strip_prefix('(')?.split_once(')')?)));
        match link {
            Some((text, (url, after))) => {
                unlinked += &format!("{text} ({url})");
                rest = after;
            }
            None => unlinked.push('['),
        }
    }
    unlinked + rest
}

/// Wrap `text` into lines at most `max_px` wide in the 1x pixel font. Each
/// line break in `text` ends a line, and blank lines stay as empty ones so
/// paragraphs keep apart. A `# ` heading keeps its marker on every piece,
/// and a word too long for a line gets one to itself.
fn wrap_notes(text: &str, max_px: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut probe = String::new();
    for source in text.lines() {
        if source.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let (marker, words) = match source.strip_prefix("# ") {
            Some(rest) => ("# ", rest),
            None => ("", source),
        };
        let mut line = String::from(marker);
        let mut started = false;
        for word in words.split(' ') {
            if word.is_empty() {
                // A run of spaces keeps one each, once a line has begun.
                if started { line.push(' '); }
                continue;
            }
            if !started {
                line.push_str(word);
                started = true;
                continue;
            }
            probe.clear();
            probe.push_str(&line);
            probe.push(' ');
            probe.push_str(word);
            if bitmap_font::measure(&probe, 1) <= max_px {
                std::mem::swap(&mut line, &mut probe);
            } else {
                lines.push(std::mem::replace(&mut line, format!("{marker}{word}")));
            }
        }
        if started { lines.push(line); }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::install_channel::{update_action, HostOs, InstallProbe};

    fn asset(name: &str) -> RawAsset {
        RawAsset { name: name.to_owned(), browser_download_url: format!("https://dl.example/{name}"), size: 0 }
    }

    /// Every file a full release carries.
    fn full_release() -> Vec<RawAsset> {
        [
            "Framefang.msi", "Framefang-portable.exe",
            "Framefang-macos-arm64.dmg", "Framefang-macos-x64.dmg",
            "Framefang-linux-x86_64.AppImage", "Framefang-linux-aarch64.AppImage",
            "Framefang-linux-x86_64-portable.tar.gz", "Framefang-linux-arm64-portable.tar.gz",
            "SHA256SUMS",
        ].into_iter().map(asset).collect()
    }

    fn picked(assets: &[RawAsset], channel: InstallChannel, os: &str, arch: &str) -> Option<String> {
        pick_for(assets, channel, os, arch).map(|u| file_name_of(&u).to_owned())
    }

    #[test]
    fn progress_counts_megabytes_then_says_it_is_installing() {
        let mb = 1 << 20;
        assert_eq!(update_progress(mb, Some(4 * mb), "v1.3.1"), ("Downloading v1.3.1  1.0 / 4.0 MB".to_owned(), Some(0.25)));
        let (line, done) = update_progress(4 * mb, Some(4 * mb), "v1.3.1");
        assert!(line.starts_with("Installing v1.3.1"), "{line}");
        assert_eq!(done, Some(1.0));
        assert_eq!(update_progress(10, None, "v1.3.1"), ("Downloading v1.3.1...".to_owned(), None));
        assert!(line.is_ascii(), "the pixel font draws ASCII only");
        assert!(bitmap_font::measure(&line, 1) <= CARD_W as i32 - 2 * MARGIN);
    }

    #[test]
    fn every_platform_takes_its_own_file_from_a_full_release() {
        let all = full_release();
        let cases = [
            (InstallChannel::WindowsMsi, "windows", "x86_64", Some("Framefang.msi")),
            (InstallChannel::WindowsPortable, "windows", "x86_64", Some("Framefang-portable.exe")),
            (InstallChannel::Scoop, "windows", "x86_64", Some("Framefang-portable.exe")),
            (InstallChannel::MacAppBundle, "macos", "aarch64", Some("Framefang-macos-arm64.dmg")),
            (InstallChannel::HomebrewCask, "macos", "x86_64", Some("Framefang-macos-x64.dmg")),
            (InstallChannel::Portable, "linux", "x86_64", Some("Framefang-linux-x86_64-portable.tar.gz")),
            (InstallChannel::Portable, "linux", "aarch64", Some("Framefang-linux-arm64-portable.tar.gz")),
            (InstallChannel::AppImage, "linux", "x86_64", Some("Framefang-linux-x86_64.AppImage")),
            (InstallChannel::AppImage, "linux", "aarch64", Some("Framefang-linux-aarch64.AppImage")),
            (InstallChannel::WindowsPortable, "windows", "aarch64", None),
            (InstallChannel::Portable, "freebsd", "x86_64", None),
        ];
        for (channel, os, arch, want) in cases {
            assert_eq!(picked(&all, channel, os, arch).as_deref(), want, "{channel:?} on {os}/{arch}");
        }
        // And on the machine running the tests, through the real entry point.
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            assert_eq!(asset_for_this_machine(&all, InstallChannel::WindowsMsi).as_deref(), Some("https://dl.example/Framefang.msi"));
        }
    }

    #[test]
    fn windows_falls_back_from_the_msi_to_the_portable_exe_then_the_old_zip() {
        let no_msi = vec![asset("Framefang-portable.exe"), asset("framefang-windows-x86_64.zip")];
        assert_eq!(picked(&no_msi, InstallChannel::WindowsMsi, "windows", "x86_64").as_deref(), Some("Framefang-portable.exe"));
        let old = vec![asset("Framefang-windows-x86_64.zip")];
        assert_eq!(picked(&old, InstallChannel::WindowsMsi, "windows", "x86_64").as_deref(), Some("Framefang-windows-x86_64.zip"));
        // Exact names really are exact (case aside).
        let lookalike = vec![asset("FRAMEFANG.MSI"), asset("Framefang.msi.sig")];
        assert_eq!(picked(&lookalike, InstallChannel::WindowsMsi, "windows", "x86_64").as_deref(), Some("FRAMEFANG.MSI"));
    }

    #[test]
    fn an_appimage_is_only_ever_offered_an_appimage_for_its_architecture() {
        let mixed = vec![asset("framefang-linux-x86_64.tar.gz"), asset("framefang-x86_64.AppImage"), asset("SHA256SUMS")];
        assert_eq!(picked(&mixed, InstallChannel::AppImage, "linux", "x86_64").as_deref(), Some("framefang-x86_64.AppImage"));
        assert_eq!(picked(&mixed, InstallChannel::AppImage, "linux", "aarch64"), None);
        let none = vec![asset("framefang-linux-x86_64.tar.gz"), asset("Framefang.msi")];
        assert_eq!(picked(&none, InstallChannel::AppImage, "linux", "x86_64"), None);
        assert_eq!(picked(&mixed, InstallChannel::AppImage, "linux", "riscv64"), None);
    }

    #[test]
    fn installing_in_place_depends_on_the_file_as_well_as_the_install() {
        let msi = "https://dl.example/Framefang.msi";
        let zip = "https://dl.example/framefang-windows-x86_64.zip";
        let exe = "https://dl.example/Framefang-portable.exe";
        let tarball = "https://dl.example/framefang-linux-x86_64.tar.gz";
        assert!(asset_supports_self_install(msi, InstallChannel::WindowsMsi));
        // An MSI install offered only the zip goes to the browser, so the
        // exe isn't swapped out from under the installer's records.
        assert!(!asset_supports_self_install(zip, InstallChannel::WindowsMsi));
        assert!(asset_supports_self_install(zip, InstallChannel::WindowsPortable));
        assert!(asset_supports_self_install(exe, InstallChannel::WindowsPortable));
        assert!(asset_supports_self_install("https://dl.example/framefang-x86_64.AppImage?t=1", InstallChannel::AppImage));
        assert!(!asset_supports_self_install(tarball, InstallChannel::AppImage));
        assert_eq!(asset_supports_self_install(tarball, InstallChannel::Portable), cfg!(target_os = "linux"));
        assert_eq!(asset_supports_self_install("https://dl.example/a.TGZ", InstallChannel::Portable), cfg!(target_os = "linux"));
        for channel in [
            InstallChannel::Snap, InstallChannel::Flatpak, InstallChannel::HomebrewCask,
            InstallChannel::HomebrewFormula, InstallChannel::LinuxSystemPackage, InstallChannel::Scoop,
            InstallChannel::MacAppBundle,
        ] {
            for url in [msi, zip, exe, tarball] {
                assert!(!asset_supports_self_install(url, channel), "{channel:?} must not self-install {url}");
            }
        }
    }

    #[test]
    fn a_file_name_drops_the_path_and_the_query() {
        assert_eq!(file_name_of("https://x/releases/download/v1/Framefang.msi?sig=abc"), "Framefang.msi");
        assert_eq!(file_name_of("Framefang.msi"), "Framefang.msi");
        assert_eq!(file_name_of("https://x/dir/"), "");
    }

    #[test]
    fn versions_match_with_or_without_a_v_and_trailing_zeros() {
        assert!(same_version("v1.3.0", "1.3.0"));
        assert!(same_version("1.3", "1.3.0"));
        assert!(same_version(" V2 ", "2.0.0"));
        assert!(same_version("v0", "0.0.0"));
        assert!(!same_version("v1.3.1", "1.3.0"));
        assert!(!same_version("v1.30", "1.3.0"));
        assert!(!same_version("v1.3.0-beta", "1.3.0"));
    }

    #[test]
    fn the_release_date_is_the_first_ten_characters() {
        assert_eq!(date_of("2026-05-17T18:22:11Z"), "2026-05-17");
        assert_eq!(date_of("2026-05"), "");
        assert_eq!(date_of(""), "");
        // Never splits a character (it used to panic here).
        assert_eq!(date_of("2026-05-1\u{e9}T"), "");
    }

    /// The pixel font has glyphs for printable ASCII only; anything else
    /// draws as `?`, which is how `???` bullets once reached the dialog.
    fn assert_drawable(s: &str) {
        for c in s.chars() {
            assert!(c == '\n' || (' '..='~').contains(&c), "{c:?} (U+{:04X}) would draw as '?'", c as u32);
        }
    }

    #[test]
    fn a_generated_release_body_comes_out_as_plain_ascii() {
        // The layout GitHub generates for a release: `* ` bullets, links,
        // bold, and a dash from a commit title.
        let body = [
            "## What's new",
            "* fix(docs): drop stale icon.svg by @someone in https://github.com/winters27/framefang/pull/37",
            "* fix(windows): close the tray menu \u{2014} overflow flyout first by @someone in https://github.com/winters27/framefang/pull/39",
            "",
            "**Full Changelog**: https://github.com/winters27/framefang/compare/v0.3.6...v0.3.7",
        ].join("\n");
        let out = plain_text(&body);
        assert_drawable(&out);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "# What's new");
        assert!(lines[1].starts_with("> fix(docs)"), "{out:?}");
        assert!(lines[2].contains("menu - overflow"), "{out:?}");
        assert_eq!(lines[3], "");
        assert!(lines[4].starts_with("Full Changelog: "), "{out:?}");
    }

    #[test]
    fn bullets_keep_their_indent_and_headings_their_marker() {
        let out = plain_text("### Fixed\n  - nested `code`\r\n#### deep\n-not a bullet");
        assert_eq!(out, "# Fixed\n  > nested code\n#### deep\n-not a bullet\n");
    }

    #[test]
    fn inline_marks_go_and_other_characters_stay_whole() {
        assert_eq!(plain_inline("caf\u{e9} [docs](https://x.y)"), "caf\u{e9} docs (https://x.y)");
        assert_eq!(plain_inline("\u{201C}quoted\u{201D} \u{2018}it\u{2019}s\u{2019}\u{2026} \u{2022} x"), "\"quoted\" 'it's'... > x");
        assert_eq!(plain_inline("snake_case and **bold**"), "snakecase and bold");
    }

    #[test]
    fn links_read_as_text_then_url() {
        assert_eq!(
            plain_inline("see [the release](https://github.com/a/b/releases) now"),
            "see the release (https://github.com/a/b/releases) now"
        );
        // Brackets that aren't a link are left alone.
        assert_eq!(plain_inline("[x] done, [y](no close"), "[x] done, [y](no close");
        assert_eq!(plain_inline("a [b] (c) [d](e)"), "a [b] (c) d (e)");
    }

    #[test]
    fn notes_wrap_to_the_pane_and_keep_paragraphs_apart() {
        let width = bitmap_font::measure("aaaa bbbb", 1);
        let lines = wrap_notes("aaaa bbbb cccc\n\n# head ing more\nwordthatistoolong x", width);
        assert_eq!(lines, ["aaaa bbbb", "cccc", "", "# head", "# ing", "# more", "wordthatistoolong", "x"]);
        // Double spaces survive inside a line.
        assert_eq!(wrap_notes("a  b", 500), ["a  b"]);
        assert!(wrap_notes("", 100).is_empty());
    }

    #[test]
    fn release_json_gives_the_tag_notes_assets_and_sums_file() {
        let raw = parse_release(r#"{
            "tag_name": "v9.9.9", "body": "**hi**", "published_at": "2026-11-01T10:00:00Z",
            "html_url": "https://github.com/winters27/framefang/releases/tag/v9.9.9",
            "assets": [
                {"name": "Framefang.msi", "browser_download_url": "https://dl.example/Framefang.msi", "size": 4096},
                {"name": "sha256sums", "browser_download_url": "https://dl.example/SHA256SUMS", "size": 300},
                {"name": "", "browser_download_url": "https://dl.example/nameless", "size": 1},
                {"name": "no-url"}
            ]
        }"#).unwrap();
        assert_eq!(raw.tag_name, "v9.9.9");
        assert_eq!(raw.body, "**hi**");
        assert_eq!(raw.published, "2026-11-01T10:00:00Z");
        assert_eq!(raw.assets.len(), 2, "assets without a name or URL are skipped");
        assert_eq!(raw.assets[0].size, 4096);
        assert_eq!(raw.sums_url.as_deref(), Some("https://dl.example/SHA256SUMS"));
        assert!(parse_release("{}").err().is_some_and(|e| e.contains("tag_name")));
        assert!(parse_release("<html>").is_err());
    }

    #[test]
    fn a_release_finds_its_file_hash_and_size() {
        let hash = "ab".repeat(32);
        let mut assets = full_release();
        for a in &mut assets { a.size = 1234; }
        let raw = GithubRelease {
            tag_name: "v0.0.1".into(),
            body: "## Notes\n- one".into(),
            published: "2020-01-02T00:00:00Z".into(),
            assets,
            sums_url: None,
            sha256sums: format!("{hash}  Framefang.msi\n{hash}  Framefang-portable.exe\n{hash}  Framefang-macos-arm64.dmg\n\
                {hash}  Framefang-macos-x64.dmg\n{hash}  Framefang-linux-x86_64-portable.tar.gz\n\
                {hash}  Framefang-linux-arm64-portable.tar.gz\n"),
        };
        let release = Release::from_raw(raw, InstallChannel::Portable);
        assert_eq!(release.date, "2020-01-02");
        assert_eq!(release.notes, ["# Notes", "> one"]);
        assert!(release.newer, "an older tag still counts as different");
        if release.asset_url.is_some() {
            assert_eq!(release.sha256.as_deref(), Some(hash.as_str()));
            assert_eq!(release.asset_size, Some(1234));
        } else {
            assert!(release.sha256.is_none() && release.asset_size.is_none());
        }
    }

    fn release_with(newer: bool, asset: bool) -> Release {
        Release {
            tag: "v9".into(), date: String::new(), newer, notes: Vec::new(),
            asset_url: asset.then(|| "https://dl.example/Framefang.msi".to_owned()),
            sha256: None, asset_size: None,
        }
    }

    fn captions(b: &[(Action, &'static str, Rect)]) -> Vec<&'static str> { b.iter().map(|x| x.1).collect() }

    #[test]
    fn the_main_button_follows_the_install_and_the_release() {
        let own = UpdateAction::SelfInstall;
        let browser = UpdateAction::BrowserDownload;
        let new = release_with(true, true);
        assert_eq!(captions(&buttons(Some(&new), None, &own, true)), ["Update now", "See all releases"]);
        assert_eq!(captions(&buttons(Some(&new), None, &browser, false)), ["Download", "See all releases"]);
        let failed = Updating { url: String::new(), error: Some("checksum mismatch".into()) };
        assert_eq!(captions(&buttons(Some(&new), Some(&failed), &own, true)), ["Try again", "See all releases"]);
        let running = Updating { url: String::new(), error: None };
        assert!(buttons(Some(&new), Some(&running), &own, true).is_empty(), "nothing to press mid-download");
        // Already current, or nothing for this machine: just the releases page.
        assert_eq!(captions(&buttons(Some(&release_with(false, true)), None, &own, true)), ["See all releases"]);
        assert_eq!(captions(&buttons(Some(&release_with(true, false)), None, &own, false)), ["See all releases"]);
        assert_eq!(captions(&buttons(None, None, &own, false)), ["See all releases"]);
        let managed = UpdateAction::Managed { hint: "Installed with Snap.", command: Some("sudo snap refresh framefang".into()) };
        let b = buttons(Some(&new), None, &managed, false);
        assert_eq!(b[0].0, Action::CopyCommand);
        let no_command = UpdateAction::Managed { hint: "Your package manager updates it.", command: None };
        assert_eq!(captions(&buttons(Some(&new), None, &no_command, false)), ["See all releases"]);
        // Both rows sit centered on the bottom margin.
        for row in [buttons(Some(&new), None, &own, true), buttons(None, None, &own, false)] {
            let (first, last) = (row[0].2, row[row.len() - 1].2);
            assert_eq!(first.0, CARD_W as i32 - (last.0 + last.2));
            assert_eq!(first.1 + first.3, CARD_H as i32 - MARGIN);
        }
    }

    /// The info block doesn't wrap, so every hint and command a package
    /// manager can be given has to fit one line, and every caption its
    /// button.
    #[test]
    fn package_manager_hints_commands_and_captions_fit() {
        let line = CARD_W as i32 - 2 * MARGIN;
        let new = release_with(true, true);
        for (action, in_place) in [
            (UpdateAction::SelfInstall, true),
            (UpdateAction::SelfInstall, false),
            (UpdateAction::Managed { hint: "", command: Some(String::new()) }, false),
        ] {
            for (_, caption, r) in buttons(Some(&new), None, &action, in_place) {
                assert!(bitmap_font::measure(caption, 1) <= r.2 - 16, "{caption}");
            }
        }
        assert!(bitmap_font::measure("Try again", 1) <= 200 - 16);

        // No os-release at all, then one of each distro family.
        let distros = ["", "ID=ubuntu\nID_LIKE=debian\n", "ID=fedora\n", "ID=arch\n",
                       "ID=opensuse-leap\nID_LIKE=suse\n", "ID=voidlinux\n"];
        let linux = [InstallChannel::Snap, InstallChannel::Flatpak, InstallChannel::LinuxSystemPackage]
            .map(|c| (c, HostOs::Linux));
        let others = [(InstallChannel::HomebrewCask, HostOs::MacOs), (InstallChannel::HomebrewFormula, HostOs::MacOs),
                      (InstallChannel::Scoop, HostOs::Windows)];
        for (channel, os) in linux.into_iter().chain(others) {
            for distro in distros {
                let os_release = (!distro.is_empty()).then(|| distro.to_owned());
                let offered = update_action(channel, &InstallProbe { os, os_release, ..Default::default() });
                let UpdateAction::Managed { hint, command } = offered else {
                    panic!("{channel:?} should belong to a package manager");
                };
                assert!(bitmap_font::measure(hint, 1) <= line, "{channel:?} hint too wide: {hint:?}");
                if let Some(cmd) = command {
                    assert!(bitmap_font::measure(&cmd, 1) <= line, "{channel:?} command too wide: {cmd:?}");
                    assert_drawable(&cmd);
                }
            }
        }
    }
}
