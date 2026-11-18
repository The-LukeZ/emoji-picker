//! Velopack updates from GitHub Releases.
//!
//! The updater thread checks shortly after start and then every 6 hours, downloads a newer
//! release in the background and reports it with [`UpdateEvent::Ready`]. The user can then
//! restart from the tray; otherwise Velopack applies the downloaded package on the next start
//! (`VelopackApp::run` in `main`). When the app is not installed by Velopack (a dev run from
//! `target/`), the update manager cannot be built and the updater does nothing: no thread
//! and no network.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use velopack::sources::{GithubSource, UpdateSource};
use velopack::{UpdateCheck, UpdateManager, VelopackAsset};

/// Releases are published here by `.github/workflows/release.yml`.
const REPO_URL: &str = "https://github.com/The-LukeZ/emoji-picker";
/// Delay before the first check, so it does not compete with sign-in (network may be down).
const START_DELAY: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Reported on the updater thread; the receiver forwards it to the UI thread.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// A newer version (`"1.2.3"`) is downloaded and can be applied with
    /// [`UpdaterHandle::apply_and_restart`].
    Ready(String),
    /// Result text of a check the user started with [`UpdaterHandle::check_now`].
    Status(String),
}

/// A downloaded update waiting for a restart.
struct Pending {
    manager: UpdateManager,
    asset: VelopackAsset,
}

pub struct UpdaterHandle {
    /// `None` when the app is not installed: checks do nothing.
    tx: Option<Sender<()>>,
    pending: Arc<Mutex<Option<Pending>>>,
}

impl UpdaterHandle {
    /// Starts a check now (in the background). False when the app is not installed, so
    /// there is nothing to check; the result of a started check arrives as an
    /// [`UpdateEvent`].
    pub fn check_now(&self) -> bool {
        match &self.tx {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// Starts the Velopack updater, which waits for this process to exit, applies the
    /// downloaded update and starts the new version. True when it was started: the caller
    /// must then quit the app normally (within 60 seconds). False when no update is ready
    /// or the updater could not be started.
    pub fn apply_and_restart(&self) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(p) = pending.as_ref() else {
            return false;
        };
        match p
            .manager
            .wait_exit_then_apply_updates(&p.asset, false, true, Vec::<String>::new())
        {
            Ok(()) => {
                log!("updater: applying {} after exit", p.asset.Version);
                true
            }
            Err(e) => {
                log!("updater: starting the update failed: {e}");
                false
            }
        }
    }
}

/// The update source: GitHub Releases, or in debug and `diag` builds a local folder from
/// `EMOJI_PICKER_UPDATE_DIR` (the `vpk pack` output dir) for testing.
fn source() -> Box<dyn UpdateSource> {
    if cfg!(any(debug_assertions, feature = "diag")) {
        if let Some(dir) = std::env::var_os("EMOJI_PICKER_UPDATE_DIR").filter(|d| !d.is_empty()) {
            log!("updater: using local folder {}", dir.to_string_lossy());
            return Box::new(velopack::sources::FileSource::new(dir));
        }
    }
    Box::new(GithubSource::new(REPO_URL, None, false))
}

/// Starts the updater thread when the app is installed by Velopack.
pub fn spawn_updater(on_event: impl Fn(UpdateEvent) + Send + 'static) -> UpdaterHandle {
    let pending = Arc::new(Mutex::new(None));
    let manager = match UpdateManager::new_boxed(source(), None, None) {
        Ok(m) => m,
        Err(e) => {
            log!("updater: off (not an installed copy): {e}");
            return UpdaterHandle { tx: None, pending };
        }
    };
    log!(
        "updater: installed version {}",
        manager.get_current_version_as_string()
    );
    let (tx, rx) = mpsc::channel::<()>();
    let shared = pending.clone();
    let spawned = std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || {
            let mut next = Instant::now() + START_DELAY;
            loop {
                let manual = match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                    Ok(()) => true,
                    Err(RecvTimeoutError::Timeout) => {
                        next = Instant::now() + CHECK_INTERVAL;
                        false
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                check(&manager, &shared, manual, &on_event);
            }
        });
    match spawned {
        Ok(_) => UpdaterHandle {
            tx: Some(tx),
            pending,
        },
        Err(e) => {
            log!("updater: spawning the thread failed: {e}");
            UpdaterHandle { tx: None, pending }
        }
    }
}

/// One check: downloads a newer release and reports it. `manual` checks also report "no
/// update" and errors as [`UpdateEvent::Status`].
fn check(
    manager: &UpdateManager,
    pending: &Mutex<Option<Pending>>,
    manual: bool,
    on_event: &impl Fn(UpdateEvent),
) {
    let status = |text: String| {
        if manual {
            on_event(UpdateEvent::Status(text));
        }
    };
    let ready = pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| p.asset.Version.clone());
    let info = match manager.check_for_updates() {
        Ok(UpdateCheck::UpdateAvailable(info)) => info,
        Ok(UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty) => {
            log!("updater: no update");
            status(format!(
                "You have the latest version (v{}).",
                manager.get_current_version_as_string()
            ));
            return;
        }
        Err(e) => {
            log!("updater: check failed: {e}");
            status(format!("Update check failed: {e}"));
            return;
        }
    };
    let version = info.TargetFullRelease.Version.clone();
    if ready.as_deref() == Some(version.as_str()) {
        status(format!(
            "Version {version} is ready: choose Restart to update in the tray menu."
        ));
        return;
    }
    log!("updater: downloading {version}");
    status(format!("Downloading version {version}..."));
    if let Err(e) = manager.download_updates(&info, None) {
        log!("updater: download failed: {e}");
        status(format!("Downloading the update failed: {e}"));
        return;
    }
    log!("updater: {version} downloaded");
    *pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending {
        manager: manager.clone(),
        asset: info.TargetFullRelease.clone(),
    });
    on_event(UpdateEvent::Ready(version));
}
