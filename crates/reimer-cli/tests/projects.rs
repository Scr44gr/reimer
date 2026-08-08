use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("package-cli-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent should be created");
        }
        fs::write(path, contents).expect("fixture should be written");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn invoke(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reimer"))
        .args(arguments)
        .output()
        .expect("command should start")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
fn pe_subsystem(path: &Path) -> u16 {
    let image = fs::read(path).expect("PE image should be readable");
    let pe_offset = u32::from_le_bytes(
        image[0x3c..0x40]
            .try_into()
            .expect("DOS header should contain the PE offset"),
    ) as usize;
    assert_eq!(&image[pe_offset..pe_offset + 4], b"PE\0\0");
    let optional_header = pe_offset + 24;
    u16::from_le_bytes(
        image[optional_header + 68..optional_header + 70]
            .try_into()
            .expect("optional header should contain a subsystem"),
    )
}

#[test]
fn doc_should_generate_documented_public_api_for_the_root_package() {
    let fixture = Fixture::new();
    fixture.write(
        "app/reimer.toml",
        &manifest("app", "math = { path = \"../math\", version = \"^0.1\" }\n"),
    );
    fixture.write(
        "app/src/package.reim",
        "from math import answer;\n\
         /// Adds the dependency answer to `value`.\n\
         ///\n\
         /// # Errors\n\
         /// This function does not fail.\n\
         pub fn add_answer(value: i32) -> i32 { value + answer() }\n\
         fn hidden() -> i32 { 0 }\n\
         /// Stores a visible counter value.\n\
         pub struct Counter { pub value: i32, secret: i32 }\n\
         impl Counter {\n\
             /// Reads the current value.\n\
             pub fn get(&self) -> i32 { self.value }\n\
             fn secret(&self) -> i32 { self.secret }\n\
         }\n",
    );
    fixture.write("math/reimer.toml", &manifest("math", ""));
    fixture.write(
        "math/src/package.reim",
        "/// Dependency implementation detail.\npub fn answer() -> i32 { 42 }\n",
    );
    let app = fixture.path("app").display().to_string();

    let output = invoke(&["doc", &app]);
    assert_success(&output);

    let documentation = fs::read_to_string(fixture.path("app/target/reimer/doc/app.md"))
        .expect("generated documentation should be readable");
    assert!(documentation.contains("# app 0.1.0"));
    assert!(documentation.contains("pub fn add_answer(value: i32) -> i32;"));
    assert!(documentation.contains("Adds the dependency answer to `value`."));
    assert!(documentation.contains("pub struct Counter"));
    assert!(documentation.contains("Counter::get"));
    assert!(documentation.contains("Reads the current value."));
    assert!(!documentation.contains("fn hidden"));
    assert!(!documentation.contains("Dependency implementation detail."));
}

fn manifest(name: &str, dependencies: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [dependencies]\n{dependencies}\n\
         [profile.debug]\noptimization = 0\n\n\
         [profile.release]\noptimization = 3\n"
    )
}

