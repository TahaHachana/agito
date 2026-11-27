//! Plain tests in every state `agito run` reports.

#[cfg(test)]
mod tests {
    #[test]
    fn passes() {}

    #[test]
    fn fails() {
        assert_eq!(1 + 1, 3, "arithmetic is broken");
    }

    #[test]
    fn prints_then_fails() {
        println!("diagnostic: the widget count was 7");
        panic!("widgets went missing");
    }

    #[test]
    #[ignore = "needs a network"]
    fn ignored() {}

    /// Fails the first time it runs in a state directory, then passes.
    #[test]
    fn flaky() {
        let Some(dir) = std::env::var_os("FIXTURE_STATE") else {
            return;
        };
        let marker = std::path::Path::new(&dir).join("flaky-ran");
        if !marker.exists() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&marker, "").unwrap();
            panic!("first run fails");
        }
    }
}
