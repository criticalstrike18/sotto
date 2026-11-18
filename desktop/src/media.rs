// Copyright (c) 2026 Sotto
// Non-call media pause/resume for dictation.
//
// Windows exposes per-session media transport controls through SMTC. We never
// send a blind VK_MEDIA_PLAY_PAUSE: that toggles whichever app Windows happens
// to consider current and can mute a meeting. Instead the worker:
//   1. finds the SMTC session that is actually playing,
//   2. refuses known communication apps and call-looking browser titles,
//   3. pauses only that session and remembers it,
//   4. resumes only the session it paused.
//
// All COM/WinRT work happens on one dedicated MTA thread. The hotkey path only
// pushes a command, so a slow media app can never delay audio capture.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

static ENABLED: AtomicBool = AtomicBool::new(true);
static COMMANDS: OnceLock<Sender<Command>> = OnceLock::new();

enum Command {
    Pause,
    Resume(Duration),
    Shutdown(Sender<()>),
}

/// Start the media worker and apply the persisted preference. Safe to call
/// once during startup; later preference changes go through `set_enabled`.
pub fn init(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
    if enabled {
        let _ = sender();
    }
}

/// Enable or disable automatic media control for future takes. Disabling while
/// a track is paused resumes it immediately so the user is never left silent.
pub fn set_enabled(enabled: bool) {
    let was = ENABLED.swap(enabled, Ordering::SeqCst);
    if was && !enabled {
        resume_immediately();
    } else if enabled && !was {
        let _ = sender();
    }
}

/// Called at the start of a take, before the recorder opens the headset.
/// Fire-and-forget: the SMTC pause often completes before the HFP handshake.
pub fn pause_for_dictation() {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(tx) = sender_ref() {
        let _ = tx.send(Command::Pause);
    }
}

/// Called after text injection. The 140 ms settle gives the audio route a
/// moment to return to A2DP before music starts again.
pub fn resume_after_dictation() {
    if let Some(tx) = sender_ref() {
        let _ = tx.send(Command::Resume(Duration::from_millis(140)));
    }
}

/// Cancel/error/exit path: resume without waiting for A2DP.
pub fn resume_immediately() {
    if let Some(tx) = sender_ref() {
        let _ = tx.send(Command::Resume(Duration::ZERO));
    }
}

/// Ask the worker to resume and exit. Called before the process terminates.
/// Waits briefly for the worker to acknowledge so a paused track is not left
/// silent when the process exits immediately afterwards.
pub fn shutdown() {
    let Some(tx) = sender_ref() else {
        return;
    };
    let (ack_tx, ack_rx) = mpsc::channel();
    if tx.send(Command::Shutdown(ack_tx)).is_ok() {
        let _ = ack_rx.recv_timeout(Duration::from_millis(700));
    }
}

fn sender() -> &'static Sender<Command> {
    COMMANDS.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        if let Err(e) = thread::Builder::new()
            .name("sotto-media".to_string())
            .spawn(move || run_worker(rx))
        {
            eprintln!("[Media] Could not start media worker: {}", e);
        }
        tx
    })
}

fn sender_ref() -> Option<&'static Sender<Command>> {
    COMMANDS.get()
}

#[cfg(target_os = "windows")]
fn run_worker(rx: mpsc::Receiver<Command>) {
    platform::run(rx);
}

#[cfg(not(target_os = "windows"))]
fn run_worker(rx: mpsc::Receiver<Command>) {
    for _ in rx {}
}

/// Conservative policy used before touching a session.
///
/// Known communication apps are always left alone. Unknown apps are also left
/// alone: pausing a random app is worse than missing a niche music player.
pub fn should_pause(app_id: &str, title: &str) -> bool {
    let app = app_id.trim().to_lowercase();
    let title = title.trim().to_lowercase();
    if app.is_empty() || is_call_app(&app) || looks_like_call_title(&title) {
        return false;
    }
    is_known_media_app(&app)
}

