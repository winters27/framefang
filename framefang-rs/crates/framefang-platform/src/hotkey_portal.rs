//! Global shortcuts on Wayland, through `org.freedesktop.portal.GlobalShortcuts`.
//!
//! Everywhere else a global hotkey is a grab: the app names a key and the
//! window system routes it to us. Wayland has no grab on purpose, since no
//! client may watch input meant for another. Instead the app asks
//! `xdg-desktop-portal` for a shortcut, the compositor decides what key it
//! really gets (and lists it in the desktop's own keyboard settings), and an
//! `Activated` signal arrives whenever the user presses it.
//!
//! That changes three things compared with the other backends:
//!
//! * **We can only ask.** The trigger we send is a preference. The desktop
//!   may bind something else, or nothing until the user picks a key, so the
//!   Settings REBIND control asks rather than sets, and the useful advice is
//!   where the desktop shows the binding.
//! * **Each rebind is a new session.** A portal session takes one
//!   `BindShortcuts` call, so [`PortalHotkeys::register_all`] closes the old
//!   session, opens another and binds the full set in one go.
//! * **Silence is the enemy.** The X11 backend running under Wayland reports
//!   success and then never fires. Here a missing portal is an error with
//!   advice the user can follow.
//!
//! Two detached threads do the work, each driving one future to the end:
//!
//! * the **control** thread owns the portal proxy and the live session and
//!   carries out bind and release requests from a `std::sync::mpsc` channel;
//! * the **listener** thread owns the `Activated` stream and sends each press
//!   down a channel that [`PortalHotkeys::drain_pressed`] empties, the same
//!   way the winit loop empties the `global-hotkey` backend.
//!
//! Splitting them works because `ashpd` shares one session-bus connection
//! across the process, and the portal sends `Activated` to the connection
//! that made the session, so a stream from any proxy sees it. The UI thread
//! only ever waits a bounded time: for the startup handshake and the bind
//! reply.

use crate::{Error, Result};
use framefang_core::hotkeys::HotkeyAction;
use framefang_core::settings::Hotkey;
use framefang_core::shortcut::portal_trigger;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;

/// What the portal knows an action by: its id and the line the desktop shows
/// beside it in the keyboard settings.
///
/// The portal stores the user's binding under the id, so an id is a wire
/// format: renaming one loses every existing binding for that action.
/// (`"capture"` is the oldest; the others came later.)
fn portal_names(action: HotkeyAction) -> (&'static str, &'static str) {
    match action {
        HotkeyAction::Capture           => ("capture", "Take a screenshot"),
        HotkeyAction::CaptureFullScreen => ("capture-full-screen", "Screenshot the whole screen"),
        HotkeyAction::ToggleRecording   => ("toggle-recording", "Start or stop recording the screen"),
        HotkeyAction::CaptureLastRegion => ("capture-last-region", "Screenshot the last region again"),
        HotkeyAction::CaptureText       => ("copy-screen-text", "Copy text from part of the screen"),
        HotkeyAction::CaptureQr         => ("read-qr-code", "Read a QR code from the screen"),
        HotkeyAction::CaptureScroll     => ("scrolling-capture", "Screenshot a page that scrolls"),
    }
}

/// Which action an `Activated` signal is about.
fn action_named(id: &str) -> Option<HotkeyAction> {
    HotkeyAction::ALL.into_iter().find(|&action| portal_names(action).0 == id)
}

/// How long `new` waits for the portal to prove it exists: making the proxy
/// and one `CreateSession` call. Both answer in milliseconds from a running
/// service; the slack is for an `xdg-desktop-portal` started on demand.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `register_all` waits to hear how the bind went. Running out only
/// means we stop waiting to report; the bind itself carries on.
///
/// GNOME and KDE reply to `BindShortcuts` at once. A portal that shows the
/// user a dialog first could take minutes, and this wait is on the UI
/// thread, so no answer counts as "still pending, probably fine". Guessing
/// wrong costs a missing toast; waiting longer would freeze the tray.
const BIND_TIMEOUT: Duration = Duration::from_secs(2);

type Reply = std::result::Result<(), String>;

/// Work for the control thread.
enum Request {
    /// Replace any live session with one that binds all of these, and say
    /// over the sender whether the portal took the request.
    Bind(Vec<(HotkeyAction, String)>, Sender<Reply>),
    /// Close the live session and leave nothing bound.
    Release,
}

/// The app's registration with the GlobalShortcuts portal.
///
/// Dropping it hangs up both channels: the control thread then closes the
/// session and both threads end.
pub struct PortalHotkeys {
    requests: Sender<Request>,
    pressed: Receiver<HotkeyAction>,
}

