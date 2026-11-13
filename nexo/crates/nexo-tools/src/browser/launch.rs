//! How the `browser` tool starts its browser: the `[browser]` settings and the choices of an `open` call
//! (profile, visible window) become one set of launch options, in [`plan`]. Everything else here keeps the
//! running browser in step with them.

use std::path::{Path, PathBuf};

use nexo_browser::{AnyBrowser as Browser, LaunchOptions, Profile};
use nexo_core::config::{BrowserConfig, BrowserProfile, BrowserSource};
use nexo_core::ToolOutput;
use tokio::sync::MutexGuard;

use super::BrowserTool;

/// What one `open` asks for. `None` keeps what is configured, or what the running browser already has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Want {
    pub profile: Option<BrowserProfile>,
    pub visible: Option<bool>,
}

/// How a browser was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mode {
    pub profile: BrowserProfile,
    pub headless: bool,
}

#[derive(Debug)]
pub(super) struct Plan {
    pub options: LaunchOptions,
    pub mode: Mode,
    /// A window would have been shown by default, but there is no screen: the browser runs without one.
    pub no_screen: bool,
}

/// Where the kept profile lives: `<data folder>/browser/profile`, one for all of nexo's browsers.
pub(super) fn profile_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("browser").join("profile")
}

/// The launch options for the settings `cfg` and the request `want`, or why nothing can start.
/// `display` says whether a window can be shown (see `nexo_browser`'s check: false on a Linux box with no screen).
pub(super) fn plan(cfg: &BrowserConfig, want: &Want, data_dir: &Path, display: bool) -> Result<Plan, String> {
    let profile = want.profile.unwrap_or(cfg.profile);
    // a window by default only where the person has something to do in it: signing in to a kept profile
    let asked = want.visible.is_some() || cfg.headless.is_some();
    let mut headless = match (want.visible, cfg.headless) {
        (Some(visible), _) => !visible,
        (None, Some(h)) => h,
        (None, None) => profile == BrowserProfile::Ephemeral,
    };
    let mut no_screen = false;
    if !headless && !display {
        if asked {
            return Err(
                "browser: no screen to show a window on (no DISPLAY or WAYLAND_DISPLAY); open it with visible=false"
                    .into(),
            );
        }
        headless = true;
        no_screen = true;
    }
    let executable = match (cfg.path.as_deref().map(str::trim).filter(|p| !p.is_empty()), cfg.source) {
        (Some(path), _) => Some(expand(path)),
        (None, BrowserSource::OwnChrome) => {
            return Err("browser: browser.source = \"own-chrome\" needs nexo's own Chrome, which this build does not \
                        download; use source = \"installed\" or set browser.path"
                .into())
        }
        // `auto` is the installed browser until nexo keeps one of its own
        (None, BrowserSource::Installed | BrowserSource::Auto) => None,
    };
    let kept = match profile {
        BrowserProfile::Ephemeral => Profile::Ephemeral,
        BrowserProfile::Persistent => Profile::Persistent(profile_dir(data_dir)),
    };
    let window = Some((cfg.window.width, cfg.window.height));
    Ok(Plan {
        options: LaunchOptions { executable, headless, profile: kept, window },
        mode: Mode { profile, headless },
        no_screen,
    })
}

/// What a call does not say stays as the running browser has it. The window goes along only while the profile
/// stays: a call that moves to another profile gets that profile's own default (a kept profile is there to sign
/// in, so it shows a window).
pub(super) fn inherit(want: Want, running: Option<Mode>) -> Want {
    let Some(m) = running else { return want };
    let same_profile = want.profile.is_none_or(|p| p == m.profile);
    let visible = if same_profile { want.visible.or(Some(!m.headless)) } else { want.visible };
    Want { profile: want.profile.or(Some(m.profile)), visible }
}

/// `~/chrome` as a path of this machine.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(path), |h| h.join(rest)),
        None => PathBuf::from(path),
    }
}

