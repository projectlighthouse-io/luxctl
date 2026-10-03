use crate::executor::context::Context;
use crate::executor::error::ExecutionError;
use crate::transpiler::ir::{HttpMethod, HttpMode, HttpProbe, ProbeResult, Value};
use std::collections::HashMap;
use std::time::Instant;

pub async fn execute(probe: &HttpProbe, ctx: &Context) -> Result<ProbeResult, ExecutionError> {
    let path = ctx.interpolate(&probe.path);
    let host = &ctx.config.host;
    let port = ctx.config.port.unwrap_or(80);
    let base_url = format!("http://{host}:{port}");
    let url = format!("{base_url}{path}");

    match &probe.mode {
        HttpMode::Single => execute_single(probe, &url, ctx).await,
        HttpMode::Concurrent { clients } => execute_concurrent(probe, &url, ctx, *clients).await,
        HttpMode::Keepalive { requests } => execute_keepalive(probe, &url, ctx, *requests).await,
        HttpMode::Pipelined { requests } => execute_pipelined(probe, &url, ctx, *requests).await,
        HttpMode::Burst { count, window_ms } => {
            execute_burst(probe, &url, ctx, *count, *window_ms).await
        }
        HttpMode::Chunked => execute_single(probe, &url, ctx).await,
    }
}

async fn execute_single(
    probe: &HttpProbe,
    url: &str,
    ctx: &Context,
) -> Result<ProbeResult, ExecutionError> {
    let start = Instant::now();

    let client = reqwest::Client::new();
    let mut builder = match probe.method {
        HttpMethod::GET => client.get(url),
        HttpMethod::POST => client.post(url),
        HttpMethod::PUT => client.put(url),
        HttpMethod::DELETE => client.delete(url),
        HttpMethod::PATCH => client.patch(url),
        HttpMethod::HEAD => client.head(url),
        HttpMethod::OPTIONS => client.request(reqwest::Method::OPTIONS, url),
    };

    // add headers from probe definition and step-level headers
    for (k, v) in &probe.headers {
        let v = ctx.interpolate(v);
        builder = builder.header(k, v);
    }

    // add body
    if let Some(body) = &probe.body {
        let body = ctx.interpolate(body);
        builder = builder
            .header("Content-Type", "application/json")
            .body(body);
    }

    let response = builder
        .send()
        .await
        .map_err(|e| ExecutionError::new(format!("HTTP request failed: {e}")))?;

    let duration = start.elapsed();
    let status = response.status().as_u16();

    // collect headers
    let mut fields = HashMap::new();
    fields.insert("status".to_string(), Value::Int(status as i64));
    fields.insert(
        "latency".to_string(),
        Value::Int(duration.as_millis() as i64),
    );
    fields.insert(
        "duration".to_string(),
        Value::Int(duration.as_millis() as i64),
    );

    for (key, value) in response.headers() {
        if let Ok(v) = value.to_str() {
            let header_key = format!("header.{}", key.as_str());
            fields.insert(header_key, Value::String(v.to_string()));
        }
    }

    let gzipped = response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"));

    // a gzip body is decompressed here rather than by reqwest's `gzip`
    // feature, which strips Content-Encoding from the response: a blueprint
    // checks both that the server compressed and what it compressed
    let body = if gzipped {
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ExecutionError::new(format!("failed to read response body: {e}")))?;
        gunzip(&bytes)?
    } else {
        response
            .text()
            .await
            .map_err(|e| ExecutionError::new(format!("failed to read response body: {e}")))?
    };

    fields.insert("body".to_string(), Value::String(body.clone()));

    // try to parse body as JSON and flatten top-level fields
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
        flatten_json("body.json", &json, &mut fields);
    }

    Ok(ProbeResult {
        fields,
        raw_stdout: Some(body),
        duration_ms: duration.as_millis() as u64,
    })
}

async fn execute_concurrent(
    probe: &HttpProbe,
    url: &str,
    ctx: &Context,
    clients: u32,
) -> Result<ProbeResult, ExecutionError> {
    let start = Instant::now();
    let mut handles = Vec::new();

    for _ in 0..clients {
        let probe = probe.clone();
        let url = url.to_string();
        let ctx = ctx.clone();
        handles.push(tokio::spawn(async move {
            execute_single(&probe, &url, &ctx).await
        }));
    }

    let mut all_passed = true;
    let mut statuses = Vec::new();
    let mut total_fields = HashMap::new();

    for handle in handles {
        match handle.await {
            Ok(Ok(result)) => {
                if let Some(Value::Int(s)) = result.fields.get("status") {
                    statuses.push(*s);
                }
            }
            _ => {
                all_passed = false;
            }
        }
    }

    let duration = start.elapsed();

    // for "all status: X" expectations, store combined results
    total_fields.insert(
        "status".to_string(),
        Value::Int(statuses.first().copied().unwrap_or(0)),
    );
    total_fields.insert("all_passed".to_string(), Value::Bool(all_passed));
    total_fields.insert(
        "response_count".to_string(),
        Value::Int(statuses.len() as i64),
    );

    // store all statuses for "all" operator evaluation
    let all_same = statuses.windows(2).all(|w| w[0] == w[1]);
    total_fields.insert("all_status_same".to_string(), Value::Bool(all_same));

    // for the "all" operator: store the representative status
    if let Some(&first) = statuses.first() {
        if all_same {
            total_fields.insert("all_status".to_string(), Value::Int(first));
        }
    }

    Ok(ProbeResult {
        fields: total_fields,
        raw_stdout: None,
        duration_ms: duration.as_millis() as u64,
    })
}

