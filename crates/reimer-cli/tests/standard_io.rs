use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn run_command_should_deliver_piped_standard_input() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_input.reim");
    let mut child = Command::new(env!("CARGO_BIN_EXE_reimer"))
        .arg("run")
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("compiler process should start");
    child
        .stdin
        .take()
        .expect("child standard input should be piped")
        .write_all(b"answer\n")
        .expect("fixture input should be written");

    let output = child
        .wait_with_output()
        .expect("compiler process should complete");

    assert!(
        output.status.success(),
        "compiler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("program returned 42"),
        "unexpected output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