/// `persistent profile, visible window`
fn describe(mode: Mode) -> String {
    let profile = match mode.profile {
        BrowserProfile::Persistent => "persistent profile (logins are kept)",
        BrowserProfile::Ephemeral => "clean profile (deleted at close)",
    };
    format!("{profile}, {}", if mode.headless { "no window" } else { "visible window" })
}

impl BrowserTool {
    /// Makes sure a browser is running, starting one with `want` when none is (or when the window of the last one
    /// was closed). The answer is a note for the model when it had to start a new one over a lost one.
    pub(super) async fn ensure(
        &self,
        guard: &mut MutexGuard<'_, Option<Browser>>,
        want: Want,
    ) -> Result<Option<String>, String> {
        let lost = match guard.as_ref() {
            Some(b) if b.is_alive().await => return Ok(None),
            Some(_) => true,
            None => false,
        };
        let remembered = *lock(&self.mode);
        // a lost browser comes back the way it was
        let want = match (lost, remembered) {
            (true, Some(m)) => Want { profile: Some(m.profile), visible: Some(!m.headless) },
            _ => want,
        };
        if let Some(old) = guard.take() {
            let _ = old.close().await;
        }
        let mode = self.start(guard, want).await?;
        if lost {
            return Ok(Some("the browser had been closed, so a new one was started; its pages are gone".to_string()));
        }
        // the model learns of a window or kept logins it did not ask for in this call (they come from `[browser]`)
        let plain = Mode { profile: BrowserProfile::Ephemeral, headless: true };
        Ok((mode != plain).then(|| format!("browser started: {}", describe(mode))))
    }

    async fn start(&self, guard: &mut MutexGuard<'_, Option<Browser>>, want: Want) -> Result<Mode, String> {
        let plan = plan(&self.config, &want, &self.data_dir(), nexo_browser::has_display())?;
        let browser = match Browser::launch(plan.options).await {
            Ok(b) => b,
            Err(e) => {
                // whatever ran before is closed by now
                *lock(&self.mode) = None;
                return Err(format!("browser: could not start: {e}"));
            }
        };
        **guard = Some(browser);
        *lock(&self.mode) = Some(plan.mode);
        Ok(plan.mode)
    }

    /// `open`: starts the browser with the profile and window asked for, restarts it when it runs differently,
    /// then goes to `url`.
    pub(super) async fn open(
        &self,
        guard: &mut MutexGuard<'_, Option<Browser>>,
        want: Want,
        url: Option<&str>,
    ) -> ToolOutput {
        // what a call does not say stays as the running browser has it
        // the mode describes nexo's own browser only; the person's own browser has none
        let current = if guard.as_ref().is_some_and(Browser::is_real) { None } else { *lock(&self.mode) };
        let alive = match guard.as_ref() {
            Some(b) => b.is_alive().await,
            None => false,
        };
        let want = inherit(want, current.filter(|_| alive));
        let planned = match plan(&self.config, &want, &self.data_dir(), nexo_browser::has_display()) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        let (wanted, no_screen) = (planned.mode, planned.no_screen);
        let mut notes: Vec<String> = Vec::new();
        if alive && current == Some(wanted) {
            notes.push(format!("the browser is already open: {}", describe(wanted)));
        } else {
            // refuse what is known beforehand while the running browser is still there: a busy profile, a missing
            // program. The holder of a kept profile is the running browser itself when it has that profile too.
            let holder_is_ours = alive && current.is_some_and(|m| m.profile == BrowserProfile::Persistent);
            let free = !(holder_is_ours && wanted.profile == BrowserProfile::Persistent);
            if let Err(e) = nexo_browser::preflight(&planned.options, free) {
                let kept = if alive { "; the browser that was running is untouched" } else { "" };
                return ToolOutput::error(format!("browser: could not start: {e}{kept}"));
            }
            if let Some(old) = guard.take() {
                let _ = old.close().await;
                if alive {
                    notes.push("restarted the browser to change how it runs; its open pages are gone".into());
                }
            }
            match self.start(guard, want).await {
                Ok(mode) => notes.push(format!("browser started: {}", describe(mode))),
                Err(e) => {
                    let gone = if alive { "; the browser that was running was closed" } else { "" };
                    return ToolOutput::error(format!("{e}{gone}"));
                }
            }
        }
        let mode = wanted;
        if mode.profile == BrowserProfile::Persistent {
            if mode.headless && no_screen {
                notes.push(
                    "logins already kept in the profile are used. There is no screen on this machine, so no window can \
                     be shown: signing in to a new site needs a machine with a screen."
                        .into(),
                );
            } else if mode.headless {
                notes.push(
                    "logins already kept in the profile are used. To sign in to a new site, open it with visible=true."
                        .into(),
                );
            } else {
                notes.push(
                    "The window is on screen: for a site that needs a login, ask the person to sign in there, wait until \
                     they say they are done, then go on; the login stays in the profile for later sessions."
                        .into(),
                );
            }
        }
        let Some(b) = guard.as_ref() else { return ToolOutput::error("browser: could not start") };
        if let Some(url) = url {
            if let Err(e) = b.navigate(url).await {
                return ToolOutput::error(format!("{}\nbrowser: could not open {url}: {e}", notes.join("\n")));
            }
            notes.push(format!("opened {url}"));
        }
        ToolOutput::ok(format!("{}\n\n{}", notes.join("\n"), self.page(b).await))
    }

