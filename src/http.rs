use std::thread;
use std::time::Duration;

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
    let mut last_error = String::new();

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
                    last_error = format!("response decode failed: {err}");
                }
            },
            Err(ureq::Error::Status(status, response)) => {
                let response_body = response.into_string().ok().unwrap_or_default();
                let body = response_body.trim();
                if body.is_empty() {
                    last_error = format!("HTTP status {status}");
                } else {
                    let truncated = body.chars().take(240).collect::<String>();
                    last_error = format!("HTTP status {status} ({truncated})");
                }
            }
            Err(ureq::Error::Transport(err)) => {
                last_error = format!("transport error: {err}");
            }
        }

        if attempt < attempts {
            thread::sleep(retry_delay);
        }
    }

    Err(format!(
        "request failed after {attempts} attempt(s): {last_error}"
    ))
}
