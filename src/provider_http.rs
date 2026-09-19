use anyhow::{Context, Result, bail};
use reqwest::{Client, RequestBuilder, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;

const MAX_ERROR_BODY_CHARS: usize = 200;

pub(crate) enum Auth {
    None,
    Bearer(String),
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
        let path = base_url.path().to_string();
        if !path.ends_with('/') {
            base_url.set_path(&format!("{path}/"));
        }

        Ok(Self {
            client: Client::new(),
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
        while let Some(endpoint) = next {
            let page: Value = self.get_json_value(&endpoint).await?;
            let (page_rows, continuation) = parse_page(&page)
                .with_context(|| format!("Invalid provider pagination page at '{endpoint}'"))?;
            rows.extend(page_rows);
            next = continuation;
        }
        Ok(rows)
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Url::parse(path).with_context(|| format!("Invalid provider endpoint '{path}'"));
        }
        if path.starts_with('/') {
            return self
                .base_url
                .join(path)
                .with_context(|| format!("Invalid provider endpoint '{path}'"));
        }
        self.base_url
            .join(path.trim_start_matches('/'))
            .with_context(|| format!("Invalid provider endpoint '{path}'"))
    }

    fn request(&self, url: Url) -> RequestBuilder {
        let request = self.client.get(url);
        match &self.auth {
            Auth::None => request,
            Auth::Bearer(token) => request.bearer_auth(token),
            Auth::Basic { username, password } => request.basic_auth(username, Some(password)),
        }
    }
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
    use anyhow::Context;
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Arc,
        thread,
    };

    struct Request {
        headers: HashMap<String, String>,
        path: String,
    }

    impl Request {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .get(&name.to_ascii_lowercase())
                .map(String::as_str)
        }
    }

    struct Response {
        status: u16,
        body: String,
    }

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            body: body.into(),
        }
    }

    struct TestServer {
        url: String,
    }

    impl TestServer {
        fn url(&self) -> &str {
            &self.url
        }
    }

    async fn test_server(
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handler = Arc::new(handler);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let handler = Arc::clone(&handler);
                thread::spawn(move || serve_request(stream, handler));
            }
        });
        TestServer {
            url: format!("http://{address}/api/v1/"),
        }
    }

    fn serve_request(
        mut stream: TcpStream,
        handler: Arc<dyn Fn(&Request) -> Response + Send + Sync>,
    ) {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let Ok(read) = stream.read(&mut buffer) else {
                return;
            };
            if read == 0 {
                return;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.lines();
        let request_line = lines.next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let headers = lines
            .take_while(|line| !line.is_empty())
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.to_ascii_lowercase(), value.trim().to_string()))
            })
            .collect();
        let request = Request { headers, path };
        let response = handler(&request);
        let reason = match response.status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Response",
        };
        let payload = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.status,
            response.body.len(),
            response.body
        );
        let _ = stream.write_all(payload.as_bytes());
    }

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
        let server = test_server(|request| match request.path.as_str() {
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
}
