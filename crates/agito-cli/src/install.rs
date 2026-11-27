//! `agito install`: downloads Chrome for Testing into the browser cache and records the
//! executable in the marker the launcher reads.
//!
//! A build unpacks to `<root>/chromium-<version>/chrome-<platform>/...`. It is extracted into a
//! hidden staging directory first and renamed into place once complete, so a directory under its
//! final name is always a whole browser: an interrupted install leaves only staging debris, which
//! a later install sweeps, never a half-extracted build that looks installed.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use agito::BrowserChannel;
use agito::launch::finder;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

/// Where build metadata and downloads come from. Tests point this at a local server.
#[derive(Debug, Clone)]
pub struct Source {
    /// Base of the JSON endpoints, such as `last-known-good-versions.json`.
    pub metadata: String,
    /// Base of the archives: `<downloads>/<version>/<platform>/chrome-<platform>.zip`.
    pub downloads: String,
}

impl Default for Source {
    fn default() -> Self {
        Source {
            metadata: "https://googlechromelabs.github.io/chrome-for-testing".into(),
            downloads: "https://storage.googleapis.com/chrome-for-testing-public".into(),
        }
    }
}

/// A Chrome for Testing platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux64,
    LinuxArm64,
    MacArm64,
    MacX64,
    Win32,
    Win64,
}

impl Platform {
    /// The platform of this machine. Windows on Arm gets the x64 build, which it runs under
    /// emulation, since Chrome for Testing publishes no Arm build for Windows.
    pub fn current() -> Result<Platform> {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        Ok(match (os, arch) {
            ("linux", "x86_64") => Platform::Linux64,
            ("linux", "aarch64") => Platform::LinuxArm64,
            ("macos", "aarch64") => Platform::MacArm64,
            ("macos", "x86_64") => Platform::MacX64,
            ("windows", "x86") => Platform::Win32,
            ("windows", "x86_64" | "aarch64") => Platform::Win64,
            _ => bail!(
                "Chrome for Testing has no build for {os}/{arch}. Install Chromium with the \
                 system package manager, or point AGITO_EXECUTABLE_PATH at a browser."
            ),
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Platform::Linux64 => "linux64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::MacArm64 => "mac-arm64",
            Platform::MacX64 => "mac-x64",
            Platform::Win32 => "win32",
            Platform::Win64 => "win64",
        }
    }

    /// The executable's path inside an extracted archive.
    pub fn executable(self) -> PathBuf {
        let top = PathBuf::from(format!("chrome-{}", self.key()));
        match self {
            Platform::Linux64 | Platform::LinuxArm64 => top.join("chrome"),
            Platform::MacArm64 | Platform::MacX64 => top
                .join("Google Chrome for Testing.app")
                .join("Contents")
                .join("MacOS")
                .join("Google Chrome for Testing"),
            Platform::Win32 | Platform::Win64 => top.join("chrome.exe"),
        }
    }
}

/// A Chrome for Testing release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseChannel {
    Stable,
    Beta,
    Dev,
    Canary,
}

impl ReleaseChannel {
    fn name(self) -> &'static str {
        match self {
            ReleaseChannel::Stable => "Stable",
            ReleaseChannel::Beta => "Beta",
            ReleaseChannel::Dev => "Dev",
            ReleaseChannel::Canary => "Canary",
        }
    }
}

/// The build `--revision` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requested {
    /// The newest build on a release channel; `stable` when `--revision` is not given.
    Channel(ReleaseChannel),
    /// An exact version, such as `131.0.6778.85`.
    Version(String),
    /// The newest build of a milestone, such as `131`.
    Milestone(u32),
    /// The build at a Chromium revision (a commit position), such as `1368529`.
    Revision(u64),
}

impl Default for Requested {
    fn default() -> Self {
        Requested::Channel(ReleaseChannel::Stable)
    }
}

/// Milestones are three digits for the foreseeable future and revisions are seven, so a plain
/// number below this is read as a milestone and anything above as a revision.
const FIRST_REVISION: u64 = 100_000;