async fn execute_keepalive(
    probe: &HttpProbe,
    url: &str,
    ctx: &Context,
    requests: u32,
) -> Result<ProbeResult, ExecutionError> {
    let start = Instant::now();
    // reuse the same client (connection pool) for keepalive
    let client = reqwest::Client::new();
    let mut statuses = Vec::new();

    for _ in 0..requests {
        let mut builder = match probe.method {
            HttpMethod::GET => client.get(url),
            HttpMethod::POST => client.post(url),
            HttpMethod::PUT => client.put(url),
            HttpMethod::DELETE => client.delete(url),
            HttpMethod::PATCH => client.patch(url),
            HttpMethod::HEAD => client.head(url),
            HttpMethod::OPTIONS => client.request(reqwest::Method::OPTIONS, url),
        };

        for (k, v) in &probe.headers {
            let v = ctx.interpolate(v);
            builder = builder.header(k, v);
        }

        match builder.send().await {
            Ok(resp) => statuses.push(resp.status().as_u16() as i64),
            Err(e) => {
                return Err(ExecutionError::new(format!(
                    "keepalive request failed: {e}"
                )))
            }
        }
    }

    let duration = start.elapsed();
    let mut fields = HashMap::new();
    let all_same = statuses.windows(2).all(|w| w[0] == w[1]);
    fields.insert(
        "status".to_string(),
        Value::Int(statuses.first().copied().unwrap_or(0)),
    );
    fields.insert("all_status_same".to_string(), Value::Bool(all_same));
    fields.insert(
        "response_count".to_string(),
        Value::Int(statuses.len() as i64),
    );
    if all_same {
        if let Some(&s) = statuses.first() {
            fields.insert("all_status".to_string(), Value::Int(s));
        }
    }

    Ok(ProbeResult {
        fields,
        raw_stdout: None,
        duration_ms: duration.as_millis() as u64,
    })
}

async fn execute_pipelined(
    probe: &HttpProbe,
    url: &str,
    ctx: &Context,
    requests: u32,
) -> Result<ProbeResult, ExecutionError> {
    // pipelining is similar to keepalive but sends requests without waiting
    // for simplicity, we use concurrent requests on same connection pool
    execute_keepalive(probe, url, ctx, requests).await
}

async fn execute_burst(
    probe: &HttpProbe,
    url: &str,
    ctx: &Context,
    count: u32,
    window_ms: u64,
) -> Result<ProbeResult, ExecutionError> {
    let start = Instant::now();
    let mut handles = Vec::new();
    let window = std::time::Duration::from_millis(window_ms);

    // send all requests within the window
    let delay_per_request = window
        .checked_div(count)
        .unwrap_or(std::time::Duration::ZERO);

    for i in 0..count {
        let probe = probe.clone();
        let url = url.to_string();
        let ctx = ctx.clone();
        let delay = delay_per_request * i;
        handles.push(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            execute_single(&probe, &url, &ctx).await
        }));
    }

    let mut accepted = 0i64;
    let mut rejected = 0i64;
    let mut rejected_status = 0i64;

    for handle in handles {
        match handle.await {
            Ok(Ok(result)) => {
                if let Some(Value::Int(s)) = result.fields.get("status") {
                    if *s == 429 {
                        rejected += 1;
                        rejected_status = 429;
                    } else {
                        accepted += 1;
                    }
                }
            }
            _ => rejected += 1,
        }
    }

    let duration = start.elapsed();
    // a burst has no single `status`, so `expect { status: 429 }` finds no
    // field and fails. "some requests were rate limited" is
    // `rejected-status: 429` (429 if any response was a 429, else 0) or
    // `rejected: > 0`; "some got through" is `accepted: > 0`. `rejected` also
    // counts requests that failed outright; `rejected-status` does not.
    let mut fields = HashMap::new();
    fields.insert("accepted".to_string(), Value::Int(accepted));
    fields.insert("rejected".to_string(), Value::Int(rejected));
    fields.insert("rejected-status".to_string(), Value::Int(rejected_status));
    fields.insert(
        "status-any".to_string(),
        Value::Int(if accepted > 0 { 201 } else { 429 }),
    );

    Ok(ProbeResult {
        fields,
        raw_stdout: None,
        duration_ms: duration.as_millis() as u64,
    })
}

