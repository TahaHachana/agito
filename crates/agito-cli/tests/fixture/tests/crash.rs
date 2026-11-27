//! A binary that dies mid-run when FIXTURE_CRASH is set.

#[test]
fn survives() {}

#[test]
fn aborts() {
    if std::env::var_os("FIXTURE_CRASH").is_some() {
        std::process::abort();
    }
}
