//! Files in and out of the page: uploads (`set_input_files`, file choosers) and downloads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agito_protocol::cdp::{browser as cdp_browser, dom};
use tokio::sync::watch;

use crate::error::{Error, ErrorKind, Result};
use crate::handles::ElementHandle;
use crate::page::{Page, PageInner};
use crate::transport::Session;

/// A file to upload, held in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePayload {
    /// The file name the page sees.
    pub name: String,
    /// The MIME type the page sees; guessed from the browser's own rules from the name.
    pub mime_type: Option<String>,
    /// The file's bytes.
    pub contents: Vec<u8>,
}

impl FilePayload {
    /// A file called `name` holding `contents`.
    pub fn new(name: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            mime_type: None,
            contents: contents.into(),
        }
    }
}

/// What to put in a file input: files on disk, or files held in memory.
#[derive(Debug, Clone)]
pub enum InputFiles {
    /// Files on disk.
    Paths(Vec<PathBuf>),
    /// Files made in memory.
    Payloads(Vec<FilePayload>),
}

impl InputFiles {
    /// No files: clears the input.
    pub fn none() -> Self {
        InputFiles::Paths(Vec::new())
    }
}

impl<P: AsRef<Path>> From<&[P]> for InputFiles {
    fn from(paths: &[P]) -> Self {
        InputFiles::Paths(paths.iter().map(|p| p.as_ref().to_path_buf()).collect())
    }
}

impl<P: AsRef<Path>, const N: usize> From<[P; N]> for InputFiles {
    fn from(paths: [P; N]) -> Self {
        InputFiles::Paths(paths.iter().map(|p| p.as_ref().to_path_buf()).collect())
    }
}

impl From<PathBuf> for InputFiles {
    fn from(path: PathBuf) -> Self {
        InputFiles::Paths(vec![path])
    }
}

impl From<&Path> for InputFiles {
    fn from(path: &Path) -> Self {
        InputFiles::Paths(vec![path.to_path_buf()])
    }
}

impl From<&str> for InputFiles {
    fn from(path: &str) -> Self {
        InputFiles::Paths(vec![PathBuf::from(path)])
    }
}

impl From<FilePayload> for InputFiles {
    fn from(file: FilePayload) -> Self {
        InputFiles::Payloads(vec![file])
    }
}

impl From<Vec<FilePayload>> for InputFiles {
    fn from(files: Vec<FilePayload>) -> Self {
        InputFiles::Payloads(files)
    }
}

/// A fresh, unique directory under the system temp dir.
pub(crate) fn temp_dir(kind: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "agito-{kind}-{}-{}-{nanos:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
    Ok(dir)
}

/// Turns the files into absolute paths the browser can read. In-memory files are written to
/// `upload_dir`, which must outlive the upload: the page reads file contents lazily.
pub(crate) fn materialize(
    files: InputFiles,
    upload_dir: impl FnOnce() -> Result<PathBuf>,
) -> Result<Vec<String>> {
    match files {
        InputFiles::Paths(paths) => paths
            .into_iter()
            .map(|p| {
                let absolute = std::path::absolute(&p)
                    .map_err(|e| Error::io(format!("resolving {}", p.display()), e))?;
                if !absolute.is_file() {
                    return Err(Error::invalid_argument(format!(
                        "No such file to upload: {}",
                        absolute.display()
                    )));
                }
                Ok(absolute.to_string_lossy().into_owned())
            })
            .collect(),
        InputFiles::Payloads(payloads) if payloads.is_empty() => Ok(Vec::new()),
        InputFiles::Payloads(payloads) => {
            let base = upload_dir()?;
            let mut paths = Vec::with_capacity(payloads.len());
            for (i, file) in payloads.into_iter().enumerate() {
                if file.name.is_empty() || file.name.contains(['/', '\\']) {
                    return Err(Error::invalid_argument(format!(
                        "'{}' is not a plain file name",
                        file.name
                    )));
                }
                // One directory per file, so two files may share a name.
                let dir = base.join(i.to_string());
                std::fs::create_dir_all(&dir)
                    .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
                let path = dir.join(&file.name);
                std::fs::write(&path, &file.contents)
                    .map_err(|e| Error::io(format!("writing {}", path.display()), e))?;
                paths.push(path.to_string_lossy().into_owned());
            }
            Ok(paths)
        }
    }
}

