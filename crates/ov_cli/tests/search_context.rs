use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::TcpListener,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn run_context_search(response: Value, args: &[&str]) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "CLI did not connect to the server"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("failed to accept CLI request: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut request_line = line.split_whitespace();
        assert_eq!(request_line.next(), Some("POST"));
        assert_eq!(
            request_line.next().unwrap().split('?').next(),
            Some("/api/v1/search/search")
        );
        let mut content_length = None;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }
        let mut body = vec![0; content_length.expect("request content length")];
        reader.read_exact(&mut body).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["mode"], "context");
        let body = response.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ovcli.conf");
    fs::write(
        &config,
        json!({
            "url": url,
            "output": "table",
            "timeout": 5,
            "echo_command": false,
            "show_progress": false
        })
        .to_string(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ov"))
        .current_dir(dir.path())
        .env("OPENVIKING_CLI_CONFIG_FILE", &config)
        .env("OPENVIKING_LANG", "en")
        .env("NO_COLOR", "1")
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .stdin(Stdio::null())
        .args(["search", "recent decisions", "--mode", "context"])
        .args(args)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

fn context_response(rendered: &str, digest: &str, rewrite: &str, errors: &[&str]) -> Value {
    json!({
        "status": "ok",
        "result": {
            "entries": [],
            "rendered": rendered,
            "digest": digest,
            "stats": {"rewrite": rewrite, "retrieval_errors": errors}
        }
    })
}

#[test]
fn partial_retrieval_failures_preserve_context_and_warn_on_stderr() {
    let errors = [
        "RuntimeError: embedder unavailable",
        "TimeoutError: index unavailable",
    ];
    let expected_stderr = errors
        .iter()
        .map(|error| format!("Warning: context retrieval failed: {error}\n"))
        .collect::<String>();
    for (digest, expected_text) in [
        ("", "<memory>Full context.\nSecond line.</memory>"),
        ("Concise context.", "Concise context."),
    ] {
        let response = context_response(
            "<memory>Full context.\nSecond line.</memory>",
            digest,
            "off",
            &errors,
        );
        let output = run_context_search(response, &[]);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{expected_text}\n")
        );
        assert_eq!(String::from_utf8(output.stderr).unwrap(), expected_stderr);
    }
}

#[test]
fn retrieval_failures_do_not_report_empty_matches_or_restore_rejected_context() {
    for (rendered, rewrite) in [("", "off"), ("Rejected context.", "no_relevant")] {
        let response = context_response(
            rendered,
            "",
            rewrite,
            &["RuntimeError: embedder unavailable"],
        );
        let output = run_context_search(response, &[]);
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "Warning: context retrieval failed: RuntimeError: embedder unavailable\n"
        );
    }
}

#[test]
fn retrieval_failure_keeps_profile_from_response_envelope() {
    let mut response = context_response(
        "Available context.",
        "",
        "off",
        &["RuntimeError: index unavailable"],
    );
    response["profile"] = json!(["context assembly: 12ms"]);
    let output = run_context_search(response, &["--profile"]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Available context.\n\nprofile\ncontext assembly: 12ms\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Warning: context retrieval failed: RuntimeError: index unavailable\n"
    );
}

#[test]
fn successful_empty_context_keeps_no_matches_message_without_warnings() {
    for (rewrite, expected_text) in [
        ("off", "No context found.\n"),
        ("no_relevant", "No relevant context found.\n"),
    ] {
        let output = run_context_search(context_response("", "", rewrite, &[]), &[]);
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected_text);
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}

#[test]
fn json_preserves_retrieval_errors_without_stderr_warnings() {
    for args in [vec!["-o", "json"], vec!["-o", "json", "--compact=false"]] {
        let response = context_response(
            "Available context.",
            "",
            "off",
            &["RuntimeError: index unavailable"],
        );
        let expected = response["result"].clone();
        let output = run_context_search(response, &args);
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
        let result = actual.get("result").unwrap_or(&actual);
        assert_eq!(result, &expected);
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}
