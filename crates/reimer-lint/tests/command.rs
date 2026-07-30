use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn command_should_report_a_close_spelling_suggestion() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should follow the Unix epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("language-lint-{nonce}"));
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let entry = directory.join("main.reim");
    fs::write(&entry, "fn main() -> i32 { let answer = 42; anser }")
        .expect("fixture should be written");

    let output = Command::new(env!("CARGO_BIN_EXE_reimer-lint"))
        .arg(&entry)
        .output()
        .expect("linter should run");

    assert!(String::from_utf8_lossy(&output.stderr).contains("did you mean `answer`?"));

    fs::remove_dir_all(&directory).expect("fixture directory should be removed");
}

#[test]
fn command_should_resolve_a_manifest_dependency() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should follow the Unix epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("package-lint-{nonce}"));
    let app = directory.join("app");
    let math = directory.join("math");
    fs::create_dir_all(app.join("src")).expect("application source should be created");
    fs::create_dir_all(math.join("src")).expect("dependency source should be created");
    fs::write(
        app.join("reimer.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [dependencies]\nmath = { path = \"../math\" }\n",
    )
    .expect("application manifest should be written");
    let entry = app.join("src/main.reim");
    fs::write(
        &entry,
        "from math import answer;\nfn main() -> i32 { answer() }\n",
    )
    .expect("application source should be written");
    fs::write(
        math.join("reimer.toml"),
        "[package]\nname = \"math\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("dependency manifest should be written");
    fs::write(
        math.join("src/package.reim"),
        "pub fn answer() -> i32 { 42 }\n",
    )
    .expect("dependency facade should be written");

    let output = Command::new(env!("CARGO_BIN_EXE_reimer-lint"))
        .arg(&entry)
        .output()
        .expect("linter should run");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::remove_dir_all(&directory).expect("fixture directory should be removed");
}