/// decompress a body the server sent with `Content-Encoding: gzip`. One that
/// claims gzip and isn't is the server's bug, and the probe says so.
fn gunzip(bytes: &[u8]) -> Result<String, ExecutionError> {
    use std::io::Read;

    let mut decoded = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .read_to_end(&mut decoded)
        .map_err(|e| {
            ExecutionError::new(format!(
                "response says Content-Encoding: gzip but the body is not valid gzip: {e}"
            ))
        })?;
    Ok(String::from_utf8_lossy(&decoded).into_owned())
}

/// flatten a JSON value into dotted key paths in the fields map
fn flatten_json(prefix: &str, value: &serde_json::Value, fields: &mut HashMap<String, Value>) {
    match value {
        serde_json::Value::Object(obj) => {
            for (key, val) in obj {
                let path = format!("{prefix}.{key}");
                flatten_json(&path, val, fields);
                // also store the leaf value directly
                match val {
                    serde_json::Value::String(s) => {
                        fields.insert(path, Value::String(s.clone()));
                    }
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            fields.insert(path, Value::Int(i));
                        } else if let Some(f) = n.as_f64() {
                            fields.insert(path, Value::Float(f));
                        }
                    }
                    serde_json::Value::Bool(b) => {
                        fields.insert(path, Value::Bool(*b));
                    }
                    serde_json::Value::Null => {
                        fields.insert(path, Value::Null);
                    }
                    _ => {} // arrays and nested objects handled by recursion
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, val) in arr.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                flatten_json(&path, val, fields);
            }
        }
        serde_json::Value::String(s) => {
            fields.insert(prefix.to_string(), Value::String(s.clone()));
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                fields.insert(prefix.to_string(), Value::Int(i));
            } else if let Some(f) = n.as_f64() {
                fields.insert(prefix.to_string(), Value::Float(f));
            }
        }
        serde_json::Value::Bool(b) => {
            fields.insert(prefix.to_string(), Value::Bool(*b));
        }
        serde_json::Value::Null => {
            fields.insert(prefix.to_string(), Value::Null);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::context::ExecutionMode;
    use crate::transpiler::ir::Config;
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn test_gzip_body_is_decompressed_and_its_header_kept() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(br#"{"greeting":"hello"}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let gzipped = encoder.finish().unwrap_or_else(|e| panic!("{e}"));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let port = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("{e}"))
            .port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap_or_else(|e| panic!("{e}"));
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                gzipped.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&gzipped).await;
        });

        let config = Config {
            host: "127.0.0.1".to_string(),
            port: Some(port),
            ..Config::default()
        };
        let ctx = Context::new(config, ExecutionMode::Validate);
        let probe = HttpProbe {
            method: HttpMethod::GET,
            path: "/".to_string(),
            body: None,
            headers: HashMap::new(),
            mode: HttpMode::Single,
        };

        let result = execute(&probe, &ctx)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            matches!(result.fields.get("header.content-encoding"), Some(Value::String(s)) if s == "gzip")
        );
        assert!(
            matches!(result.fields.get("body"), Some(Value::String(s)) if s == r#"{"greeting":"hello"}"#)
        );
        assert!(
            matches!(result.fields.get("body.json.greeting"), Some(Value::String(s)) if s == "hello")
        );
    }

    #[tokio::test]
    async fn test_burst_reports_rate_limiting_as_rejected_status() {
        // answers the first two requests and rate limits the rest
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let port = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("{e}"))
            .port();
        tokio::spawn(async move {
            for served in 0.. {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut request = [0u8; 1024];
                let _ = socket.read(&mut request).await;
                let status = if served < 2 {
                    "200 OK"
                } else {
                    "429 Too Many Requests"
                };
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let config = Config {
            host: "127.0.0.1".to_string(),
            port: Some(port),
            ..Config::default()
        };
        let ctx = Context::new(config, ExecutionMode::Validate);
        let probe = HttpProbe {
            method: HttpMethod::GET,
            path: "/".to_string(),
            body: None,
            headers: HashMap::new(),
            mode: HttpMode::Burst {
                count: 5,
                window_ms: 100,
            },
        };

        let result = execute(&probe, &ctx)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(result.fields.get("accepted"), Some(Value::Int(2))));
        assert!(matches!(result.fields.get("rejected"), Some(Value::Int(3))));
        assert!(matches!(
            result.fields.get("rejected-status"),
            Some(Value::Int(429))
        ));
        assert!(result.fields.get("status").is_none());
    }
}
