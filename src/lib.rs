pub mod command;
pub mod forgejo;
pub mod git;
pub mod model;
pub mod provider;
pub mod registry;
pub mod terminal;
pub mod tui;

#[cfg(test)]
pub(crate) mod test_support {
    // Shared by src/forgejo.rs's and src/provider.rs's unit tests: both set
    // the process-global PHROURION_FORGEJO_TEST_URL var, and Rust's test
    // harness runs different #[test]/#[tokio::test] fns concurrently on
    // separate threads by default.
    pub(crate) static FORGEJO_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
}