impl PortalHotkeys {
    /// Reach the portal and check that it answers.
    ///
    /// The error text is for the user, not a log: we only get here on
    /// Wayland, where the alternative is a shortcut that looks fine and
    /// never fires.
    pub fn new() -> Result<Self> {
        let (requests, inbox) = channel::<Request>();
        let (ready_tx, ready) = channel::<Reply>();
        let (press_tx, pressed) = channel::<HotkeyAction>();

        on_thread("framefang-portal-hotkey", "shortcut portal",
                  move || pollster::block_on(serve_requests(inbox, ready_tx)))?;

        let Ok(handshake) = ready.recv_timeout(HANDSHAKE_TIMEOUT) else {
            let secs = HANDSHAKE_TIMEOUT.as_secs();
            return Err(Error::Hotkey(format!(
                "no reply from the desktop's global shortcuts portal after {secs} seconds. Check \
                 that xdg-desktop-portal is installed and running together with your desktop's \
                 backend for it (xdg-desktop-portal-gnome or -kde, for example). Until it \
                 answers, use the tray menu to capture."
            )));
        };
        handshake.map_err(Error::Hotkey)?;

        // Started only after the handshake: listening to a portal that isn't
        // there would wait forever on a stream that never yields.
        on_thread("framefang-portal-hotkey-rx", "shortcut listener",
                  move || pollster::block_on(forward_presses(press_tx)))?;

        Ok(PortalHotkeys { requests, pressed })
    }

    /// Ask the desktop for every binding in `bindings`, replacing whatever
    /// was asked for before.
    ///
    /// Behaves like `HotkeyManager::register_all`: a key the portal has no
    /// trigger syntax for is reported and left out, and the rest go in one
    /// `BindShortcuts`. If the portal turns that call down, every action in
    /// it is reported, because none of them will fire. Success means the
    /// portal accepted the request, not that the keys work; only the
    /// compositor knows that (see the module notes).
    pub fn register_all(&mut self, bindings: &[(HotkeyAction, Hotkey)])
        -> Vec<(HotkeyAction, Error)>
    {
        let mut failures = Vec::new();
        let mut wanted = Vec::with_capacity(bindings.len());
        for (action, key) in bindings {
            match portal_trigger(key) {
                Some(trigger) => wanted.push((*action, trigger)),
                None => failures.push((*action, Error::Hotkey(format!(
                    "Wayland desktops can't be asked for {}, so choose another key in Settings.",
                    key.describe()
                )))),
            }
        }
        if wanted.is_empty() {
            self.unregister_all();
            return failures;
        }

        let asked: Vec<HotkeyAction> = wanted.iter().map(|(action, _)| *action).collect();
        let fail_all = |failures: &mut Vec<(HotkeyAction, Error)>, why: &str| {
            failures.extend(asked.iter().map(|&a| (a, Error::Hotkey(why.to_owned()))));
        };

        let (reply_tx, reply) = channel();
        if self.requests.send(Request::Bind(wanted, reply_tx)).is_err() {
            fail_all(&mut failures, "the shortcut portal thread has stopped");
            return failures;
        }
        // A timeout is left unreported on purpose; see BIND_TIMEOUT.
        if let Ok(Err(why)) = reply.recv_timeout(BIND_TIMEOUT) {
            fail_all(&mut failures, &why);
        }
        failures
    }

    /// Release every binding. Best effort: a caller couldn't act on a
    /// failure, and dropping `self` closes the session anyway.
    pub fn unregister_all(&mut self) {
        let _ = self.requests.send(Request::Release);
    }

    /// Every action pressed since the last call, each listed once however
    /// many times it fired, in the same shape `HotkeyManager::drain_pressed`
    /// gives so the event loop needn't know which backend it has.
    pub fn drain_pressed(&self) -> Vec<HotkeyAction> {
        let mut seen = Vec::new();
        // Stops when the queue is empty, or for good if the listener died.
        while let Ok(action) = self.pressed.try_recv() {
            if !seen.contains(&action) {
                seen.push(action);
            }
        }
        seen
    }
}

/// Run `work` on a detached thread called `name`; `what` names it in the
/// error if the thread can't be made.
fn on_thread(name: &str, what: &str, work: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .map(drop)
        .map_err(|e| Error::Hotkey(format!("the {what} thread wouldn't start: {e}")))
}