    /// `close`: the browser goes, and the mode with it.
    pub(super) async fn close(&self, guard: &mut MutexGuard<'_, Option<Browser>>) -> ToolOutput {
        *lock(&self.mode) = None;
        match guard.take() {
            Some(b) => match (b.is_real(), b.close().await) {
                (real, Ok(())) => ToolOutput::ok(super::real::closed(real)),
                (_, Err(e)) => ToolOutput::ok(format!("browser closed (with a warning: {e})")),
            },
            None => ToolOutput::ok("no browser is open"),
        }
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::config::WindowSize;

    fn cfg(f: impl FnOnce(&mut BrowserConfig)) -> BrowserConfig {
        let mut c = BrowserConfig::default();
        f(&mut c);
        c
    }

    fn go(c: &BrowserConfig, w: Want, display: bool) -> Result<Plan, String> {
        plan(c, &w, Path::new("/data"), display)
    }

    #[test]
    fn the_default_is_a_clean_headless_browser_of_1280_by_800() {
        let p = go(&BrowserConfig::default(), Want::default(), true).unwrap();
        assert_eq!(p.mode, Mode { profile: BrowserProfile::Ephemeral, headless: true });
        assert!(p.options.headless && p.options.profile == Profile::Ephemeral);
        assert_eq!((p.options.executable, p.options.window), (None, Some((1280, 800))));
    }

    #[test]
    fn a_kept_profile_shows_its_window_by_default_in_nexos_data_folder() {
        let c = cfg(|c| c.profile = BrowserProfile::Persistent);
        let p = go(&c, Want::default(), true).unwrap();
        assert_eq!(p.mode, Mode { profile: BrowserProfile::Persistent, headless: false });
        assert_eq!(p.options.profile, Profile::Persistent(Path::new("/data").join("browser").join("profile")));
        // without a screen it quietly runs headless, since nobody asked for the window
        let p = go(&c, Want::default(), false).unwrap();
        assert!(p.mode.headless);
    }

    #[test]
    fn a_call_beats_the_settings_and_the_settings_beat_the_defaults() {
        let c = cfg(|c| c.headless = Some(false));
        assert!(!go(&c, Want::default(), true).unwrap().mode.headless);
        assert!(go(&c, Want { visible: Some(false), ..Want::default() }, true).unwrap().mode.headless);
        let c = cfg(|c| {
            c.profile = BrowserProfile::Persistent;
            c.headless = Some(true);
        });
        assert!(go(&c, Want::default(), true).unwrap().mode.headless);
        let w = Want { profile: Some(BrowserProfile::Ephemeral), visible: Some(true) };
        let p = go(&BrowserConfig::default(), w, true).unwrap();
        assert_eq!(p.mode, Mode { profile: BrowserProfile::Ephemeral, headless: false });
    }

    #[test]
    fn a_running_browsers_window_is_inherited_only_while_the_profile_stays() {
        let headless = Mode { profile: BrowserProfile::Ephemeral, headless: true };
        let persistent = BrowserProfile::Persistent;
        // nothing running: the call as it is
        assert_eq!(inherit(Want::default(), None), Want::default());
        // the usual flow: navigate (headless, clean), then `open profile=persistent` with no `visible`
        let w = inherit(Want { profile: Some(persistent), visible: None }, Some(headless));
        assert_eq!(w, Want { profile: Some(persistent), visible: None });
        let p = go(&BrowserConfig::default(), w, true).unwrap();
        assert_eq!(p.mode, Mode { profile: persistent, headless: false }, "the kept profile gets its window");
        // same profile, nothing said: stays as it runs
        assert_eq!(inherit(Want::default(), Some(headless)).visible, Some(false));
        let shown = Mode { profile: persistent, headless: false };
        assert_eq!(inherit(Want { profile: Some(persistent), visible: None }, Some(shown)).visible, Some(true));
        // an explicit `visible` always wins
        assert_eq!(inherit(Want { profile: None, visible: Some(true) }, Some(headless)).visible, Some(true));
        // back to a clean profile: its own default (hidden), not the window of the kept one
        let w = inherit(Want { profile: Some(BrowserProfile::Ephemeral), visible: None }, Some(shown));
        assert!(go(&BrowserConfig::default(), w, true).unwrap().mode.headless);
    }

    #[test]
    fn a_missing_screen_is_reported_when_it_turned_a_default_window_into_none() {
        let c = cfg(|c| c.profile = BrowserProfile::Persistent);
        assert!(go(&c, Want::default(), false).unwrap().no_screen);
        assert!(!go(&c, Want::default(), true).unwrap().no_screen);
        assert!(!go(&BrowserConfig::default(), Want::default(), false).unwrap().no_screen, "clean is headless anyway");
    }

    #[test]
    fn a_window_that_was_asked_for_and_cannot_be_shown_is_an_error() {
        let ask = Want { visible: Some(true), ..Want::default() };
        assert!(go(&BrowserConfig::default(), ask, false).unwrap_err().contains("no screen"));
        let c = cfg(|c| c.headless = Some(false));
        assert!(go(&c, Want::default(), false).unwrap_err().contains("visible=false"));
        assert!(go(&c, Want { visible: Some(false), ..Want::default() }, false).is_ok());
    }

    #[test]
    fn path_and_window_reach_the_launch_options() {
        let c = cfg(|c| {
            c.path = Some(" /opt/chrome/chrome ".into());
            c.window = WindowSize { width: 1024, height: 768 };
        });
        let p = go(&c, Want::default(), true).unwrap();
        assert_eq!(p.options.executable, Some(PathBuf::from("/opt/chrome/chrome")));
        assert_eq!(p.options.window, Some((1024, 768)));
        let c = cfg(|c| c.path = Some("~/bin/chrome".into()));
        let exe = go(&c, Want::default(), true).unwrap().options.executable.unwrap();
        assert!(exe.ends_with("bin/chrome") && exe.is_absolute() || dirs::home_dir().is_none(), "{exe:?}");
        let c = cfg(|c| c.path = Some("  ".into()));
        assert_eq!(go(&c, Want::default(), true).unwrap().options.executable, None, "a blank path is no path");
    }

    #[test]
    fn the_source_picks_the_browser() {
        let c = cfg(|c| c.source = BrowserSource::Auto);
        assert_eq!(go(&c, Want::default(), true).unwrap().options.executable, None);
        let c = cfg(|c| c.source = BrowserSource::OwnChrome);
        assert!(go(&c, Want::default(), true).unwrap_err().contains("own-chrome"));
        let c = cfg(|c| {
            c.source = BrowserSource::OwnChrome;
            c.path = Some("/x/chrome".into());
        });
        assert!(go(&c, Want::default(), true).is_ok(), "an explicit path wins over the source");
    }
}
