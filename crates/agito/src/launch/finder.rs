//! Finding a browser executable: an explicit path, one installed by `agito install`, or a
//! system installation.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Which browser build to launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserChannel {
    Chromium,
    Chrome,
    Edge,
    Firefox,
}

impl BrowserChannel {
    pub fn name(self) -> &'static str {
        match self {
            BrowserChannel::Chromium => "chromium",
            BrowserChannel::Chrome => "chrome",
            BrowserChannel::Edge => "edge",
            BrowserChannel::Firefox => "firefox",
        }
    }
}

impl std::str::FromStr for BrowserChannel {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "chromium" => Ok(BrowserChannel::Chromium),
            "chrome" => Ok(BrowserChannel::Chrome),
            "edge" | "msedge" => Ok(BrowserChannel::Edge),
            "firefox" => Ok(BrowserChannel::Firefox),
            other => Err(Error::invalid_argument(format!(
                "unknown browser channel '{other}'; expected chromium, chrome, edge or firefox"
            ))),
        }
    }
}

/// Where `agito install` puts browsers: `~/.agito/browsers`.
pub fn default_install_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".agito").join("browsers")
}

/// The executable recorded by `agito install` for `channel`, if it still exists.
/// The marker file `.installed.<channel>` holds the executable's path.
pub fn installed(root: &Path, channel: BrowserChannel) -> Option<PathBuf> {
    let marker = root.join(format!(".installed.{}", channel.name()));
    let path = PathBuf::from(std::fs::read_to_string(marker).ok()?.trim());
    path.is_file().then_some(path)
}

/// Resolves the executable to launch.
pub fn resolve(channel: Option<BrowserChannel>, executable_path: Option<&Path>) -> Result<PathBuf> {
    resolve_with(
        channel,
        executable_path,
        &default_install_root(),
        &system_candidates,
    )
}

pub(crate) fn resolve_with(
    channel: Option<BrowserChannel>,
    executable_path: Option<&Path>,
    install_root: &Path,
    candidates: &dyn Fn(BrowserChannel) -> Vec<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = executable_path {
        if !path.is_file() {
            return Err(Error::invalid_argument(format!(
                "Browser executable not found: {}",
                path.display()
            )));
        }
        return Ok(path.to_path_buf());
    }
    if let Some(channel) = channel {
        return installed(install_root, channel)
            .or_else(|| first_existing(candidates(channel)))
            .ok_or_else(|| Error::other(format!("No {} installation found.", channel.name())));
    }
    if let Some(path) = installed(install_root, BrowserChannel::Chromium) {
        return Ok(path);
    }
    for channel in [
        BrowserChannel::Chrome,
        BrowserChannel::Edge,
        BrowserChannel::Chromium,
    ] {
        if let Some(path) = first_existing(candidates(channel)) {
            return Ok(path);
        }
    }
    Err(Error::other(
        "No supported browser found. Install Chrome, Edge, or Chromium, or run 'agito install' to download Chromium.",
    ))
}

fn first_existing(paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|p| p.is_file())
}

/// Well-known install locations per platform.
pub fn system_candidates(channel: BrowserChannel) -> Vec<PathBuf> {
    let list: Vec<String> = if cfg!(target_os = "macos") {
        match channel {
            BrowserChannel::Chrome => {
                vec!["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into()]
            }
            BrowserChannel::Edge => {
                vec!["/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into()]
            }
            BrowserChannel::Chromium => {
                vec!["/Applications/Chromium.app/Contents/MacOS/Chromium".into()]
            }
            BrowserChannel::Firefox => vec![
                "/Applications/Firefox.app/Contents/MacOS/firefox".into(),
                "/Applications/Firefox Nightly.app/Contents/MacOS/firefox".into(),
            ],
        }
    } else if cfg!(windows) {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        let (local, pf, pf86) = (
            var("LOCALAPPDATA"),
            var("ProgramFiles"),
            var("ProgramFiles(x86)"),
        );
        match channel {
            BrowserChannel::Chrome => vec![
                format!(r"{pf}\Google\Chrome\Application\chrome.exe"),
                format!(r"{pf86}\Google\Chrome\Application\chrome.exe"),
                format!(r"{local}\Google\Chrome\Application\chrome.exe"),
            ],
            BrowserChannel::Edge => vec![
                format!(r"{pf}\Microsoft\Edge\Application\msedge.exe"),
                format!(r"{pf86}\Microsoft\Edge\Application\msedge.exe"),
            ],
            BrowserChannel::Chromium => vec![format!(r"{local}\Chromium\Application\chrome.exe")],
            BrowserChannel::Firefox => vec![
                format!(r"{pf}\Mozilla Firefox\firefox.exe"),
                format!(r"{pf86}\Mozilla Firefox\firefox.exe"),
            ],
        }
    } else {
        match channel {
            BrowserChannel::Chrome => vec![
                "/usr/bin/google-chrome-stable".into(),
                "/usr/bin/google-chrome".into(),
            ],
            BrowserChannel::Edge => vec![
                "/usr/bin/microsoft-edge-stable".into(),
                "/usr/bin/microsoft-edge".into(),
            ],
            BrowserChannel::Chromium => vec![
                "/usr/bin/chromium-browser".into(),
                "/usr/bin/chromium".into(),
                "/snap/bin/chromium".into(),
            ],
            BrowserChannel::Firefox => vec![
                "/usr/bin/firefox".into(),
                "/usr/bin/firefox-esr".into(),
                "/usr/local/bin/firefox".into(),
                "/snap/bin/firefox".into(),
            ],
        }
    };
    list.into_iter().map(PathBuf::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agito-finder-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn explicit_path_must_exist() {
        let err = resolve_with(
            None,
            Some(Path::new("/no/such/chrome")),
            Path::new("/x"),
            &|_| vec![],
        )
        .unwrap_err();
        assert!(err.to_string().contains("/no/such/chrome"));
    }

    #[test]
    fn installed_marker_wins_over_system() {
        let root = scratch("marker");
        let exe = root.join("chrome-bin");
        std::fs::write(&exe, "").unwrap();
        std::fs::write(root.join(".installed.chromium"), exe.to_str().unwrap()).unwrap();
        let system = root.join("system-chrome");
        std::fs::write(&system, "").unwrap();
        let found = resolve_with(None, None, &root, &|_| vec![system.clone()]).unwrap();
        assert_eq!(found, exe);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn falls_back_to_system_candidates_in_order() {
        let root = scratch("system");
        let edge = root.join("edge");
        std::fs::write(&edge, "").unwrap();
        let found = resolve_with(None, None, &root, &|c| match c {
            BrowserChannel::Edge => vec![edge.clone()],
            _ => vec![root.join("missing")],
        })
        .unwrap();
        assert_eq!(found, edge);
        assert!(resolve_with(None, None, &root, &|_| vec![]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn channel_parses() {
        assert_eq!(
            "Chrome".parse::<BrowserChannel>().unwrap(),
            BrowserChannel::Chrome
        );
        assert!("safari".parse::<BrowserChannel>().is_err());
    }
}
