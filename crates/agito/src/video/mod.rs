//! Recording a page as a video, from the browser's screencast.
//!
//! Videos are MJPEG in AVI, written without external tools, which most players and editors
//! open; `ffmpeg -i in.avi out.mp4` converts one. They play in real time at 25 frames per
//! second whatever the page did: the browser sends a frame only when the screen changes, and
//! the last one is held until the next. No pointer is drawn: the screencast shows what the
//! page painted.

mod avi;

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agito_protocol::cdp::page as cdp_page;
use base64::Engine;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};
use crate::page::Page;

pub use avi::FPS;

/// How long closing a page or context waits for its videos to be written.
const FINISH_TIMEOUT: Duration = Duration::from_secs(10);

/// A recording in progress on a page.
pub(crate) struct Recording {
    path: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    done: Option<JoinHandle<Result<()>>>,
}

impl Page {
    /// Starts recording the page to `path`, an `.avi` file, until
    /// [`stop_video`](Self::stop_video) or the page closes.
    ///
    /// ```no_run
    /// # async fn example(page: agito::Page) -> agito::Result<()> {
    /// page.start_video("target/videos/checkout.avi").await?;
    /// page.goto("https://shop.test/").await?;
    /// page.stop_video().await?;
    /// # Ok(()) }
    /// ```
    pub async fn start_video(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref().to_path_buf();
        if self.inner.video.lock().unwrap().is_some() {
            return Err(Error::invalid_argument(
                "a video is already being recorded on this page",
            ));
        }
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
        }
        let file = std::fs::File::create(&path)
            .map_err(|e| Error::io(format!("creating {}", path.display()), e))?;

        let session = self.main_frame().session();
        // Subscribe before starting, so no frame is missed.
        let frames = session.subscribe(&["Page.screencastFrame"]);
        let viewport = self.inner.context.options().viewport;
        session
            .send(&cdp_page::StartScreencastParams {
                format: Some(cdp_page::StartScreencastParamsFormat::Jpeg),
                quality: Some(80),
                max_width: viewport.map(|v| i64::from(v.width)),
                max_height: viewport.map(|v| i64::from(v.height)),
                ..Default::default()
            })
            .await?;
        let (stop, stopped) = oneshot::channel();
        let done = tokio::spawn(record(session, frames, stopped, file, path.clone()));
        *self.inner.video.lock().unwrap() = Some(Recording {
            path,
            stop: Some(stop),
            done: Some(done),
        });
        Ok(())
    }

    /// Stops recording, finishes the file, and returns its path.
    pub async fn stop_video(&self) -> Result<PathBuf> {
        let recording =
            self.inner.video.lock().unwrap().take().ok_or_else(|| {
                Error::invalid_argument("no video is being recorded on this page")
            })?;
        finish(recording).await
    }

    /// Where the page's video is being written, if it is being recorded: by
    /// [`start_video`](Self::start_video), or for every page of a context with
    /// [`ContextOptions::record_video`](crate::ContextOptions::record_video).
    pub fn video_path(&self) -> Option<PathBuf> {
        self.inner
            .video
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.path.clone())
    }

    /// Finishes the page's video, if one is being recorded, as the page goes away.
    pub(crate) async fn finish_video(&self) {
        let recording = self.inner.video.lock().unwrap().take();
        if let Some(recording) = recording {
            let _ = finish(recording).await;
        }
    }
}

async fn finish(mut recording: Recording) -> Result<PathBuf> {
    if let Some(stop) = recording.stop.take() {
        let _ = stop.send(());
    }
    if let Some(done) = recording.done.take() {
        match tokio::time::timeout(FINISH_TIMEOUT, done).await {
            Ok(Ok(written)) => written?,
            Ok(Err(e)) => return Err(Error::other(format!("the video recorder stopped: {e}"))),
            Err(_) => {
                return Err(Error::other(format!(
                    "the video {} was not finished within {FINISH_TIMEOUT:?}",
                    recording.path.display()
                )));
            }
        }
    }
    Ok(recording.path)
}

/// Writes frames until told to stop or the page goes away, then finishes the file.
async fn record(
    session: std::sync::Arc<dyn crate::transport::Session>,
    mut frames: crate::transport::EventStream,
    mut stopped: oneshot::Receiver<()>,
    file: std::fs::File,
    path: PathBuf,
) -> Result<()> {
    let mut writer = avi::AviWriter::new(BufWriter::new(file));
    let started = Instant::now();
    // The first frame's capture time, by the browser's clock, and when it arrived, by ours:
    // frames are placed by their capture times, and the end by ours.
    let mut first: Option<(f64, Instant)> = None;
    let io_error = |e: std::io::Error| Error::io(format!("writing {}", path.display()), e);
    let stopping = loop {
        let event = tokio::select! {
            _ = &mut stopped => break true,
            event = frames.next() => event,
        };
        // The page went away.
        let Some(event) = event else { break false };
        let Ok(frame) = serde_json::from_value::<cdp_page::ScreencastFrameEvent>(event.params)
        else {
            continue;
        };
        // Unacknowledged, the browser stops sending.
        let ack = session.clone();
        tokio::spawn(async move {
            let _ = ack
                .send(&cdp_page::ScreencastFrameAckParams {
                    session_id: frame.session_id,
                })
                .await;
        });
        let Ok(jpeg) = base64::engine::general_purpose::STANDARD.decode(&frame.data) else {
            continue;
        };
        let captured = frame.metadata.timestamp;
        let at = match (first, captured) {
            (Some((t0, _)), Some(t)) => t - t0,
            (Some((_, arrived)), None) => arrived.elapsed().as_secs_f64(),
            (None, t) => {
                first = Some((t.unwrap_or(0.0), Instant::now()));
                0.0
            }
        };
        writer.add(&jpeg, at).map_err(io_error)?;
    };
    if stopping {
        let _ = session
            .send(&cdp_page::StopScreencastParams::default())
            .await;
    }
    let end = first.map_or(started.elapsed(), |(_, arrived)| arrived.elapsed());
    writer.finish(end.as_secs_f64()).map_err(io_error)?;
    Ok(())
}
