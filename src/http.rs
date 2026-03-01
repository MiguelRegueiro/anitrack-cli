use std::thread;
use std::time::Duration;

fn should_retry_http_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..=599).contains(&status)
}

pub(crate) fn get_text_with_retries(
    url: &str,
    referer: &str,
    query: &[(String, String)],
    connect_timeout: Duration,
    read_timeout: Duration,
    attempts: usize,
    retry_delay: Duration,
) -> Result<String, String> {
    let attempts = attempts.max(1);

    for attempt in 1..=attempts {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(connect_timeout)
            .timeout_read(read_timeout)
            .timeout_write(read_timeout)
            .build();

        let mut request = agent.get(url).set("Referer", referer);
        for (key, value) in query {
            request = request.query(key, value);
        }

        match request.call() {
            Ok(response) => match response.into_string() {
                Ok(body) => return Ok(body),
                Err(err) => {
                    return Err(format!("request failed: response decode failed: {err}"));
                }
            },
            Err(ureq::Error::Status(status, response)) => {
                let response_body = response.into_string().ok().unwrap_or_default();
                let body = response_body.trim();
                let status_error = if body.is_empty() {
                    format!("HTTP status {status}")
                } else {
                    let truncated = body.chars().take(240).collect::<String>();
                    format!("HTTP status {status} ({truncated})")
                };

                if should_retry_http_status(status) && attempt < attempts {
                    thread::sleep(retry_delay);
                    continue;
                }

                if should_retry_http_status(status) {
                    return Err(format!(
                        "request failed after {attempts} attempt(s): {status_error}"
                    ));
                }

                return Err(format!("request failed: {status_error}"));
            }
            Err(ureq::Error::Transport(err)) => {
                let transport_error = format!("transport error: {err}");
                if attempt < attempts {
                    thread::sleep(retry_delay);
                    continue;
                }
                return Err(format!(
                    "request failed after {attempts} attempt(s): {transport_error}"
                ));
            }
        }
    }

    Err("request failed: exhausted attempts without a concrete error".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    enum Behavior {
        Respond(u16, String),
        DelayRespond(Duration, u16, String),
    }

    #[derive(Debug)]
    struct TestServer {
        base_url: String,
        requests: Arc<AtomicUsize>,
        shutdown_tx: mpsc::Sender<()>,
        join_handle: Option<std::thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn spawn(behaviors: Vec<Behavior>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test server");
            listener.set_nonblocking(true).expect("set nonblocking");
            let addr = listener.local_addr().expect("local addr");

            let requests = Arc::new(AtomicUsize::new(0));
            let requests_clone = Arc::clone(&requests);
            let shared_behaviors = Arc::new(Mutex::new(VecDeque::from(behaviors)));
            let behaviors_clone = Arc::clone(&shared_behaviors);
            let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

            let join_handle = std::thread::spawn(move || {
                loop {
                    if shutdown_rx.try_recv().is_ok() {
                        break;
                    }

                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            requests_clone.fetch_add(1, Ordering::SeqCst);
                            let behavior = {
                                let mut queue = behaviors_clone.lock().expect("lock behaviors");
                                queue.pop_front().unwrap_or_else(|| {
                                    Behavior::Respond(200, "default-ok".to_string())
                                })
                            };
                            std::thread::spawn(move || {
                                let _ = consume_request(&mut stream);
                                serve_behavior(&mut stream, behavior);
                            });
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                base_url: format!("http://{addr}"),
                requests,
                shutdown_tx,
                join_handle: Some(join_handle),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            let _ = self.shutdown_tx.send(());
            if let Some(handle) = self.join_handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn consume_request(stream: &mut TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut buf = [0_u8; 1024];
        let mut data = Vec::new();
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => {
                    data.extend_from_slice(&buf[..read]);
                    if data.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    fn reason_phrase(status: u16) -> &'static str {
        match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            408 => "Request Timeout",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "Status",
        }
    }

    fn serve_behavior(stream: &mut TcpStream, behavior: Behavior) {
        match behavior {
            Behavior::Respond(status, body) => {
                let _ = write_response(stream, status, &body);
            }
            Behavior::DelayRespond(delay, status, body) => {
                std::thread::sleep(delay);
                let _ = write_response(stream, status, &body);
            }
        }
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
        let reason = reason_phrase(status);
        let payload = body.as_bytes();
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        )?;
        stream.write_all(payload)?;
        stream.flush()
    }

    #[test]
    fn retries_retryable_statuses_until_success() {
        let server = TestServer::spawn(vec![
            Behavior::Respond(500, "server-error".to_string()),
            Behavior::Respond(429, "throttled".to_string()),
            Behavior::Respond(200, "ok".to_string()),
        ]);
        let query = vec![("q".to_string(), "x".to_string())];

        let result = get_text_with_retries(
            &server.base_url,
            "https://example.test",
            &query,
            Duration::from_millis(200),
            Duration::from_millis(200),
            3,
            Duration::from_millis(1),
        );

        assert_eq!(result.expect("should eventually succeed"), "ok");
        assert_eq!(server.request_count(), 3);
    }

    #[test]
    fn does_not_retry_hard_client_errors() {
        let server = TestServer::spawn(vec![Behavior::Respond(404, "not-found".to_string())]);
        let query = vec![("q".to_string(), "x".to_string())];

        let result = get_text_with_retries(
            &server.base_url,
            "https://example.test",
            &query,
            Duration::from_millis(200),
            Duration::from_millis(200),
            5,
            Duration::from_millis(1),
        );

        let err = result.expect_err("404 should not be retried");
        assert!(
            err.contains("HTTP status 404"),
            "unexpected error message: {err}"
        );
        assert_eq!(server.request_count(), 1);
    }

    #[test]
    fn retries_transport_timeout_and_recovers() {
        let server = TestServer::spawn(vec![
            Behavior::DelayRespond(Duration::from_millis(120), 200, "slow".to_string()),
            Behavior::Respond(200, "ok".to_string()),
        ]);
        let query = vec![("q".to_string(), "x".to_string())];

        let result = get_text_with_retries(
            &server.base_url,
            "https://example.test",
            &query,
            Duration::from_millis(250),
            Duration::from_millis(20),
            2,
            Duration::from_millis(1),
        );

        assert_eq!(result.expect("timeout should be retried"), "ok");
        assert_eq!(server.request_count(), 2);
    }

    #[test]
    fn returns_retry_exhausted_error_for_retryable_status() {
        let server = TestServer::spawn(vec![
            Behavior::Respond(503, "down".to_string()),
            Behavior::Respond(503, "still-down".to_string()),
        ]);
        let query = vec![("q".to_string(), "x".to_string())];

        let result = get_text_with_retries(
            &server.base_url,
            "https://example.test",
            &query,
            Duration::from_millis(200),
            Duration::from_millis(200),
            2,
            Duration::from_millis(1),
        );

        let err = result.expect_err("retryable failures should eventually error");
        assert!(
            err.contains("after 2 attempt(s)") && err.contains("HTTP status 503"),
            "unexpected error message: {err}"
        );
        assert_eq!(server.request_count(), 2);
    }
}
