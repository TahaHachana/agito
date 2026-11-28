# Video

agito records pages as videos, from the browser's own screencast, with no external tools.

```rust,ignore
page.start_video("target/videos/checkout.avi").await?;
page.goto("https://shop.test/").await?;
page.locator("text=Buy").click().await?;
let path = page.stop_video().await?;
```

To record every page of a context for its whole life, give the context a directory:

```rust,ignore
let context = browser
    .new_context(ContextOptions {
        record_video: Some("target/videos".into()),
        ..ContextOptions::default()
    })
    .await?;
```

Each page then writes `<target id>.avi` there, which `page.video_path()` names. A video is
finished when its page closes, and `context.close()` returns once every one is written.

## The files

Videos are MJPEG in AVI, which most players and editors open; convert one with
`ffmpeg -i video.avi video.mp4`. They are recorded at the page's viewport size.

- **Real time.** The browser sends a frame only when the screen changes, so a page that sits
  still for five seconds sends nothing for five seconds. Each frame keeps the time it was
  captured, and still moments are written as repeats of the frame before, so the video plays
  as long as the test took, and a still page costs almost nothing. Players show 25 frames a
  second.
- **No pointer** is drawn: the screencast shows what the page painted, and a mouse is not
  part of the page.
