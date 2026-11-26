//! Launching a browser, or connecting to one that is already running.

pub mod args;
pub mod endpoint;
pub mod finder;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::browser::{Browser, BrowserProcess};
use crate::config;
use crate::error::{Error, ErrorKind, Result};
use crate::plugin::Plugin;
use crate::transport::{CdpConnection, ConnectionOptions, ws};
pub use finder::BrowserChannel;

/// How to launch a browser. [`Default`] starts from built-in defaults with `agito.config.json`
/// and `AGITO_*` variables applied, so code only names what it wants to change.
#[derive(Clone)]
pub struct LaunchOptions {
    pub headless: bool,
    pub channel: Option<BrowserChannel>,
    pub executable_path: Option<PathBuf>,
    /// Extra command-line flags.
    pub args: Vec<String>,
    /// Default flags to leave out.
    pub ignore_default_args: Vec<String>,
    /// Delay before every protocol command.
    pub slow_mo: Duration,
    /// How long launching may take.
    pub timeout: Duration,
    /// A profile directory to use; a temporary one is created (and removed) otherwise.
    pub user_data_dir: Option<PathBuf>,
    pub downloads_path: Option<PathBuf>,
    /// Plugins to load into every context, in addition to `#[agito::plugin]` registrations.
    pub plugins: Vec<Arc<dyn Plugin>>,
}

impl std::fmt::Debug for LaunchOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchOptions")
            .field("headless", &self.headless)
            .field("channel", &self.channel)
            .field("executable_path", &self.executable_path)
            .field("args", &self.args)
            .field("slow_mo", &self.slow_mo)
            .field("timeout", &self.timeout)
            .field("plugins", &self.plugins.len())
            .finish_non_exhaustive()
    }
}

impl Default for LaunchOptions {
    fn default() -> Self {
        let c = &config::get().launch;
        Self {
            headless: c.headless.unwrap_or(true),
            channel: c.channel.as_deref().and_then(|s| s.parse().ok()),
            executable_path: c.executable_path.as_ref().map(PathBuf::from),
            args: Vec::new(),
            ignore_default_args: Vec::new(),
            slow_mo: Duration::from_millis(c.slow_mo.unwrap_or(0)),
            timeout: Duration::from_millis(c.timeout.unwrap_or(30_000)),
            user_data_dir: None,
            downloads_path: None,
            plugins: Vec::new(),
        }
    }
}

/// Options for [`connect`].
#[derive(Clone)]
pub struct ConnectOptions {
    pub slow_mo: Duration,
    pub timeout: Duration,
    pub plugins: Vec<Arc<dyn Plugin>>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            slow_mo: Duration::ZERO,
            timeout: Duration::from_secs(30),
            plugins: Vec::new(),
        }
    }
}

// Runs the browser with its command pipe on fd 3 and its reply pipe on fd 4, which is what
// --remote-debugging-pipe expects, using the shell so no unsafe fd juggling is needed here.
#[cfg(unix)]
const PIPE_LAUNCH_SCRIPT: &str = r#"exec "$0" "$@" 3<&0 4>&1 1>/dev/null"#;

/// Launches a browser.
pub async fn launch(options: LaunchOptions) -> Result<Browser> {
    let executable = finder::resolve(options.channel, options.executable_path.as_deref())?;
    let is_firefox = options.channel == Some(BrowserChannel::Firefox)
        || executable
            .file_name()
            .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("firefox"));
    if is_firefox {
        return Err(Error::not_supported(
            "Firefox (WebDriver BiDi) is not supported yet; launch a Chromium-based browser.",
        ));
    }

    static SWEEP: std::sync::Once = std::sync::Once::new();
    SWEEP.call_once(sweep_stale_profiles);

    let (user_data_dir, owned_dir) = match &options.user_data_dir {
        Some(dir) => (dir.clone(), None),
        None => {
            let dir = temp_profile_dir();
            std::fs::create_dir_all(&dir)
                .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
            (dir.clone(), Some(dir))
        }
    };

    let result = launch_chromium(&options, &executable, &user_data_dir, owned_dir.clone()).await;
    if result.is_err()
        && let Some(dir) = owned_dir
    {
        let _ = std::fs::remove_dir_all(dir);
    }
    result
}

