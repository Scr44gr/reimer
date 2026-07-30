use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

#[test]
fn server_should_complete_an_initialize_shutdown_exchange_over_stdio() {
    let mut server = TestServer::spawn();
    let response = server.request(
        1,
        "initialize",
        &json!({
            "processId": null,
            "rootUri": null,
            "capabilities": {}
        }),
    );
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        "Reimer Language Server"
    );

    server.notify("initialized", &json!({}));
    server.shutdown(2);
}

#[test]
fn server_should_serve_editor_intelligence_over_stdio() {
    let mut server = TestServer::spawn();
    let uri = "untitled:editor-intelligence.reim";
    let source = "fn main() -> i32 { let answer = 42; answer }\n";

    let initialize = server.request(
        1,
        "initialize",
        &json!({
            "processId": null,
            "rootUri": null,
            "capabilities": {
                "textDocument": {
                    "inlayHint": {}
                }
            }
        }),
    );
    assert_eq!(initialize["result"]["capabilities"]["hoverProvider"], true);
    assert!(initialize["result"]["capabilities"]["completionProvider"].is_object());
    assert!(initialize["result"]["capabilities"]["inlayHintProvider"].is_object());

    server.notify("initialized", &json!({}));
    server.notify(
        "textDocument/didOpen",
        &json!({
            "textDocument": {
                "uri": uri,
                "languageId": "reimer",
                "version": 1,
                "text": source
            }
        }),
    );
    let hover = server.request(
        2,
        "textDocument/hover",
        &json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 38 }
        }),
    );
    assert!(
        hover["result"]["contents"]["value"]
            .as_str()
            .is_some_and(|value| {
                value.contains("i32")
                    && value.contains("32-bit signed integer")
                    && !value.to_lowercase().contains("inferred")
            })
    );

    let completion = server.request(
        3,
        "textDocument/completion",
        &json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 23 }
        }),
    );
    let labels = completion["result"]
        .as_array()
        .expect("completion result should be an array")
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"answer"));
    assert!(labels.contains(&"println"));

    let inlay_hints = server.request(
        4,
        "textDocument/inlayHint",
        &json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            }
        }),
    );
    assert!(
        inlay_hints["result"]
            .as_array()
            .expect("inlay hint result should be an array")
            .iter()
            .any(|hint| hint["label"] == ": i32")
    );

    server.shutdown(5);
}

struct TestServer {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl TestServer {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_reimer-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("language server should start");
        let input = child.stdin.take().expect("child stdin should be piped");
        let output = child.stdout.take().expect("child stdout should be piped");
        Self {
            child,
            input,
            output: BufReader::new(output),
        }
    }

    fn request(&mut self, id: u64, method: &str, params: &Value) -> Value {
        send(
            &mut self.input,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }),
        );
        receive_by_id(&mut self.output, id)
    }

    fn notify(&mut self, method: &str, params: &Value) {
        send(
            &mut self.input,
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            }),
        );
    }

    fn shutdown(mut self, id: u64) {
        self.request(id, "shutdown", &Value::Null);
        self.notify("exit", &Value::Null);
        drop(self.input);
        let status = self.child.wait().expect("language server should terminate");
        assert!(status.success(), "language server exited with {status}");
    }
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

fn receive_by_id(output: &mut impl BufRead, id: u64) -> Value {
    loop {
        let message = receive(output);
        if message.get("id") == Some(&json!(id)) {
            return message;
        }
    }
}