/// The control thread's work: own the portal proxy and the live session for
/// as long as the app runs.
///
/// Blocking on `recv()` between requests is fine inside the future because
/// zbus runs its connection on its own executor thread, not this one.
async fn serve_requests(inbox: Receiver<Request>, ready: Sender<Reply>) {
    // A proxy can be made on a bus with no portal behind it, so only a real
    // call (the first session) completes the handshake.
    let opened = match GlobalShortcuts::new().await {
        Ok(portal) => portal.create_session().await.map(|first| (portal, first)),
        Err(e) => Err(e),
    };
    let (portal, first) = match opened {
        Ok(both) => both,
        Err(e) => {
            ready.send(Err(no_portal(&e))).ok();
            return;
        }
    };
    ready.send(Ok(())).ok();
    let mut live = Some(first);

    while let Ok(request) = inbox.recv() {
        // Both requests start by closing the old session. For a bind that
        // also stops the desktop listing two bindings for one id.
        if let Some(old) = live.take() {
            old.close().await.ok();
        }
        let Request::Bind(wanted, reply) = request else { continue };

        let session = match portal.create_session().await {
            Ok(s) => s,
            Err(e) => {
                reply.send(Err(no_portal(&e))).ok();
                continue;
            }
        };
        let outcome = match portal.bind_shortcuts(&session, &new_shortcuts(&wanted), None).await {
            Ok(pending) => pending.response().map(drop).map_err(|e| refused(&e)),
            Err(e) => Err(refused(&e)),
        };
        live = Some(session);
        reply.send(outcome).ok();
    }

    // `PortalHotkeys` is gone. Close the session so the desktop doesn't keep
    // a binding nobody listens to.
    if let Some(old) = live {
        old.close().await.ok();
    }
}

/// The portal's description of each wanted shortcut.
fn new_shortcuts(wanted: &[(HotkeyAction, String)]) -> Vec<NewShortcut> {
    let mut list = Vec::with_capacity(wanted.len());
    for (action, trigger) in wanted {
        let (id, description) = portal_names(*action);
        list.push(NewShortcut::new(id, description).preferred_trigger(trigger.as_str()));
    }
    list
}

/// The listener thread's work: pass `Activated` signals for our ids on to
/// `pressed`.
///
/// Ends when `PortalHotkeys` is dropped or the stream closes (the portal
/// restarted). The tray menu still works without it, so it says why on
/// stderr and stops instead of taking the app down.
async fn forward_presses(pressed: Sender<HotkeyAction>) {
    let portal = match GlobalShortcuts::new().await {
        Ok(p) => p,
        Err(e) => return eprintln!("framefang: couldn't open the shortcut portal to listen for presses: {e}"),
    };
    let activations = match portal.receive_activated().await {
        Ok(s) => s,
        Err(e) => return eprintln!("framefang: couldn't subscribe to shortcut presses: {e}"),
    };
    // Pinned on the heap because the stream type promises no `Unpin`.
    let mut activations = Box::pin(activations);

    while let Some(signal) = activations.next().await {
        // The signal goes to the whole connection, and every rebind makes a
        // new session handle, so the stable shortcut id is what tells us
        // which key it was.
        let Some(action) = action_named(signal.shortcut_id()) else { continue };
        if pressed.send(action).is_err() {
            return;
        }
    }
    eprintln!("framefang: the shortcut portal stopped sending presses; the capture shortcut \
               won't work again until Frame Fang restarts.");
}

/// For when nothing answers on the portal's name.
fn no_portal(e: &ashpd::Error) -> String {
    format!(
        "on Wayland the capture shortcut has to be registered through \
         xdg-desktop-portal, and the portal gave no answer ({e}). Install and start \
         xdg-desktop-portal with your desktop's backend for it (xdg-desktop-portal-gnome, \
         -kde or -wlr, for example), or capture from the tray menu."
    )
}

/// For when the portal is there but turns the binding down.
fn refused(e: &ashpd::Error) -> String {
    format!(
        "your desktop turned down the capture shortcut ({e}). Some desktops want you to \
         set it yourself: look for Frame Fang under Settings > Keyboard > Shortcuts. \
         Until then, capture from the tray menu."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_its_own_id_and_comes_back_from_it() {
        let ids: Vec<&str> = HotkeyAction::ALL.into_iter().map(|a| portal_names(a).0).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "{id} is used twice");
        }
        for action in HotkeyAction::ALL {
            assert_eq!(action_named(portal_names(action).0), Some(action));
        }
        assert_eq!(action_named("not-ours"), None);
    }

    #[test]
    fn portal_ids_never_change() {
        // The desktop keeps the user's keys under these strings.
        let ids: Vec<&str> = HotkeyAction::ALL.into_iter().map(|a| portal_names(a).0).collect();
        for id in ["capture", "capture-full-screen", "toggle-recording", "capture-last-region",
                   "copy-screen-text", "read-qr-code", "scrolling-capture"] {
            assert!(ids.contains(&id), "{id} went missing");
        }
        assert_eq!(ids.len(), 7);
    }
}