fn is_call_app(app: &str) -> bool {
    const CALL_APPS: &[&str] = &[
        "teams",
        "msteams",
        "zoom",
        "webex",
        "discord",
        "slack",
        "skype",
        "whatsapp",
        "telegram",
        "facetime",
        "phone",
        "meet.google",
    ];
    CALL_APPS.iter().any(|name| app.contains(name))
}

fn looks_like_call_title(title: &str) -> bool {
    const CALL_TITLES: &[&str] = &[
        "google meet",
        "meet.google",
        "meet -",
        "zoom meeting",
        "zoom.us",
        "microsoft teams",
        "teams meeting",
        "webex meeting",
        "slack huddle",
        "discord call",
        "whatsapp call",
        "telegram call",
        "facetime",
        "in a call",
        "on a call",
        "call with",
    ];
    CALL_TITLES.iter().any(|needle| title.contains(needle))
}

fn is_known_media_app(app: &str) -> bool {
    const MEDIA_APPS: &[&str] = &[
        "spotify",
        "zunemusic",
        "zune",
        "applemusic",
        "apple music",
        "itunes",
        "amazonmusic",
        "amazon music",
        "tidal",
        "deezer",
        "soundcloud",
        "pandora",
        "vlc",
        "foobar",
        "musicbee",
        "aimp",
        "plex",
        "mpv",
        "wmplayer",
        "windows media player",
        "media player",
        "chrome",
        "msedge",
        "microsoft edge",
        "firefox",
        "brave",
        "opera",
        "vivaldi",
        "youtube",
        "bandcamp",
        "audible",
        "groove",
    ];
    MEDIA_APPS.iter().any(|name| app.contains(name))
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{should_pause, Command};
    use std::sync::mpsc::Receiver;
    use std::thread;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    struct PausedSession {
        session: GlobalSystemMediaTransportControlsSession,
        app_id: String,
        title: String,
    }

    pub(super) fn run(rx: Receiver<Command>) {
        // A dedicated MTA apartment keeps WinRT completions off the Slint STA.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
        if hr < 0 {
            eprintln!("[Media] CoInitializeEx failed: 0x{:08x}", hr);
            return;
        }

        let mut manager: Option<GlobalSystemMediaTransportControlsSessionManager> = None;
        let mut paused: Option<PausedSession> = None;

        while let Ok(command) = rx.recv() {
            match command {
                Command::Pause => {
                    if paused.is_none() {
                        paused = pause_current(&mut manager);
                    }
                }
                Command::Resume(delay) => {
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                    resume_paused(&mut paused, &mut manager);
                }
                Command::Shutdown(ack) => {
                    resume_paused(&mut paused, &mut manager);
                    let _ = ack.send(());
                    break;
                }
            }
        }

        unsafe {
            CoUninitialize();
        }
    }

    fn get_manager(
        slot: &mut Option<GlobalSystemMediaTransportControlsSessionManager>,
    ) -> Option<&GlobalSystemMediaTransportControlsSessionManager> {
        if slot.is_none() {
            match GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                .and_then(|op| op.join())
            {
                Ok(manager) => *slot = Some(manager),
                Err(e) => {
                    eprintln!("[Media] SMTC manager unavailable: {}", e);
                    return None;
                }
            }
        }
        slot.as_ref()
    }

    fn is_playing(session: &GlobalSystemMediaTransportControlsSession) -> bool {
        session
            .GetPlaybackInfo()
            .ok()
            .and_then(|info| info.PlaybackStatus().ok())
            .map(|status| {
                status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
            })
            .unwrap_or(false)
    }

    fn playing_session(
        manager: &GlobalSystemMediaTransportControlsSessionManager,
    ) -> Option<GlobalSystemMediaTransportControlsSession> {
        if let Ok(current) = manager.GetCurrentSession() {
            if is_playing(&current) {
                return Some(current);
            }
        }

        let sessions = manager.GetSessions().ok()?;
        let size = sessions.Size().unwrap_or(0);
        for index in 0..size {
            if let Ok(session) = sessions.GetAt(index) {
                if is_playing(&session) {
                    return Some(session);
                }
            }
        }
        None
    }

    fn app_id(session: &GlobalSystemMediaTransportControlsSession) -> String {
        session
            .SourceAppUserModelId()
            .map(|id| id.to_string_lossy())
            .unwrap_or_default()
    }

    fn title(session: &GlobalSystemMediaTransportControlsSession) -> String {
        session
            .TryGetMediaPropertiesAsync()
            .ok()
            .and_then(|op| op.join().ok())
            .and_then(|props| props.Title().ok())
            .map(|value| value.to_string_lossy())
            .unwrap_or_default()
    }

    fn pause_current(
        manager: &mut Option<GlobalSystemMediaTransportControlsSessionManager>,
    ) -> Option<PausedSession> {
        let manager = get_manager(manager)?;
        let session = playing_session(manager)?;
        let app = app_id(&session);
        let title = title(&session);

        if !should_pause(&app, &title) {
            if !app.is_empty() {
                println!(
                    "[Media] Leaving session untouched (call/unknown): app={:?} title={:?}",
                    app, title
                );
            }
            return None;
        }

        match session.TryPauseAsync().and_then(|op| op.join()) {
            Ok(true) => {
                println!(
                    "[Media] Paused for dictation: app={:?} title={:?}",
                    app, title
                );
                Some(PausedSession {
                    session,
                    app_id: app,
                    title,
                })
            }
            Ok(false) => {
                println!("[Media] Session declined pause: app={:?}", app);
                None
            }
            Err(e) => {
                eprintln!("[Media] Pause failed for {:?}: {}", app, e);
                None
            }
        }
    }

    fn resume_paused(
        paused: &mut Option<PausedSession>,
        manager: &mut Option<GlobalSystemMediaTransportControlsSessionManager>,
    ) {
        let Some(session) = paused.take() else {
            return;
        };

        // Always request play, even if the status still reads Playing. Some
        // players report the old status for a few frames after pausing, and
        // guarding on it could leave a very short take silent. TryPlayAsync is
        // not a toggle, so it is harmless when playback already resumed.
        if matches!(
            session.session.TryPlayAsync().and_then(|op| op.join()),
            Ok(true)
        ) {
            println!(
                "[Media] Resumed after dictation: app={:?} title={:?}",
                session.app_id, session.title
            );
            return;
        }

        // The original COM session can expire while a long transcription runs.
        // Fall back to the same app's current session, never the global current
        // session (which might be a call).
        if let Some(manager) = get_manager(manager) {
            if let Ok(sessions) = manager.GetSessions() {
                let size = sessions.Size().unwrap_or(0);
                for index in 0..size {
                    if let Ok(candidate) = sessions.GetAt(index) {
                        if app_id(&candidate) == session.app_id
                            && matches!(candidate.TryPlayAsync().and_then(|op| op.join()), Ok(true))
                        {
                            println!(
                                "[Media] Resumed replacement session: app={:?}",
                                session.app_id
                            );
                            return;
                        }
                    }
                }
            }
        }

        eprintln!("[Media] Could not resume session: app={:?}", session.app_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_session_is_paused() {
        assert!(should_pause(
            "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
            "A quiet track"
        ));
    }

    #[test]
    fn browser_music_session_is_paused() {
        assert!(should_pause("MSEdge", "YouTube Music - Focus playlist"));
        assert!(should_pause("Google Chrome", "Bandcamp"));
    }

    #[test]
    fn teams_is_never_touched() {
        assert!(!should_pause("Microsoft Teams", "Weekly sync"));
        assert!(!should_pause(
            "MSTeams_8wekyb3d8bbwe!MSTeams",
            "Microsoft Teams"
        ));
    }

    #[test]
    fn browser_call_titles_are_never_touched() {
        assert!(!should_pause("Google Chrome", "Meet - abc-defg-hij"));
        assert!(!should_pause("Microsoft Edge", "Zoom meeting"));
        assert!(!should_pause("Firefox", "Slack huddle"));
        assert!(!should_pause("Chrome", "In a call with the design team"));
    }

    #[test]
    fn unknown_apps_are_left_alone() {
        assert!(!should_pause("SomeGame.exe", "Main menu"));
        assert!(!should_pause("", "A track"));
    }
}