impl FromStr for Requested {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim();
        let channel = match s.to_ascii_lowercase().as_str() {
            "stable" => Some(ReleaseChannel::Stable),
            "beta" => Some(ReleaseChannel::Beta),
            "dev" => Some(ReleaseChannel::Dev),
            "canary" => Some(ReleaseChannel::Canary),
            _ => None,
        };
        if let Some(channel) = channel {
            return Ok(Requested::Channel(channel));
        }
        let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if s.contains('.') {
            let parts: Vec<&str> = s.split('.').collect();
            if parts.len() == 4 && parts.iter().all(|p| numeric(p)) {
                return Ok(Requested::Version(s.to_string()));
            }
        } else if numeric(s)
            && let Ok(n) = s.parse::<u64>()
        {
            return Ok(if n < FIRST_REVISION {
                Requested::Milestone(n as u32)
            } else {
                Requested::Revision(n)
            });
        }
        Err(format!(
            "'{s}' is not a build: expected a version such as 131.0.6778.85, a milestone such \
             as 131, a Chromium revision such as 1368529, or stable, beta, dev or canary"
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Build {
    version: String,
    revision: String,
}

#[derive(Deserialize)]
struct Channels {
    channels: HashMap<String, Build>,
}

#[derive(Deserialize)]
struct Milestones {
    milestones: HashMap<String, Build>,
}

#[derive(Deserialize)]
struct KnownGood {
    versions: Vec<Build>,
}

fn version_on_channel(index: &Channels, channel: ReleaseChannel) -> Result<String> {
    index
        .channels
        .get(channel.name())
        .map(|b| b.version.clone())
        .ok_or_else(|| anyhow!("Chrome for Testing lists no {} build", channel.name()))
}

fn version_for_milestone(index: &Milestones, milestone: u32) -> Result<String> {
    if let Some(build) = index.milestones.get(&milestone.to_string()) {
        return Ok(build.version.clone());
    }
    let known = index
        .milestones
        .keys()
        .filter_map(|k| k.parse::<u32>().ok());
    let (oldest, newest) = (known.clone().min(), known.max());
    match (oldest, newest) {
        (Some(oldest), Some(newest)) => bail!(
            "Chrome for Testing has no build for milestone {milestone}; it covers {oldest} to {newest}"
        ),
        _ => bail!("Chrome for Testing has no build for milestone {milestone}"),
    }
}

/// Orders versions numerically, so `131.0.6778.108` comes after `131.0.6778.85`.
fn version_key(version: &str) -> Vec<u64> {
    version.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The build at exactly `revision`. Patch releases on a branch all report the branch point's
/// revision, so several versions can match; the newest, with every fix on that branch, wins.
/// Rather than quietly substituting a neighbour, a miss names the nearest builds on either side
/// so the caller can pick one.
fn version_for_revision(index: &KnownGood, revision: u64) -> Result<String> {
    let mut builds: Vec<(u64, &Build)> = index
        .versions
        .iter()
        .filter_map(|b| Some((b.revision.parse().ok()?, b)))
        .collect();
    builds.sort_by_key(|(r, b)| (*r, version_key(&b.version)));
    if let Some((_, build)) = builds.iter().rev().find(|(r, _)| *r == revision) {
        return Ok(build.version.clone());
    }
    let describe = |(r, b): &(u64, &Build)| format!("{} (revision {r})", b.version);
    let before = builds
        .iter()
        .rev()
        .find(|(r, _)| *r < revision)
        .map(describe);
    // The newest version at the next revision up, which is what asking for that revision gets.
    let after = builds
        .iter()
        .find(|(r, _)| *r > revision)
        .and_then(|(next, _)| builds.iter().rev().find(|(r, _)| r == next))
        .map(describe);
    let nearest = match (before, after) {
        (Some(b), Some(a)) => format!("; the nearest are {b} and {a}"),
        (Some(x), None) | (None, Some(x)) => format!("; the nearest is {x}"),
        (None, None) => String::new(),
    };
    bail!("Chrome for Testing has no build at Chromium revision {revision}{nearest}")
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("could not fetch {url}"))?
        .json()
        .await
        .with_context(|| format!("could not read {url}"))
}

/// The exact version `requested` names.
pub async fn resolve_version(
    client: &reqwest::Client,
    source: &Source,
    requested: &Requested,
) -> Result<String> {
    let url = |file: &str| format!("{}/{file}", source.metadata);
    match requested {
        Requested::Version(v) => Ok(v.clone()),
        Requested::Channel(channel) => version_on_channel(
            &fetch_json(client, &url("last-known-good-versions.json")).await?,
            *channel,
        ),
        Requested::Milestone(m) => version_for_milestone(
            &fetch_json(client, &url("latest-versions-per-milestone.json")).await?,
            *m,
        ),
        Requested::Revision(r) => version_for_revision(
            &fetch_json(client, &url("known-good-versions.json")).await?,
            *r,
        ),
    }
}

/// What to install and where.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub root: PathBuf,
    pub requested: Requested,
    pub platform: Platform,
    pub source: Source,
}

/// An installed build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub executable: PathBuf,
    /// False when the build was already in the cache and only its marker was written.
    pub downloaded: bool,
}

/// Installs the requested build into `options.root`, unless it is already there, and makes it
/// the one the launcher picks for the `chromium` channel.
pub async fn install(options: &InstallOptions) -> Result<Installed> {
    let root = &options.root;
    std::fs::create_dir_all(root)
        .with_context(|| format!("could not create the browser cache {}", root.display()))?;
    sweep_staging(root);

    let client = reqwest::Client::builder()
        .user_agent(concat!("agito-cli/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .build()?;
    let version = resolve_version(&client, &options.source, &options.requested).await?;
    let dir = root.join(format!("chromium-{version}"));
    let executable = dir.join(options.platform.executable());

    let downloaded = if executable.is_file() {
        eprintln!("Chromium {version} is already installed.");
        false
    } else {
        if dir.exists() {
            // Not a build this installer finished: staging is renamed into place only once the
            // executable is in it. Something else put it there, so it is replaced.
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("could not remove the incomplete {}", dir.display()))?;
        }
        download_and_extract(&client, options, &version, &dir).await?;
        true
    };

    write_marker(root, &executable)?;
    grant_sandbox_access(&dir).await;
    Ok(Installed {
        version,
        executable,
        downloaded,
    })
}

/// Staging entries share this prefix, so a sweep never touches anything else in the cache.
const STAGING_PREFIX: &str = ".staging-";

/// Removes what interrupted installs left behind. Only entries a day old are removed, so an
/// install running alongside this one keeps its staging.
fn sweep_staging(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff);
        if stale {
            let path = entry.path();
            let _ = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
    }
}

/// Deletes a path when dropped, so a failed install cleans up after itself.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = if self.0.is_dir() {
            std::fs::remove_dir_all(&self.0)
        } else {
            std::fs::remove_file(&self.0)
        };
    }
}