#[test]
fn commands_should_compile_and_test_a_transitive_path_graph() {
    let fixture = Fixture::new();
    fixture.write(
        "app/reimer.toml",
        &manifest(
            "app",
            "physics = { path = \"../physics\", version = \"^0.1\" }\n",
        ),
    );
    fixture.write(
        "app/src/main.reim",
        "from physics import combined;\nfn main() -> i32 { combined() }\n",
    );
    fixture.write(
        "app/tests/answer.reim",
        "from physics import combined;\nfn main() -> i32 { combined() - 42 }\n",
    );
    fixture.write(
        "physics/reimer.toml",
        &manifest(
            "physics",
            "vectors = { path = \"../vectors\", version = \"^0.1\" }\n",
        ),
    );
    fixture.write(
        "physics/src/package.reim",
        "from vectors import answer;\npub fn combined() -> i32 { answer() }\n",
    );
    fixture.write("vectors/reimer.toml", &manifest("vectors", ""));
    fixture.write(
        "vectors/src/package.reim",
        "pub fn answer() -> i32 { 42 }\n",
    );
    let app = fixture.path("app").display().to_string();

    assert_success(&invoke(&["check", &app]));
    assert_success(&invoke(&["check", &app, "--locked"]));
    assert_success(&invoke(&["run", &app, "--release", "--locked"]));
    assert_success(&invoke(&["build", &app, "--release", "--locked"]));
    assert_success(&invoke(&["test", &app, "--locked"]));

    assert!(fixture.path("app/reimer.lock").is_file());
    let executable = fixture.path(&format!(
        "app/target/reimer/release/app{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(executable.is_file());
    let status = Command::new(executable)
        .status()
        .expect("built executable should start");
    assert_eq!(status.code(), Some(42));
}

#[test]
fn run_trace_verbosity_should_report_stages_and_exact_sources_on_stderr() {
    let fixture = Fixture::new();
    fixture.write("app/reimer.toml", &manifest("app", ""));
    fixture.write("app/src/main.reim", "fn main() -> i32 { 0 }\n");
    let app = fixture.path("app").display().to_string();

    let output = invoke(&["run", &app, "--verbosity", "trace"]);
    assert_success(&output);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("starting project resolution"));
    assert!(stderr.contains("source 1/"));
    assert!(stderr.contains("app\\src\\main.reim") || stderr.contains("app/src/main.reim"));
    assert!(stderr.contains("finished native code generation"));
    assert!(stderr.contains("eta"));
}

#[test]
fn run_quiet_verbosity_should_suppress_the_command_summary() {
    let fixture = Fixture::new();
    fixture.write("app/reimer.toml", &manifest("app", ""));
    fixture.write("app/src/main.reim", "fn main() -> i32 { 0 }\n");
    let app = fixture.path("app").display().to_string();

    let output = invoke(&["run", &app, "--quiet"]);
    assert_success(&output);

    assert!(output.stdout.is_empty());
}

#[cfg(windows)]
#[test]
fn release_build_should_emit_a_gui_subsystem_when_the_profile_requests_it() {
    let fixture = Fixture::new();
    let configured_manifest = manifest("game", "").replace(
        "[profile.release]\noptimization = 3\n",
        "[profile.release]\noptimization = 3\nwindows-subsystem = \"windows\"\n",
    );
    fixture.write("game/reimer.toml", &configured_manifest);
    fixture.write("game/src/main.reim", "fn main() -> i32 { 0 }\n");
    let game = fixture.path("game").display().to_string();
    let executable = fixture.path("game/target/game.exe");
    let executable_text = executable.display().to_string();

    let output = invoke(&["build", &game, "--release", "--output", &executable_text]);
    assert_success(&output);

    assert_eq!(pe_subsystem(&executable), 2);
}

#[test]
fn locked_check_should_reject_dependency_source_drift() {
    let fixture = Fixture::new();
    fixture.write(
        "app/reimer.toml",
        &manifest("app", "math = { path = \"../math\" }\n"),
    );
    fixture.write(
        "app/src/main.reim",
        "from math import answer;\nfn main() -> i32 { answer() }\n",
    );
    fixture.write("math/reimer.toml", &manifest("math", ""));
    fixture.write("math/src/package.reim", "pub fn answer() -> i32 { 42 }\n");
    let app = fixture.path("app").display().to_string();
    assert_success(&invoke(&["check", &app]));

    fixture.write("math/src/package.reim", "pub fn answer() -> i32 { 43 }\n");
    let output = invoke(&["check", &app, "--locked"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("out of date"));
}

#[test]
fn add_and_remove_should_update_manifest_and_lockfile() {
    let fixture = Fixture::new();
    fixture.write("app/reimer.toml", &manifest("app", ""));
    fixture.write("app/src/main.reim", "fn main() -> i32 { 0 }\n");
    fixture.write("math/reimer.toml", &manifest("math", ""));
    fixture.write("math/src/package.reim", "pub fn answer() -> i32 { 42 }\n");
    let app = fixture.path("app").display().to_string();

    assert_success(&invoke(&[
        "add",
        "math",
        "--path",
        "../math",
        "--project",
        &app,
    ]));
    assert_success(&invoke(&["check", &app, "--locked"]));
    let with_dependency =
        fs::read_to_string(fixture.path("app/reimer.toml")).expect("manifest should be readable");
    assert!(with_dependency.contains("math = { path = \"../math\" }"));

    assert_success(&invoke(&["remove", "math", "--project", &app]));
    assert_success(&invoke(&["check", &app, "--locked"]));
    let without_dependency =
        fs::read_to_string(fixture.path("app/reimer.toml")).expect("manifest should be readable");
    assert!(!without_dependency.contains("math ="));
}

#[test]
fn project_lifecycle_should_create_format_and_clean_safely() {
    let fixture = Fixture::new();
    let package = fixture.path("fresh");
    let package_text = package.display().to_string();
    assert_success(&invoke(&["new", &package_text]));
    assert_success(&invoke(&["build", &package_text]));

    fs::write(
        package.join("src/main.reim"),
        "import z;  \nimport a;\nfn main() -> i32 { 0 }  \n",
    )
    .expect("source should be changed");
    let check = invoke(&["fmt", &package_text, "--check"]);
    assert!(!check.status.success());
    assert_success(&invoke(&["fmt", &package_text]));
    let formatted =
        fs::read_to_string(package.join("src/main.reim")).expect("source should be readable");
    assert_eq!(formatted, "import a;\nimport z;\nfn main() -> i32 { 0 }\n");

    assert_success(&invoke(&["clean", &package_text]));
    assert!(!package.join("target/reimer").exists());

    let initialized = fixture.path("existing");
    fs::create_dir(&initialized).expect("existing directory should be created");
    let initialized_text = initialized.display().to_string();
    assert_success(&invoke(&["init", &initialized_text]));
    assert!(initialized.join("reimer.toml").is_file());
}

#[test]
fn build_should_accept_a_library_package_without_main() {
    let fixture = Fixture::new();
    fixture.write("library/reimer.toml", &manifest("utility", ""));
    fixture.write(
        "library/src/package.reim",
        "pub fn answer() -> i32 { 42 }\n",
    );
    let library = fixture.path("library").display().to_string();

    assert_success(&invoke(&["check", &library]));
    assert_success(&invoke(&["build", &library, "--locked"]));

    let object = if cfg!(windows) {
        "library/target/reimer/debug/utility.obj"
    } else {
        "library/target/reimer/debug/utility.o"
    };
    assert!(fixture.path(object).is_file());
}

#[test]
fn project_paths_should_remain_inside_the_fixture() {
    let fixture = Fixture::new();

    assert!(fixture.root.starts_with(std::env::temp_dir()));
    assert!(fixture.path("app").starts_with(&fixture.root));
    assert_ne!(fixture.root, Path::new("/"));
}
