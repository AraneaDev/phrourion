pub mod command;
pub mod forgejo;
pub mod git;
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
        collections::HashMap,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Arc,
        thread,
    };

    // Shared by src/forgejo.rs's and src/provider.rs's unit tests: both set
    // the process-global PHROURION_FORGEJO_TEST_URL var, and Rust's test
    // harness runs different #[test]/#[tokio::test] fns concurrently on
    // separate threads by default.
    pub(crate) static FORGEJO_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) struct Request {
        headers: HashMap<String, String>,
        path: String,
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
        url: String,
    }

    impl TestServer {
        pub(crate) fn url(&self) -> &str {
            &self.url
        }
    }

    pub(crate) async fn test_server(
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
}
