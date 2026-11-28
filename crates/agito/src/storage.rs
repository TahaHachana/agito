//! Storage state: a context's cookies and local storage, saved to reuse later, most often to
//! start tests already signed in.
//!
//! The file format is Playwright's `storageState` JSON, so state files can move between tools:
//!
//! ```json
//! {
//!   "cookies": [
//!     { "name": "session", "value": "abc", "domain": "shop.test", "path": "/",
//!       "expires": -1, "httpOnly": true, "secure": true, "sameSite": "Lax" }
//!   ],
//!   "origins": [
//!     { "origin": "https://shop.test", "localStorage": [{ "name": "token", "value": "xyz" }] }
//!   ]
//! }
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A context's cookies and local storage. Read one with
/// [`BrowserContext::storage_state`](crate::BrowserContext::storage_state), and start a context
/// from one with [`ContextOptions::storage_state`](crate::ContextOptions::storage_state).
///
/// ```no_run
/// # async fn example(context: agito::BrowserContext) -> agito::Result<()> {
/// use agito::storage::StorageState;
///
/// // After signing in:
/// context.storage_state().await?.save("target/signed-in.json")?;
///
/// // Later, in another context:
/// let state = StorageState::load("target/signed-in.json")?;
/// let options = agito::ContextOptions {
///     storage_state: Some(state),
///     ..Default::default()
/// };
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageState {
    /// Every cookie in the context.
    #[serde(default)]
    pub cookies: Vec<StoredCookie>,
    /// Local storage, by origin.
    #[serde(default)]
    pub origins: Vec<OriginStorage>,
}

impl StorageState {
    /// Reads a state file.
    pub fn load(path: impl AsRef<Path>) -> Result<StorageState> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
        serde_json::from_str(&text).map_err(|e| {
            Error::invalid_argument(format!(
                "{} is not a storage state file: {e}",
                path.display()
            ))
        })
    }

    /// Writes the state to a file, creating its directory if needed.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
        }
        let json = serde_json::to_string_pretty(self).expect("a storage state serializes");
        std::fs::write(path, json).map_err(|e| Error::io(format!("writing {}", path.display()), e))
    }
}

/// A cookie as a storage state records it: everything needed to set it again, including its
/// `SameSite` policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCookie {
    /// The cookie's name.
    pub name: String,
    /// The cookie's value.
    pub value: String,
    /// The domain it is sent to; a leading `.` includes subdomains.
    pub domain: String,
    /// The path it is sent for.
    pub path: String,
    /// When it expires, in seconds since the epoch, or `-1` for a session cookie.
    pub expires: f64,
    /// Hidden from page scripts (`document.cookie`).
    pub http_only: bool,
    /// Sent over HTTPS only.
    pub secure: bool,
    /// When it is sent with cross-site requests; the browser's default (`Lax`) when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<SameSite>,
}

/// A cookie's `SameSite` policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SameSite {
    /// Only sent with same-site requests.
    Strict,
    /// Also sent when navigating to the site from another.
    Lax,
    /// Sent with every request, cross-site included; requires `secure`.
    None,
}

/// One origin's local storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginStorage {
    /// The origin, such as `https://shop.test`.
    pub origin: String,
    /// Its local storage entries.
    #[serde(default)]
    pub local_storage: Vec<StorageItem>,
}

/// One local storage entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageItem {
    /// The key.
    pub name: String,
    /// The value.
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_playwright_state_files() {
        // As Playwright writes them.
        let text = r#"{
          "cookies": [
            { "name": "sid", "value": "1", "domain": ".shop.test", "path": "/", "expires": -1,
              "httpOnly": true, "secure": true, "sameSite": "None" },
            { "name": "theme", "value": "dark", "domain": "shop.test", "path": "/",
              "expires": 1893456000, "httpOnly": false, "secure": false, "sameSite": "Lax" }
          ],
          "origins": [
            { "origin": "https://shop.test",
              "localStorage": [{ "name": "token", "value": "xyz" }] }
          ]
        }"#;
        let state: StorageState = serde_json::from_str(text).unwrap();
        assert_eq!(state.cookies[0].same_site, Some(SameSite::None));
        assert_eq!(state.cookies[1].expires, 1893456000.0);
        assert_eq!(state.origins[0].local_storage[0].value, "xyz");

        let written: serde_json::Value = serde_json::to_value(&state).unwrap();
        assert_eq!(written["cookies"][0]["httpOnly"], true);
        assert_eq!(written["cookies"][0]["sameSite"], "None");
        assert_eq!(written["origins"][0]["localStorage"][0]["name"], "token");
        assert_eq!(
            serde_json::from_value::<StorageState>(written).unwrap(),
            state
        );
    }

    #[test]
    fn missing_sections_default_to_empty() {
        let state: StorageState = serde_json::from_str("{}").unwrap();
        assert_eq!(state, StorageState::default());
    }

    #[test]
    fn files_round_trip_and_bad_ones_say_so() {
        let dir = std::env::temp_dir().join(format!("agito-storage-{}", std::process::id()));
        let path = dir.join("nested/state.json");
        let state = StorageState {
            cookies: Vec::new(),
            origins: vec![OriginStorage {
                origin: "https://a.test".into(),
                local_storage: vec![StorageItem {
                    name: "k".into(),
                    value: "v".into(),
                }],
            }],
        };
        state.save(&path).unwrap();
        assert_eq!(StorageState::load(&path).unwrap(), state);
        std::fs::write(&path, "not json").unwrap();
        let err = StorageState::load(&path).unwrap_err().to_string();
        assert!(err.contains("is not a storage state file"), "{err}");
        assert!(StorageState::load(dir.join("missing.json")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
