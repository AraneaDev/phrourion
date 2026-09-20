#[allow(dead_code)]
pub mod bitbucket;
pub mod command;
pub mod forgejo;
pub mod git;
#[allow(dead_code)]
pub mod gitlab;
pub mod model;
pub mod provider;
#[allow(dead_code)]
pub(crate) mod provider_http;
pub mod registry;
pub mod terminal;
pub mod tui;

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        collections::{HashMap, VecDeque},
        fs,
        io::{ErrorKind, Read, Write},
        net::{TcpListener, TcpStream},
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc::{self, Receiver},
        },
        thread::{self, JoinHandle},
        time::Duration,
    };

    // Shared by src/forgejo.rs's and src/provider.rs's unit tests: both set
    // the process-global PHROURION_FORGEJO_TEST_URL var, and Rust's test
    // harness runs different #[test]/#[tokio::test] fns concurrently on
    // separate threads by default.
    pub(crate) static FORGEJO_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) fn fixture(path: &str) -> serde_json::Value {
        let full_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let contents = fs::read_to_string(&full_path).unwrap_or_else(|error| {
            panic!("failed to read fixture {}: {error}", full_path.display())
        });
        serde_json::from_str(&contents).unwrap_or_else(|error| {
            panic!("failed to parse fixture {}: {error}", full_path.display())
        })
    }

    #[derive(Clone, Debug)]
    pub(crate) struct Request {
        headers: HashMap<String, String>,
        method: String,
        path: String,
        query: Option<String>,
        url_path: String,
    }

    impl Request {
        pub(crate) fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .get(&name.to_ascii_lowercase())
                .map(String::as_str)
        }

        pub(crate) fn path(&self) -> &str {
            &self.path
        }

        pub(crate) fn method(&self) -> &str {
            &self.method
        }

        pub(crate) fn query(&self) -> Option<&str> {
            self.query.as_deref()
        }

        pub(crate) fn url_path(&self) -> &str {
            &self.url_path
        }
    }

    pub(crate) struct Response {
        status: u16,
        body: String,
    }

    pub(crate) fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            body: body.into(),
        }
    }

    pub(crate) struct TestServer {
        root_url: String,
        url: String,
        shutdown: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestServer {
        pub(crate) fn url(&self) -> &str {
            &self.url
        }

        pub(crate) fn root_url(&self) -> &str {
            &self.root_url
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    pub(crate) struct MockServer {
        server: TestServer,
        requests: Receiver<Request>,
    }

    impl MockServer {
        pub(crate) fn url(&self) -> &str {
            self.server.url()
        }

        pub(crate) fn root_url(&self) -> &str {
            self.server.root_url()
        }

        pub(crate) fn next_request(&self) -> Request {
            self.requests
                .recv_timeout(Duration::from_secs(1))
                .expect("mock server did not receive a request within one second")
        }

        pub(crate) fn requests(&self) -> Vec<Request> {
            self.requests.try_iter().collect()
        }
    }

    pub(crate) async fn test_server(
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let handler = Arc::new(handler);
        let shutdown = Arc::new(AtomicBool::new(false));
        let listener_shutdown = Arc::clone(&shutdown);
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !listener_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(250)))
                            .expect("failed to set test server client read timeout");
                        let handler = Arc::clone(&handler);
                        workers.push(thread::spawn(move || serve_request(stream, handler)));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        TestServer {
            root_url: format!("http://{address}"),
            url: format!("http://{address}/api/v1/"),
            shutdown,
            thread: Some(thread),
        }
    }

    pub(crate) async fn mock_server(responses: Vec<Response>) -> MockServer {
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let (request_tx, requests) = mpsc::channel();
        let server = test_server(move |request| {
            request_tx
                .send(request.clone())
                .expect("mock request receiver was dropped");
            responses
                .lock()
                .expect("mock response queue lock was poisoned")
                .pop_front()
                .expect("mock response queue exhausted")
        })
        .await;
        MockServer { server, requests }
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
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap_or_default().to_string();
        let path = request_parts.next().unwrap_or_default().to_string();
        let (url_path, query) = path
            .split_once('?')
            .map_or((path.as_str(), None), |(url_path, query)| {
                (url_path, Some(query.to_string()))
            });
        let url_path = url_path.to_string();
        let headers = lines
            .take_while(|line| !line.is_empty())
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.to_ascii_lowercase(), value.trim().to_string()))
            })
            .collect();
        let request = Request {
            headers,
            method,
            path,
            query,
            url_path,
        };
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

    #[cfg(test)]
    mod tests {
        use super::{fixture, mock_server, response, test_server};
        use serde_json::json;
        use std::{io::Write as _, net::TcpStream, sync::mpsc, thread, time::Duration};

        #[test]
        fn fixture_loads_json_relative_to_the_crate_root() {
            assert_eq!(
                fixture("tests/fixtures/gitlab/project.json"),
                json!({
                    "id": 42,
                    "path_with_namespace": "acme/widgets",
                    "default_branch": "main",
                    "web_url": "https://gitlab.example/acme/widgets"
                })
            );
        }

        #[test]
        #[should_panic(expected = "tests/fixtures/gitlab/missing.json")]
        fn fixture_reports_the_missing_path() {
            fixture("tests/fixtures/gitlab/missing.json");
        }

        #[test]
        fn representative_provider_fixtures_are_valid_json() {
            for path in [
                "tests/fixtures/gitlab/project.json",
                "tests/fixtures/gitlab/branches.json",
                "tests/fixtures/gitlab/merge_requests.json",
                "tests/fixtures/gitlab/issues.json",
                "tests/fixtures/gitlab/releases.json",
                "tests/fixtures/gitlab/reviewers.json",
                "tests/fixtures/gitlab/pipelines.json",
                "tests/fixtures/bitbucket/repository.json",
                "tests/fixtures/bitbucket/branches.json",
                "tests/fixtures/bitbucket/pull_requests.json",
                "tests/fixtures/bitbucket/issues.json",
                "tests/fixtures/bitbucket/reviewers.json",
                "tests/fixtures/bitbucket/pipelines.json",
            ] {
                assert!(!fixture(path).is_null(), "fixture was JSON null: {path}");
            }
        }

        #[tokio::test]
        async fn test_server_stops_accepting_connections_when_dropped() {
            let server = test_server(|_| response(200, "{}")).await;
            let url = reqwest::Url::parse(server.url()).unwrap();
            let address = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());

            drop(server);

            assert!(
                TcpStream::connect(address).is_err(),
                "the listener must be closed when its server handle is dropped"
            );
        }

        #[tokio::test]
        async fn test_server_drop_completes_with_an_incomplete_client_request() {
            let server = test_server(|_| response(200, "{}")).await;
            let url = reqwest::Url::parse(server.url()).unwrap();
            let address = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
            let mut incomplete_client = TcpStream::connect(address).unwrap();
            incomplete_client
                .write_all(b"GET /stalled HTTP/1.1\r\nHost: localhost\r\n")
                .unwrap();

            // A completed request proves the accept loop has already spawned
            // the worker that is blocked on the earlier incomplete request.
            let response = reqwest::get(format!("{}ready", server.url()))
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);

            let (dropped_tx, dropped_rx) = mpsc::channel();
            let drop_thread = thread::spawn(move || {
                drop(server);
                dropped_tx.send(()).unwrap();
            });
            let completed = dropped_rx.recv_timeout(Duration::from_secs(1)).is_ok();

            drop(incomplete_client);
            drop_thread.join().unwrap();
            assert!(
                completed,
                "dropping the server must not wait indefinitely for incomplete requests"
            );
        }

        #[tokio::test]
        async fn queued_mock_server_returns_responses_in_order() {
            let server = mock_server(vec![
                response(200, r#"{"page":1}"#),
                response(200, r#"{"page":2}"#),
            ])
            .await;

            let first: serde_json::Value = reqwest::get(format!("{}first", server.url()))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let second: serde_json::Value = reqwest::get(format!("{}second", server.url()))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            assert_eq!(first, json!({ "page": 1 }));
            assert_eq!(second, json!({ "page": 2 }));
            assert_eq!(
                server
                    .requests()
                    .iter()
                    .map(|request| request.url_path())
                    .collect::<Vec<_>>(),
                ["/api/v1/first", "/api/v1/second"]
            );
        }

        #[tokio::test]
        async fn queued_mock_server_records_request_parts() {
            let server = mock_server(vec![response(200, "{}")]).await;
            let response = reqwest::Client::new()
                .get(format!(
                    "{}projects/acme?state=opened&limit=2",
                    server.url()
                ))
                .header("X-Test-Token", "sanitized-token")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);

            let request = server.next_request();
            assert_eq!(request.method(), "GET");
            assert_eq!(request.url_path(), "/api/v1/projects/acme");
            assert_eq!(request.query(), Some("state=opened&limit=2"));
            assert_eq!(request.header("x-test-token"), Some("sanitized-token"));
        }
    }
}