async fn download_and_extract(
    client: &reqwest::Client,
    options: &InstallOptions,
    version: &str,
    dir: &Path,
) -> Result<()> {
    let key = options.platform.key();
    let url = format!(
        "{}/{version}/{key}/chrome-{key}.zip",
        options.source.downloads
    );
    let tag = format!("{STAGING_PREFIX}chromium-{version}-{}", std::process::id());
    let archive = Cleanup(options.root.join(format!("{tag}.zip")));
    let staging = Cleanup(options.root.join(&tag));

    eprintln!("Downloading Chromium {version} for {key}...");
    download(client, &url, &archive.0).await.map_err(|e| {
        match e.downcast_ref::<reqwest::Error>().and_then(|e| e.status()) {
            Some(reqwest::StatusCode::NOT_FOUND) => anyhow!(
                "Chrome for Testing has no {version} build for {key} ({url} was not found). \
                 The versions it has are listed at \
                 https://googlechromelabs.github.io/chrome-for-testing/"
            ),
            _ => e,
        }
    })?;

    eprintln!("Extracting...");
    let (from, to) = (archive.0.clone(), staging.0.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&from)?;
        zip::ZipArchive::new(file)
            .and_then(|mut zip| zip.extract(&to))
            .with_context(|| format!("could not extract {}", from.display()))
    })
    .await??;

    let executable = options.platform.executable();
    if !staging.0.join(&executable).is_file() {
        bail!(
            "the archive from {url} does not contain {}",
            executable.display()
        );
    }
    if let Err(err) = std::fs::rename(&staging.0, dir) {
        // Another install of the same build may have finished first; its copy is as good.
        if !dir.join(&executable).is_file() {
            return Err(err)
                .with_context(|| format!("could not move the build to {}", dir.display()));
        }
    }
    Ok(())
}

