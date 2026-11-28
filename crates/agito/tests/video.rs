//! Recording pages as video. Run with `--features integration`.
#![cfg(feature = "integration")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use agito::{BrowserContext, ContextOptions, Page};

/// A box that moves across the page for a second, then stops.
const ANIMATED: &str = "<body style='margin:0'><div id=box style='width:80px;height:80px;\
    background:#c00;position:absolute;left:0;top:40px'></div><script>\
    const box = document.getElementById('box'); const start = performance.now();\
    function step(now) { const t = now - start; box.style.left = Math.min(t / 2, 500) + 'px';\
    if (t < 1000) requestAnimationFrame(step); } requestAnimationFrame(step);</script></body>";

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("agito-video-{name}-{}", std::process::id()))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// The video's frame count, frame size, and how many distinct JPEGs it stores.
fn inspect(path: &Path) -> (u32, (u32, u32), usize) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"AVI ");
    assert_eq!(
        u32_at(&bytes, 4) as usize,
        bytes.len() - 8,
        "the RIFF size is patched"
    );
    let movi = bytes.windows(4).position(|w| w == b"movi").unwrap();
    let idx = bytes.windows(4).rposition(|w| w == b"idx1").unwrap();
    let frames = u32_at(&bytes, idx + 4) / 16;
    assert_eq!(
        u32_at(&bytes, 0x30),
        frames,
        "dwTotalFrames matches the index"
    );
    // Each slot is a JPEG, or a zero-length chunk repeating the one before.
    let mut pictures = 0;
    for n in 0..frames as usize {
        let offset = u32_at(&bytes, idx + 8 + n * 16 + 8) as usize;
        let size = u32_at(&bytes, idx + 8 + n * 16 + 12);
        assert_eq!(&bytes[movi + offset..movi + offset + 4], b"00dc");
        assert_eq!(u32_at(&bytes, movi + offset + 4), size);
        if size > 0 {
            assert_eq!(
                &bytes[movi + offset + 8..movi + offset + 10],
                [0xFF, 0xD8],
                "a JPEG"
            );
            pictures += 1;
        }
    }
    assert!(pictures > 0, "the first slot is a picture");
    (
        frames,
        (u32_at(&bytes, 0x40), u32_at(&bytes, 0x44)),
        pictures,
    )
}

#[agito::test]
async fn a_recording_plays_in_real_time(page: Page) -> agito::Result<()> {
    let path = scratch("page").join("animated.avi");
    page.start_video(&path).await?;
    assert_eq!(page.video_path(), Some(path.clone()));
    let err = page.start_video(&path).await.unwrap_err();
    assert!(err.to_string().contains("already being recorded"), "{err}");
    page.set_content(ANIMATED).await?;
    // The box moves for a second; the last second is still.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(page.stop_video().await?, path);
    assert_eq!(page.video_path(), None);

    let (frames, size, distinct) = inspect(&path);
    // About two seconds at 25 frames a second, whatever the page painted.
    assert!((40..=60).contains(&frames), "{frames} frames");
    assert_eq!(size, (1280, 720), "the default viewport");
    assert!(
        distinct > 5,
        "the animation painted many frames: {distinct}"
    );
    assert!(
        (distinct as u32) < frames,
        "still moments repeat a stored frame: {distinct} of {frames}"
    );
    std::fs::remove_dir_all(scratch("page")).unwrap();
    Ok(())
}

fn recorded() -> ContextOptions {
    ContextOptions {
        record_video: Some(scratch("context")),
        viewport: Some(agito::Viewport {
            width: 640,
            height: 480,
        }),
        ..ContextOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_context_records_every_page_until_it_closes() -> agito::Result<()> {
    let browser = agito::launch(Default::default()).await?;
    let context: BrowserContext = browser.new_context(recorded()).await?;
    let first = context.new_page().await?;
    first.set_content(ANIMATED).await?;
    let second = context.new_page().await?;
    second.set_content("<h1>Second</h1>").await?;
    let paths = [first.video_path().unwrap(), second.video_path().unwrap()];
    tokio::time::sleep(Duration::from_millis(500)).await;
    let err = first.stop_video().await;
    assert!(err.is_ok(), "a page's own video can be stopped early");
    // Closing the context finishes the rest.
    context.close().await?;
    for path in &paths {
        let (frames, size, _) = inspect(path);
        assert!(frames > 0, "{}", path.display());
        assert_eq!(size, (640, 480), "frames come at the context's viewport");
    }
    std::fs::remove_dir_all(scratch("context")).unwrap();
    browser.close().await
}
