use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn server_should_complete_an_initialize_shutdown_exchange_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reimer-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("language server should start");
    let mut input = child.stdin.take().expect("child stdin should be piped");
    let output = child.stdout.take().expect("child stdout should be piped");
    let mut output = BufReader::new(output);

    send(
        &mut input,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": null,
                "capabilities": {}
            }
        }),
    );
    let response = receive(&mut output);
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        "Reimer Language Server"
    );

    send(
        &mut input,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    send(
        &mut input,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null
        }),
    );
    let mut shutdown = receive(&mut output);
    while shutdown.get("id") != Some(&json!(2)) {
        shutdown = receive(&mut output);
    }
    send(
        &mut input,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );
    drop(input);

    let status = child.wait().expect("language server should terminate");

    assert!(status.success(), "language server exited with {status}");
}

fn send(input: &mut impl Write, message: &Value) {
    let body = serde_json::to_vec(message).expect("message should serialize");
    write!(input, "Content-Length: {}\r\n\r\n", body.len()).expect("header should be written");
    input.write_all(&body).expect("body should be written");
    input.flush().expect("message should be flushed");
}

fn receive(output: &mut impl BufRead) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        output.read_line(&mut line).expect("header should be read");
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .expect("content length should be valid"),
            );
        }
    }
    let mut body = vec![0; length.expect("content length header should exist")];
    output.read_exact(&mut body).expect("body should be read");
    serde_json::from_slice(&body).expect("body should be valid JSON")
}