/// Puts `paths` into the file input `backend_node_id` or `object_id` names.
pub(crate) async fn set_input_files(
    session: &Arc<dyn Session>,
    target: FileInputTarget,
    paths: Vec<String>,
) -> Result<()> {
    if paths.is_empty() {
        // Chrome ignores an empty file list, so clear the input from script instead.
        let object_id = match target {
            FileInputTarget::Object(id) => id,
            FileInputTarget::Node(id) => session
                .send(&dom::ResolveNodeParams {
                    backend_node_id: Some(id),
                    ..Default::default()
                })
                .await?
                .object
                .object_id
                .ok_or_else(|| Error::other("the file input is no longer in the page"))?,
        };
        crate::handles::call_function(
            session,
            &object_id,
            "function() { if (this.files.length === 0) return; this.value = ''; \
             this.dispatchEvent(new Event('input', { bubbles: true })); \
             this.dispatchEvent(new Event('change', { bubbles: true })); }",
            &[],
            true,
            false,
        )
        .await?;
        return Ok(());
    }
    let (backend_node_id, object_id) = match target {
        FileInputTarget::Node(id) => (Some(id), None),
        FileInputTarget::Object(id) => (None, Some(id)),
    };
    session
        .send(&dom::SetFileInputFilesParams {
            files: paths,
            node_id: None,
            backend_node_id,
            object_id,
        })
        .await?;
    Ok(())
}

pub(crate) enum FileInputTarget {
    Node(i64),
    Object(String),
}

/// A file picker the page opened (an `<input type=file>` was clicked). Only reported while
/// something listens for file choosers on the page.
#[derive(Clone)]
pub struct FileChooser {
    page: Page,
    session: Arc<dyn Session>,
    backend_node_id: i64,
    multiple: bool,
}

impl std::fmt::Debug for FileChooser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileChooser")
            .field("multiple", &self.multiple)
            .finish()
    }
}

impl FileChooser {
    pub(crate) fn new(
        page: Page,
        session: Arc<dyn Session>,
        backend_node_id: i64,
        multiple: bool,
    ) -> Self {
        Self {
            page,
            session,
            backend_node_id,
            multiple,
        }
    }

    /// The page the chooser belongs to.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// Whether the input accepts several files.
    pub fn is_multiple(&self) -> bool {
        self.multiple
    }

    /// The `<input type=file>` that opened the chooser.
    pub async fn element(&self) -> Result<ElementHandle> {
        let resolved = self
            .session
            .send(&dom::ResolveNodeParams {
                backend_node_id: Some(self.backend_node_id),
                ..Default::default()
            })
            .await?;
        let id = resolved
            .object
            .object_id
            .ok_or_else(|| Error::other("the file input is no longer in the page"))?;
        Ok(ElementHandle::new(self.session.clone(), id))
    }

    /// Answers the chooser with these files.
    pub async fn set_files(&self, files: impl Into<InputFiles>) -> Result<()> {
        let files = files.into();
        let count = match &files {
            InputFiles::Paths(p) => p.len(),
            InputFiles::Payloads(p) => p.len(),
        };
        if count > 1 && !self.multiple {
            return Err(Error::invalid_argument(
                "This file input accepts a single file.",
            ));
        }
        let paths = materialize(files, || self.page.upload_dir())?;
        set_input_files(
            &self.session,
            FileInputTarget::Node(self.backend_node_id),
            paths,
        )
        .await
    }
}

#[derive(Debug, Clone, PartialEq)]
enum DownloadState {
    InProgress,
    Completed,
    Canceled,
}

pub(crate) struct DownloadInner {
    guid: String,
    url: String,
    suggested_filename: String,
    path: PathBuf,
    context_id: String,
    page: Weak<PageInner>,
    state: watch::Sender<DownloadState>,
    browser_session: Arc<dyn Session>,
    received: Mutex<(f64, f64)>,
}

/// A file the page downloaded (or is downloading). Cheap to clone.
#[derive(Clone)]
pub struct Download {
    pub(crate) inner: Arc<DownloadInner>,
}

impl std::fmt::Debug for Download {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Download({} from {})",
            self.inner.suggested_filename, self.inner.url
        )
    }
}

/// How long `path`/`save_as` wait for a download by default.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

impl Download {
    pub(crate) fn new(
        event: &cdp_browser::DownloadWillBeginEvent,
        dir: &Path,
        context_id: String,
        page: Weak<PageInner>,
        browser_session: Arc<dyn Session>,
    ) -> Self {
        Self {
            inner: Arc::new(DownloadInner {
                guid: event.guid.clone(),
                url: event.url.clone(),
                suggested_filename: event.suggested_filename.clone(),
                path: dir.join(&event.guid),
                context_id,
                page,
                state: watch::channel(DownloadState::InProgress).0,
                browser_session,
                received: Mutex::new((0.0, 0.0)),
            }),
        }
    }

