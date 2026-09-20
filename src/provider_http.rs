use anyhow::{Context, Result, bail};
use reqwest::{Client, RequestBuilder, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{collections::HashSet, net::IpAddr, time::Duration};

const MAX_ERROR_BODY_CHARS: usize = 200;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) enum Auth {
    None,
    Bearer(String),
    Token(String),
    Basic { username: String, password: String },
}

pub(crate) struct HttpClient {
    client: Client,
    base_url: Url,
    auth: Auth,
}

impl HttpClient {
    pub(crate) fn new(base_url: &str, auth: Auth) -> Result<Self> {
        let mut base_url = Url::parse(base_url.trim())
            .with_context(|| format!("Invalid provider base URL '{base_url}'"))?;
        if base_url.scheme() == "http"
            && !is_loopback(&base_url)
            && matches!(&auth, Auth::Bearer(_) | Auth::Token(_) | Auth::Basic { .. })
        {
            bail!("Credentialed provider requests require HTTPS unless using a loopback test URL");
        }
        let path = base_url.path().to_string();
        if !path.ends_with('/') {
            base_url.set_path(&format!("{path}/"));
        }

        Ok(Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .context("Failed to build provider HTTP client")?,
            base_url,
            auth,
        })
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.endpoint(path)?;
        let response = self
            .request(url)
            .send()
            .await
            .with_context(|| format!("Provider request failed for endpoint '{path}'"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("Failed to read provider response for endpoint '{path}'"))?;

        if !status.is_success() {
            let context = bounded_body_context(&body);
            bail!("Provider endpoint '{path}' returned HTTP {status}: {context}");
        }

        serde_json::from_str(&body)
            .with_context(|| format!("Invalid JSON from provider endpoint '{path}'"))
    }

    pub(crate) async fn get_json_value(&self, path: &str) -> Result<Value> {
        self.get_json(path).await
    }

