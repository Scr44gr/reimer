use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn source(contents: &str) -> Self {
        let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reimer-assertion-{}-{nonce}.reim",
            std::process::id()
        ));
        fs::write(&path, contents).expect("fixture should be written");
        Self { path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[test]
fn run_command_should_report_the_assertion_message() {
    let fixture = Fixture::source(
        "fn main() -> i32 {
            assert(false, \"asset index is invalid\");
            0
        }",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_reimer"))
        .arg("run")
        .arg(&fixture.path)
        .output()
        .expect("compiler process should start");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("asset index is invalid"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_command_should_report_the_result_expect_message() {
    let fixture = Fixture::source(
        "struct PlainError { code: i32 }
        fn fail() -> Result<i32, PlainError> {
            Err(PlainError { code: 7 })
        }
        fn main() -> i32 {
            fail().expect(\"asset load must succeed\")
        }",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_reimer"))
        .arg("run")
        .arg(&fixture.path)
        .output()
        .expect("compiler process should start");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("asset load must succeed"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn release_run_should_omit_a_failing_debug_assertion() {
    let fixture = Fixture::source(
        "fn main() -> i32 {
            debug_assert(false, \"release must not evaluate this\");
            42
        }",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_reimer"))
        .arg("run")
        .arg(&fixture.path)
        .arg("--release")
        .output()
        .expect("compiler process should start");

    assert!(
        output.status.success(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("program returned 42"));
}
