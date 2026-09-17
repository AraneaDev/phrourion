//! Forgejo/Gitea-compatible hosting adapter. HTTP-based: no gh-equivalent CLI
//! exists for Forgejo, so this talks to the REST API directly.

pub fn env_token_var(host: &str) -> String {
    let normalized: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PHROURION_TOKEN_{normalized}")
}

#[allow(dead_code)]
pub(crate) fn base_url(host: &str) -> String {
    let root =
        std::env::var("PHROURION_FORGEJO_TEST_URL").unwrap_or_else(|_| format!("https://{host}"));
    format!("{root}/api/v1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_token_var_uppercases_the_host_and_replaces_non_alphanumerics() {
        assert_eq!(
            env_token_var("codeberg.org"),
            "PHROURION_TOKEN_CODEBERG_ORG"
        );
        assert_eq!(
            env_token_var("forge.example.com:3000"),
            "PHROURION_TOKEN_FORGE_EXAMPLE_COM_3000"
        );
    }

    #[test]
    fn base_url_defaults_to_https_and_honors_the_test_override() {
        // SAFETY: single-threaded test process for env var mutation; no other
        // test in this crate reads PHROURION_FORGEJO_TEST_URL.
        unsafe {
            std::env::remove_var("PHROURION_FORGEJO_TEST_URL");
        }
        assert_eq!(base_url("codeberg.org"), "https://codeberg.org/api/v1");
        unsafe {
            std::env::set_var("PHROURION_FORGEJO_TEST_URL", "http://localhost:3000");
        }
        assert_eq!(base_url("codeberg.org"), "http://localhost:3000/api/v1");
        unsafe {
            std::env::remove_var("PHROURION_FORGEJO_TEST_URL");
        }
    }
}
