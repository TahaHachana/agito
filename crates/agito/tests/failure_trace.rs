//! `failure.trace`: a failed `#[agito::test]` keeps its trace. In a binary of its own, since
//! the setting is read once per process. Run with `--features integration`.
#![cfg(feature = "integration")]

use agito::harness::{Fixtures, TestOptions};

#[test]
fn a_failed_test_keeps_its_trace_and_a_passing_one_does_not() {
    let dir = std::env::temp_dir().join(format!("agito-failure-traces-{}", std::process::id()));
    // SAFETY: set before anything in this process reads the configuration or starts threads
    // that read the environment.
    unsafe {
        std::env::set_var("AGITO_FAILURES_TRACE", "true");
        std::env::set_var("AGITO_FAILURES_TRACE_PATH", &dir);
    }
    let run = |name: &'static str, fail: bool| {
        std::panic::catch_unwind(|| {
            agito::harness::run(
                name,
                TestOptions::default(),
                move |fixtures: Fixtures| async move {
                    let page = &fixtures.page;
                    page.set_content("<button>Pay</button>")
                        .await
                        .map_err(|e| e.to_string())?;
                    page.locator("button")
                        .click()
                        .await
                        .map_err(|e| e.to_string())?;
                    if fail {
                        return Err("payment declined".to_owned());
                    }
                    Ok::<(), String>(())
                },
            )
        })
    };
    assert!(run("traces::passes", false).is_ok());
    assert!(run("traces::fails", true).is_err());

    let saved: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(saved, ["traces__fails.trace.zip"]);
    let zip = std::fs::File::open(dir.join("traces__fails.trace.zip")).unwrap();
    let mut archive = zip::ZipArchive::new(zip).unwrap();
    let log: serde_json::Value =
        serde_json::from_reader(archive.by_name("trace.json").unwrap()).unwrap();
    assert_eq!(log["title"], "traces::fails");
    assert!(
        log["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "click"),
        "{log}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