    pub(crate) fn guid(&self) -> &str {
        &self.inner.guid
    }

    pub(crate) fn on_progress(&self, event: &cdp_browser::DownloadProgressEvent) {
        *self.inner.received.lock().unwrap() = (event.received_bytes, event.total_bytes);
        let state = match serde_json::to_value(event.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .as_deref()
        {
            Some("completed") => DownloadState::Completed,
            Some("canceled") => DownloadState::Canceled,
            _ => return,
        };
        self.inner.state.send_replace(state);
    }

    /// The URL the file came from.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// The name the server or page suggested for the file.
    pub fn suggested_filename(&self) -> &str {
        &self.inner.suggested_filename
    }

    /// The page that started the download, while it is open.
    pub fn page(&self) -> Option<Page> {
        self.inner.page.upgrade().map(Page::from_inner)
    }

    /// Bytes received so far, and the total if the server said (0 otherwise).
    pub fn progress(&self) -> (u64, u64) {
        let (received, total) = *self.inner.received.lock().unwrap();
        (received as u64, total as u64)
    }

    async fn finished(&self, timeout: Duration) -> Result<()> {
        let mut rx = self.inner.state.subscribe();
        let wait = async {
            loop {
                match &*rx.borrow_and_update() {
                    DownloadState::Completed => return Ok(()),
                    DownloadState::Canceled => {
                        return Err(Error::other(format!(
                            "The download of {} was canceled.",
                            self.inner.url
                        )));
                    }
                    DownloadState::InProgress => {}
                }
                if rx.changed().await.is_err() {
                    return Err(Error::target_closed(
                        "browser",
                        None,
                        "The browser went away during the download.",
                    ));
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or_else(|_| {
                Err(ErrorKind::WaitTimeout {
                    condition: "download".into(),
                    timeout,
                    last_evaluated_value: None,
                    message: format!(
                        "The download of {} did not finish within {}s.",
                        self.inner.url,
                        timeout.as_secs()
                    ),
                }
                .into())
            })
    }

    /// Waits for the download to finish and returns where it was saved: a temporary file,
    /// removed when the context closes. Use [`Download::save_as`] to keep it.
    pub async fn path(&self) -> Result<PathBuf> {
        self.finished(DOWNLOAD_TIMEOUT).await?;
        Ok(self.inner.path.clone())
    }

    /// Waits for the download to finish and copies it to `path`.
    pub async fn save_as(&self, path: impl AsRef<Path>) -> Result<()> {
        let from = self.path().await?;
        let to = path.as_ref();
        if let Some(parent) = to.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("creating {}", parent.display()), e))?;
        }
        std::fs::copy(&from, to)
            .map_err(|e| Error::io(format!("copying the download to {}", to.display()), e))?;
        Ok(())
    }

    /// Waits for the download to finish; `None` if it completed, or why it did not.
    pub async fn failure(&self) -> Option<String> {
        self.finished(DOWNLOAD_TIMEOUT)
            .await
            .err()
            .map(|e| e.to_string())
    }

    /// Cancels the download if it is still running.
    pub async fn cancel(&self) -> Result<()> {
        if *self.inner.state.borrow() != DownloadState::InProgress {
            return Ok(());
        }
        self.inner
            .browser_session
            .send(&cdp_browser::CancelDownloadParams {
                guid: self.inner.guid.clone(),
                browser_context_id: Some(self.inner.context_id.clone()),
            })
            .await?;
        Ok(())
    }

    /// Deletes the downloaded file.
    pub async fn delete(&self) -> Result<()> {
        let path = self.path().await?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::io(format!("deleting {}", path.display()), e))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_written_with_their_names() {
        let base = temp_dir("test-upload").unwrap();
        let paths = materialize(
            vec![
                FilePayload::new("a.txt", "one"),
                FilePayload::new("a.txt", "two"),
            ]
            .into(),
            || Ok(base.clone()),
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|p| p.ends_with("a.txt")));
        assert_eq!(std::fs::read_to_string(&paths[1]).unwrap(), "two");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn bad_names_and_missing_files_are_refused() {
        let err = materialize(FilePayload::new("../x", "").into(), || {
            Ok(std::env::temp_dir())
        })
        .unwrap_err();
        assert!(err.to_string().contains("not a plain file name"));
        let err = materialize("/no/such/file.txt".into(), || unreachable!()).unwrap_err();
        assert!(err.to_string().contains("No such file"));
        assert!(
            materialize(InputFiles::none(), || unreachable!())
                .unwrap()
                .is_empty()
        );
    }
}