async fn download(client: &reqwest::Client, url: &str, to: &Path) -> Result<()> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let total = response.content_length();
    let mut file = tokio::fs::File::create(to)
        .await
        .with_context(|| format!("could not create {}", to.display()))?;
    let mut progress = Progress::new(total);
    let mut received = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("the download of {url} failed"))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        received += chunk.len() as u64;
        progress.update(received);
    }
    progress.finish();
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    if let Some(total) = total
        && received != total
    {
        bail!("the download of {url} stopped after {received} of {total} bytes");
    }
    Ok(())
}

/// A percentage on one terminal line. Silent when stderr is not a terminal, so CI logs are not
/// filled with carriage returns.
struct Progress {
    total: Option<u64>,
    shown: Option<u64>,
    enabled: bool,
}

impl Progress {
    fn new(total: Option<u64>) -> Self {
        Progress {
            total,
            shown: None,
            enabled: total.is_some() && std::io::stderr().is_terminal(),
        }
    }

    fn update(&mut self, received: u64) {
        let Some(total) = self.total.filter(|t| *t > 0 && self.enabled) else {
            return;
        };
        let percent = received * 100 / total;
        if self.shown != Some(percent) {
            self.shown = Some(percent);
            let mb = |b: u64| b as f64 / 1_048_576.0;
            eprint!(
                "\r  {percent:>3}%  {:.1} / {:.1} MB",
                mb(received),
                mb(total)
            );
            let _ = std::io::stderr().flush();
        }
    }

    fn finish(&self) {
        if self.shown.is_some() {
            eprintln!();
        }
    }
}

/// Points the `chromium` marker at `executable`. Written beside the marker and renamed over it,
/// so a launcher reading it never sees half a path.
fn write_marker(root: &Path, executable: &Path) -> Result<()> {
    let marker = finder::marker_path(root, BrowserChannel::Chromium);
    let temp = root.join(format!("{STAGING_PREFIX}marker-{}", std::process::id()));
    let path = executable
        .to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", executable.display()))?;
    std::fs::write(&temp, path)
        .and_then(|()| std::fs::rename(&temp, &marker))
        .with_context(|| format!("could not write {}", marker.display()))
}

/// Chromium runs some child processes on Windows, the network service among them, under the
/// ALL RESTRICTED APPLICATION PACKAGES identity, and they read the browser's own files. An
/// installer grants that access; a browser unzipped into a user's directory lacks it, so the
/// first such child dies on every launch and logs a sandbox error. This grants what an installer
/// would. It is named by SID because the readable name is localised. A failure is reported, not
/// raised: the browser still runs without it.
async fn grant_sandbox_access(dir: &Path) {
    if !cfg!(windows) {
        return;
    }
    let mut command = tokio::process::Command::new("icacls");
    command
        .args(sandbox_acl_arguments(dir))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let outcome = tokio::time::timeout(Duration::from_secs(60), command.output()).await;
    let problem = match outcome {
        Err(_) => "granting it did not finish within a minute".to_string(),
        Ok(Err(e)) => e.to_string(),
        Ok(Ok(out)) if !out.status.success() => {
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        }
        Ok(Ok(_)) => return,
    };
    eprintln!(
        "warning: could not give the browser sandbox access to {}. The browser still runs, but \
         may log that it cannot reach its own executable each time it starts. {problem}",
        dir.display()
    );
}

fn sandbox_acl_arguments(dir: &Path) -> Vec<std::ffi::OsString> {
    vec![
        dir.as_os_str().to_owned(),
        "/grant".into(),
        "*S-1-15-2-2:(OI)(CI)(RX)".into(),
        "/q".into(),
    ]
}

#[cfg(test)]
mod tests;
