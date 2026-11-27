//! Uploads and downloads. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agito::{BrowserContext, ErrorKind, FilePayload, InputFiles, Page, expect};
use common::serve;

const UPLOAD: &str = r#"
<input type="file" id="one">
<input type="file" id="many" multiple>
<input type="file" id="hidden" style="display:none">
<input type="text" id="text">
<button id="pick" onclick="document.getElementById('many').click()">Choose files</button>
<p id="out"></p>
<script>
  for (const input of document.querySelectorAll('input[type=file]')) {
    input.addEventListener('change', async () => {
      const parts = [];
      for (const f of input.files) parts.push(f.name + '=' + (await f.text()));
      document.getElementById('out').textContent = input.id + ':' + parts.join(',');
    });
  }
</script>"#;

fn scratch_file(name: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("agito-files-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

#[agito::test]
async fn uploads_files_from_disk(page: Page) -> agito::Result<()> {
    page.set_content(UPLOAD).await?;
    let path = scratch_file("notes.txt", "hello from disk");
    page.locator("#one").set_input_files([&path]).await?;
    expect(&page.locator("#out"))
        .to_have_text("one:notes.txt=hello from disk")
        .await?;
    std::fs::remove_file(path).unwrap();
    Ok(())
}

#[agito::test]
async fn uploads_files_from_memory_into_hidden_inputs(page: Page) -> agito::Result<()> {
    page.set_content(UPLOAD).await?;
    page.locator("#many")
        .set_input_files(vec![
            FilePayload::new("a.txt", "first"),
            FilePayload::new("a.txt", "second"),
        ])
        .await?;
    expect(&page.locator("#out"))
        .to_have_text("many:a.txt=first,a.txt=second")
        .await?;
    page.locator("#hidden")
        .set_input_files(FilePayload::new("h.csv", "x,y"))
        .await?;
    expect(&page.locator("#out"))
        .to_have_text("hidden:h.csv=x,y")
        .await?;
    page.locator("#many")
        .set_input_files(InputFiles::none())
        .await?;
    let count: u32 = page
        .evaluate("document.getElementById('many').files.length")
        .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[agito::test]
async fn upload_mistakes_are_reported(page: Page) -> agito::Result<()> {
    page.set_content(UPLOAD).await?;
    let err = page
        .locator("#text")
        .set_input_files(FilePayload::new("a", ""))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("is not an <input type=file>"),
        "{err}"
    );
    let err = page
        .locator("#one")
        .set_input_files(vec![FilePayload::new("a", ""), FilePayload::new("b", "")])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("accepts a single file"), "{err}");
    let err = page
        .locator("#one")
        .set_input_files("/no/such/file")
        .await
        .unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::InvalidArgument(_)),
        "{err:?}"
    );
    Ok(())
}

#[agito::test]
async fn file_choosers_can_be_answered(page: Page) -> agito::Result<()> {
    page.set_content(UPLOAD).await?;
    let chooser = page.wait_for_file_chooser(None);
    page.locator("#pick").click().await?;
    let chooser = chooser.await?;
    assert!(chooser.is_multiple());
    let id: String = chooser
        .element()
        .await?
        .evaluate("function() { return this.id; }", &[])
        .await?;
    assert_eq!(id, "many");
    chooser
        .set_files(vec![FilePayload::new("c.txt", "chosen")])
        .await?;
    expect(&page.locator("#out"))
        .to_have_text("many:c.txt=chosen")
        .await?;
    Ok(())
}

#[agito::test]
async fn file_chooser_handlers_answer_automatically(page: Page) -> agito::Result<()> {
    page.set_content(UPLOAD).await?;
    page.on_file_chooser(|chooser| async move {
        let _ = chooser
            .set_files(FilePayload::new("auto.txt", "handled"))
            .await;
    });
    page.locator("#pick").click().await?;
    expect(&page.locator("#out"))
        .to_have_text("many:auto.txt=handled")
        .await?;
    Ok(())
}

const DOWNLOADS: &str = r#"<a id="csv" href="/report">Export CSV</a>
<a id="data" download="hello.txt" href="data:text/plain,hi%20there">data link</a>"#;

async fn download_site() -> String {
    serve(&[
        ("/", 200, DOWNLOADS),
        ("/report", 200, "ATTACHMENT:report.csv:a,b\n1,2\n"),
    ])
    .await
}

#[agito::test]
async fn downloads_can_be_saved(page: Page) -> agito::Result<()> {
    let base = download_site().await;
    page.goto(&base).await?;
    let download = page.wait_for_download(None);
    page.locator("#csv").click().await?;
    let download = download.await?;
    assert_eq!(download.suggested_filename(), "report.csv");
    assert_eq!(download.url(), format!("{base}/report"));
    assert!(download.failure().await.is_none());
    let path = download.path().await?;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a,b\n1,2\n");

    let target =
        std::env::temp_dir().join(format!("agito-saved-{}/report.csv", std::process::id()));
    download.save_as(&target).await?;
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "a,b\n1,2\n");
    std::fs::remove_dir_all(target.parent().unwrap()).unwrap();
    download.delete().await?;
    assert!(!path.exists());
    Ok(())
}

#[agito::test]
async fn download_handlers_see_every_download(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    page.goto(&download_site().await).await?;
    let names = Arc::new(Mutex::new(Vec::new()));
    let sink = names.clone();
    page.on_download(move |d| sink.lock().unwrap().push(d.suggested_filename().to_owned()));
    let download = page.wait_for_download(None);
    page.locator("#data").click().await?;
    let download = download.await?;
    assert_eq!(
        std::fs::read_to_string(download.path().await?).unwrap(),
        "hi there"
    );
    assert_eq!(names.lock().unwrap().clone(), ["hello.txt"]);
    assert_eq!(
        download.page().map(|p| p.target_id().to_owned()),
        Some(page.target_id().to_owned())
    );

    // Downloads live in the context's own directory, removed when it closes.
    let dir = download.path().await?.parent().unwrap().to_path_buf();
    assert!(dir.exists());
    context.close().await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while dir.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!dir.exists());
    Ok(())
}