    pub(crate) async fn paginate_json<F>(&self, path: &str, mut parse_page: F) -> Result<Vec<Value>>
    where
        F: FnMut(&Value) -> Result<(Vec<Value>, Option<String>)>,
    {
        let mut rows = Vec::new();
        let mut next = Some(path.to_string());
        let mut visited = HashSet::new();
        while let Some(endpoint) = next {
            let normalized = self.endpoint(&endpoint)?.to_string();
            if !visited.insert(normalized) {
                bail!("Provider pagination repeated endpoint '{endpoint}'");
            }
            let page: Value = self.get_json_value(&endpoint).await?;
            let (page_rows, continuation) = parse_page(&page)
                .with_context(|| format!("Invalid provider pagination page at '{endpoint}'"))?;
            rows.extend(page_rows);
            next = continuation;
        }
        Ok(rows)
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            Url::parse(path).with_context(|| format!("Invalid provider endpoint '{path}'"))?
        } else if path.starts_with('/') {
            self.base_url
                .join(path)
                .with_context(|| format!("Invalid provider endpoint '{path}'"))?
        } else {
            self.base_url
                .join(path.trim_start_matches('/'))
                .with_context(|| format!("Invalid provider endpoint '{path}'"))?
        };
        if !same_origin(&self.base_url, &url) {
            bail!(
                "Provider endpoint '{path}' uses a different origin than the configured base URL"
            );
        }
        Ok(url)
    }

    fn request(&self, url: Url) -> RequestBuilder {
        let request = self.client.get(url);
        match &self.auth {
            Auth::None => request,
            Auth::Bearer(token) => request.bearer_auth(token),
            Auth::Token(token) => request.header("Authorization", format!("token {token}")),
            Auth::Basic { username, password } => request.basic_auth(username, Some(password)),
        }
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn bounded_body_context(body: &str) -> String {
    let mut context: String = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
    if body.chars().count() > MAX_ERROR_BODY_CHARS {
        context.push_str("...");
    }
    if context.is_empty() {
        "<empty body>".into()
    } else {
        context
    }
}

#[cfg(test)]
mod tests {
    use super::{Auth, HttpClient};
    use crate::test_support::{response, test_server};
    use anyhow::Context;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn bearer_auth_and_json_errors_are_normalized() {
        let server = test_server(|request| {
            assert_eq!(request.header("authorization"), Some("Bearer test-token"));
            response(200, r#"{"default_branch":"main"}"#)
        })
        .await;

        let client = HttpClient::new(server.url(), Auth::Bearer("test-token".into())).unwrap();
        let value: serde_json::Value = client.get_json("projects/example").await.unwrap();
        assert_eq!(value["default_branch"], "main");
    }

    #[tokio::test]
    async fn non_success_status_includes_bounded_body_context() {
        let body = format!("{{\"message\":\"{}\"}}", "x".repeat(500));
        let server = test_server(move |_| response(500, &body)).await;
        let client = HttpClient::new(server.url(), Auth::None).unwrap();

        let error = client
            .get_json_value("projects/example")
            .await
            .expect_err("a non-success status must be rejected")
            .to_string();
        assert!(error.contains("500"));
        assert!(error.contains("message"));
        assert!(error.len() < 400, "response body context must be bounded");
    }

    #[tokio::test]
    async fn malformed_json_includes_endpoint_context() {
        let server = test_server(|_| response(200, "not json")).await;
        let client = HttpClient::new(server.url(), Auth::None).unwrap();

        let error = client
            .get_json_value("projects/example")
            .await
            .expect_err("malformed JSON must be rejected")
            .to_string();
        assert!(error.contains("projects/example"));
        assert!(error.contains("JSON"));
    }

    #[tokio::test]
    async fn pagination_follows_a_page_object_next_link() {
        let server = test_server(|request| match request.path() {
            "/api/v1/projects/example" => response(
                200,
                r#"{"values":[{"id":1}],"next":"/projects/example?page=2"}"#,
            ),
            "/projects/example?page=2" => response(200, r#"{"values":[{"id":2}]}"#),
            path => panic!("unexpected request path: {path}"),
        })
        .await;
        let client = HttpClient::new(server.url(), Auth::None).unwrap();

        let rows = client
            .paginate_json("projects/example", |page| {
                let rows = page["values"].as_array().context("missing values")?.clone();
                let next = page["next"].as_str().map(str::to_owned);
                Ok((rows, next))
            })
            .await
            .unwrap();
        assert_eq!(
            rows,
            vec![serde_json::json!({"id": 1}), serde_json::json!({"id": 2})]
        );
    }

    #[tokio::test]
    async fn pagination_accepts_a_provider_owned_array_page_shape() {
        let server = test_server(|_| response(200, r#"[{"id":1},{"id":2}]"#)).await;
        let client = HttpClient::new(server.url(), Auth::None).unwrap();

        let rows = client
            .paginate_json("projects/example", |page| {
                let rows = page.as_array().context("expected an array page")?.clone();
                Ok((rows, None))
            })
            .await
            .unwrap();
        assert_eq!(
            rows,
            vec![serde_json::json!({"id": 1}), serde_json::json!({"id": 2})]
        );
    }

    #[tokio::test]
    async fn pagination_rejects_cross_origin_next_links_before_sending_credentials() {
        let other_origin_was_requested = Arc::new(AtomicBool::new(false));
        let other_origin_was_requested_by_server = Arc::clone(&other_origin_was_requested);
        let other_server = test_server(move |_| {
            other_origin_was_requested_by_server.store(true, Ordering::SeqCst);
            response(200, r#"{"values":[{"id":2}]}"#)
        })
        .await;
        let next_link = format!("{}other?page=2", other_server.url());
        let first_page = format!(r#"{{"values":[{{"id":1}}],"next":"{next_link}"}}"#);
        let first_server = test_server(move |_| response(200, &first_page)).await;
        let client =
            HttpClient::new(first_server.url(), Auth::Bearer("must-not-leak".into())).unwrap();

        let error = client
            .paginate_json("projects/example", |page| {
                let rows = page["values"].as_array().context("missing values")?.clone();
                let next = page["next"].as_str().map(str::to_owned);
                Ok((rows, next))
            })
            .await
            .expect_err("cross-origin continuation links must be rejected")
            .to_string();

        assert!(
            error.contains("different origin"),
            "unexpected error: {error}"
        );
        assert!(!other_origin_was_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn credentialed_non_loopback_http_is_rejected() {
        let error = match HttpClient::new("http://example.com/api/", Auth::Bearer("token".into())) {
            Ok(_) => panic!("credentials must not be configured for public HTTP"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("require HTTPS"));
    }

    #[tokio::test]
    async fn pagination_rejects_repeated_continuation_endpoints() {
        let server =
            test_server(|_| response(200, r#"{"values":[{"id":1}],"next":"/projects/example"}"#))
                .await;
        let client = HttpClient::new(server.url(), Auth::None).unwrap();

        let error = client
            .paginate_json("projects/example", |page| {
                let rows = page["values"].as_array().context("missing values")?.clone();
                let next = page["next"].as_str().map(str::to_owned);
                Ok((rows, next))
            })
            .await
            .expect_err("repeated pagination endpoints must stop");
        assert!(error.to_string().contains("repeated endpoint"));
    }
}