async fn launch_chromium(
    options: &LaunchOptions,
    executable: &std::path::Path,
    user_data_dir: &std::path::Path,
    owned_dir: Option<PathBuf>,
) -> Result<Browser> {
    let use_pipe = cfg!(unix);
    let port = if use_pipe { None } else { Some(free_port()?) };
    let args = args::chromium_args(options, user_data_dir, port);

    let mut command;
    #[cfg(unix)]
    {
        command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(PIPE_LAUNCH_SCRIPT)
            .arg(executable)
            .args(&args);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
    }
    #[cfg(not(unix))]
    {
        command = tokio::process::Command::new(executable);
        command.args(&args);
        command.stdin(Stdio::null()).stdout(Stdio::piped());
    }
    command.stderr(Stdio::piped()).kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|e| Error::io(format!("starting {}", executable.display()), e))?;
    let output = OutputTail::default();
    if let Some(stderr) = child.stderr.take() {
        output.drain(stderr);
    }
    if !use_pipe && let Some(stdout) = child.stdout.take() {
        output.drain(stdout);
    }

    let timeout = options.timeout;
    let connect = async {
        let pair = if use_pipe {
            let stdin = child.stdin.take().expect("piped stdin");
            let stdout = child.stdout.take().expect("piped stdout");
            crate::transport::pipe::pipe_pair(stdin, stdout)
        } else {
            let url =
                endpoint::wait_for_ws_endpoint("127.0.0.1", port.expect("port mode"), timeout)
                    .await?;
            ws::connect(&url).await?
        };
        Ok::<_, Error>(CdpConnection::start(
            pair,
            ConnectionOptions {
                slow_mo: options.slow_mo,
                ..Default::default()
            },
        ))
    };
    let connection = match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(with_output(e, &output)),
        Err(_) => return Err(launch_timeout(timeout, &output)),
    };

    let process = BrowserProcess {
        child: Some(child),
        owned_dir,
        output: output.clone(),
    };
    match tokio::time::timeout(
        timeout,
        Browser::initialize(connection, Some(process), options.plugins.clone(), true),
    )
    .await
    {
        Ok(Ok(browser)) => Ok(browser),
        Ok(Err(e)) => Err(with_output(e, &output)),
        Err(_) => Err(launch_timeout(timeout, &output)),
    }
}

/// Connects to a running browser at a `ws://` endpoint or an `http://host:port` debugging
/// address. The browser is not closed when the returned handle closes; use
/// [`Browser::disconnect`] to leave it running.
pub async fn connect(endpoint: &str, options: ConnectOptions) -> Result<Browser> {
    let url = endpoint::resolve_endpoint(endpoint, options.timeout).await?;
    let pair = tokio::time::timeout(options.timeout, ws::connect(&url))
        .await
        .map_err(|_| {
            Error::other(format!(
                "Did not finish connecting to {endpoint} within {:?}.",
                options.timeout
            ))
        })??;
    let connection = CdpConnection::start(
        pair,
        ConnectionOptions {
            slow_mo: options.slow_mo,
            ..Default::default()
        },
    );
    tokio::time::timeout(
        options.timeout,
        Browser::initialize(connection, None, options.plugins, false),
    )
    .await
    .map_err(|_| {
        Error::other(format!(
            "Did not finish connecting to {endpoint} within {:?}.",
            options.timeout
        ))
    })?
}

fn launch_timeout(timeout: Duration, output: &OutputTail) -> Error {
    ErrorKind::WaitTimeout {
        condition: "browser launch".into(),
        timeout,
        last_evaluated_value: None,
        message: format!(
            "Browser started but did not answer within {}s.{}",
            timeout.as_secs_f64(),
            output.describe()
        ),
    }
    .into()
}

fn with_output(error: Error, output: &OutputTail) -> Error {
    let tail = output.describe();
    if tail.is_empty() {
        return error;
    }
    Error::other(format!("{error}{tail}"))
}

fn temp_profile_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "{PROFILE_PREFIX}{}-{}-{nanos:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

const PROFILE_PREFIX: &str = "agito-profile-";

/// Removes temporary profiles left by agito processes that are no longer running. A browser
/// shared by a test binary lives in a static, which is never dropped, so its profile outlives
/// the process; the next launch cleans it up.
fn sweep_stale_profiles() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(PROFILE_PREFIX))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != me && process_gone(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_gone(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| !s.success())
}

#[cfg(not(unix))]
fn process_gone(_pid: u32) -> bool {
    // Without a cheap liveness check, leave the directories alone.
    false
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| Error::io("allocating a port", e))?;
    Ok(listener
        .local_addr()
        .map_err(|e| Error::io("allocating a port", e))?
        .port())
}

/// The last lines a browser wrote to stdout/stderr, for error messages when it fails to start.
#[derive(Clone, Default)]
pub struct OutputTail {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl OutputTail {
    const KEEP: usize = 20;

    fn drain(&self, stream: impl AsyncRead + Unpin + Send + 'static) {
        let lines = self.lines.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stream).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut lines = lines.lock().unwrap();
                if lines.len() == Self::KEEP {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        });
    }

    /// " The browser said: ..." or empty when it said nothing.
    pub fn describe(&self) -> String {
        let lines = self.lines.lock().unwrap();
        if lines.is_empty() {
            return String::new();
        }
        format!(
            " The browser said:\n{}",
            lines.iter().cloned().collect::<Vec<_>>().join("\n")
        )
    }
}
