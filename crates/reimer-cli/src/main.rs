use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use reimer_cli::{
    check_file, check_graph, compile_file_to_object, compile_file_to_object_with_options,
    compile_graph_to_object, execute_file_test, execute_file_with_arguments, execute_graph,
    execute_graph_test, execute_graph_with_arguments, file_test_names, graph_test_names,
};
use reimer_codegen_native::OptimizationLevel;
use reimer_project::{BuildProfile, LockMode, Project};

mod documentation;
mod native_linker;

const MANIFEST_FILE: &str = "reimer.toml";

fn main() -> ExitCode {
    match run(env::args_os().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<OsString>) -> Result<(), String> {
    match Invocation::parse(arguments)? {
        Invocation::Help => {
            println!("{}", usage());
            Ok(())
        }
        Invocation::Version => {
            println!("reimer {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Invocation::New { path } => create_project(&path, CreationMode::New),
        Invocation::Init { path } => create_project(&path, CreationMode::Init),
        Invocation::Check(options) => check(&options),
        Invocation::EmitObject { source, output } => emit_object(&source, output.as_deref()),
        Invocation::Build { options, output } => build(&options, output.as_deref()),
        Invocation::Run { options, arguments } => execute(&options, &arguments),
        Invocation::Test(options) => test(&options),
        Invocation::Document { options, output } => document(&options, output.as_deref()),
        Invocation::RunUnitTest {
            options,
            test_index,
        } => run_unit_test(&options, test_index),
        Invocation::Format { path, check } => format_sources(&path, check),
        Invocation::Clean { path } => clean(&path),
        Invocation::Add(options) => add_dependency(&options),
        Invocation::Remove { alias, project } => remove_dependency(&project, &alias),
    }
}

fn check(options: &ProjectOptions) -> Result<(), String> {
    if is_source_file(&options.path) {
        check_file(&options.path).map_err(|diagnostics| render_diagnostics(&diagnostics))?;
        println!("checked {}", options.path.display());
        return Ok(());
    }

    let project = open_project(options)?;
    let entry = project.entry().map_err(|error| error.to_string())?;
    check_graph(&project.source_graph(&entry))
        .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
    println!(
        "checked {} {}",
        project.package_name(),
        display_version(&project)
    );
    Ok(())
}

fn document(options: &ProjectOptions, output: Option<&Path>) -> Result<(), String> {
    if is_source_file(&options.path) {
        validate_source_path(&options.path)?;
        let package = reimer_package::load(&options.path)
            .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
        reimer_resolver::resolve_library(&package.program)
            .map_err(|diagnostics| render_diagnostics(&package.map_diagnostics(diagnostics)))?;
        let source_root = options.path.parent().unwrap_or_else(|| Path::new("."));
        let title = options
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Reimer package");
        let rendered = documentation::render(&package, source_root, title);
        let output = output.map_or_else(
            || {
                source_root
                    .join("target")
                    .join("reimer")
                    .join("doc")
                    .join(format!("{title}.md"))
            },
            Path::to_path_buf,
        );
        write_output(&output, rendered.as_bytes())?;
        println!("documented {title}\noutput: {}", output.display());
        return Ok(());
    }

    let project = open_project(options)?;
    let entry = project.entry().map_err(|error| error.to_string())?;
    let graph = project.source_graph(&entry);
    let package = reimer_package::load_graph(&graph)
        .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
    let resolved = if entry.file_name().and_then(|name| name.to_str()) == Some("package.reim") {
        reimer_resolver::resolve_library(&package.program)
    } else {
        reimer_resolver::resolve(&package.program)
    };
    resolved.map_err(|diagnostics| render_diagnostics(&package.map_diagnostics(diagnostics)))?;
    let title = format!("{} {}", project.package_name(), display_version(&project));
    let rendered = documentation::render(&package, &project.root_directory().join("src"), &title);
    let output = output.map_or_else(
        || {
            project
                .root_directory()
                .join("target")
                .join("reimer")
                .join("doc")
                .join(format!("{}.md", project.package_name()))
        },
        Path::to_path_buf,
    );
    write_output(&output, rendered.as_bytes())?;
    println!(
        "documented {} {}\noutput: {}",
        project.package_name(),
        display_version(&project),
        output.display()
    );
    Ok(())
}

fn emit_object(source: &Path, output: Option<&Path>) -> Result<(), String> {
    validate_source_path(source)?;
    let object =
        compile_file_to_object(source).map_err(|diagnostics| render_diagnostics(&diagnostics))?;
    let output = output.map_or_else(
        || source.with_extension(object_extension()),
        Path::to_path_buf,
    );
    write_output(&output, &object)?;
    println!("emitted {}", output.display());
    Ok(())
}

fn build(options: &ProjectOptions, output: Option<&Path>) -> Result<(), String> {
    if is_source_file(&options.path) {
        validate_source_path(&options.path)?;
        let object = compile_file_to_object_with_options(
            &options.path,
            profile_optimization(options.profile),
        )
        .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
        if is_library_source(&options.path) {
            let output = output.map_or_else(
                || default_source_object_output(&options.path),
                Path::to_path_buf,
            );
            write_output(&output, &object)?;
            println!("emitted library object {}", output.display());
            return Ok(());
        }
        let output = output.map_or_else(
            || default_source_executable_output(&options.path),
            Path::to_path_buf,
        );
        let artifact_directory = source_artifact_directory(&options.path, options.profile);
        let object_path = native_linker::link_executable(&object, &output, &artifact_directory)?;
        println!(
            "built executable {}\nobject: {}",
            output.display(),
            object_path.display()
        );
        return Ok(());
    }

    let project = open_project(options)?;
    let entry = project.entry().map_err(|error| error.to_string())?;
    let optimization = selected_optimization(&project, options.profile)?;
    let object = compile_graph_to_object(&project.source_graph(&entry), optimization)
        .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
    if is_library_source(&entry) {
        let output = output.map_or_else(
            || default_project_object_output(&project, options.profile),
            Path::to_path_buf,
        );
        write_output(&output, &object)?;
        println!(
            "built {} {} ({})\nobject: {}\nlockfile: {}",
            project.package_name(),
            display_version(&project),
            profile_name(options.profile),
            output.display(),
            project.lock_path().display()
        );
        return Ok(());
    }
    let output = output.map_or_else(
        || default_project_executable_output(&project, options.profile),
        Path::to_path_buf,
    );
    let artifact_directory = project_artifact_directory(&project, options.profile);
    let object_path = native_linker::link_executable(&object, &output, &artifact_directory)?;
    println!(
        "built {} {} ({})\nexecutable: {}\nobject: {}\nlockfile: {}",
        project.package_name(),
        display_version(&project),
        profile_name(options.profile),
        output.display(),
        object_path.display(),
        project.lock_path().display()
    );
    Ok(())
}

fn execute(options: &ProjectOptions, arguments: &[OsString]) -> Result<(), String> {
    if is_source_file(&options.path) {
        validate_source_path(&options.path)?;
        let result = execute_file_with_arguments(
            &options.path,
            profile_optimization(options.profile),
            runtime_arguments(&options.path, arguments),
        )
        .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
        println!("program returned {result}");
        return Ok(());
    }

    let project = open_project(options)?;
    let entry = project.entry().map_err(|error| error.to_string())?;
    let optimization = selected_optimization(&project, options.profile)?;
    let result = execute_graph_with_arguments(
        &project.source_graph(&entry),
        optimization,
        runtime_arguments(&entry, arguments),
    )
    .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
    println!("program returned {result}");
    Ok(())
}

fn runtime_arguments(entry: &Path, arguments: &[OsString]) -> Vec<OsString> {
    let mut runtime_arguments = Vec::with_capacity(arguments.len() + 1);
    runtime_arguments.push(entry.as_os_str().to_owned());
    runtime_arguments.extend_from_slice(arguments);
    runtime_arguments
}

fn test(options: &ProjectOptions) -> Result<(), String> {
    let project = if is_source_file(&options.path) {
        None
    } else {
        Some(open_project(options)?)
    };
    let (entries, unit_tests, optimization) = if let Some(project) = &project {
        let entries = project.test_entries().map_err(|error| error.to_string())?;
        let entry = project.entry().map_err(|error| error.to_string())?;
        let tests = graph_test_names(&project.source_graph(&entry))
            .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
        let optimization = selected_optimization(project, options.profile)?;
        (entries, tests, optimization)
    } else {
        validate_source_path(&options.path)?;
        let tests = file_test_names(&options.path)
            .map_err(|diagnostics| render_diagnostics(&diagnostics))?;
        (Vec::new(), tests, profile_optimization(options.profile))
    };
    if entries.is_empty() && unit_tests.is_empty() {
        println!("no tests found");
        return Ok(());
    }

    let mut failures = Vec::new();
    for entry in &entries {
        let project = project
            .as_ref()
            .ok_or_else(|| "integration test has no containing project".to_owned())?;
        match execute_graph(&project.source_graph(entry), optimization) {
            Ok(0) => println!("pass {}", entry.display()),
            Ok(code) => {
                println!("fail {} (returned {code})", entry.display());
                failures.push(format!("{} returned {code}", entry.display()));
            }
            Err(diagnostics) => {
                println!("fail {} (did not compile)", entry.display());
                failures.push(render_diagnostics(&diagnostics));
            }
        }
    }
    for (test_index, name) in unit_tests.iter().enumerate() {
        let status = spawn_unit_test(options, test_index)?;
        if status.success() {
            println!("pass {name}");
        } else {
            println!("fail {name} ({status})");
            failures.push(format!("{name} terminated with {status}"));
        }
    }
    let total = entries.len() + unit_tests.len();
    if failures.is_empty() {
        println!("{total} test(s) passed");
        Ok(())
    } else {
        Err(format!(
            "{} integration test(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        ))
    }
}

fn run_unit_test(options: &ProjectOptions, test_index: usize) -> Result<(), String> {
    if is_source_file(&options.path) {
        validate_source_path(&options.path)?;
        return execute_file_test(
            &options.path,
            test_index,
            profile_optimization(options.profile),
        )
        .map_err(|diagnostics| render_diagnostics(&diagnostics));
    }
    let project = open_project(options)?;
    let entry = project.entry().map_err(|error| error.to_string())?;
    let optimization = selected_optimization(&project, options.profile)?;
    execute_graph_test(&project.source_graph(&entry), test_index, optimization)
        .map_err(|diagnostics| render_diagnostics(&diagnostics))
}

fn spawn_unit_test(
    options: &ProjectOptions,
    test_index: usize,
) -> Result<std::process::ExitStatus, String> {
    let executable =
        env::current_exe().map_err(|error| format!("failed to locate the test runner: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg("__run-unit-test")
        .arg(test_index.to_string())
        .arg(&options.path);
    if options.profile == BuildProfile::Release {
        command.arg("--release");
    }
    match options.lock_mode {
        LockMode::Use => {}
        LockMode::Locked => {
            command.arg("--locked");
        }
        LockMode::Refresh => {
            command.arg("--refresh");
        }
    }
    command
        .status()
        .map_err(|error| format!("failed to start isolated unit test: {error}"))
}

fn profile_optimization(profile: BuildProfile) -> OptimizationLevel {
    match profile {
        BuildProfile::Debug => OptimizationLevel::None,
        BuildProfile::Release => OptimizationLevel::Speed,
    }
}

fn format_sources(path: &Path, check: bool) -> Result<(), String> {
    let files = if is_source_file(path) {
        validate_source_path(path)?;
        vec![path.to_path_buf()]
    } else {
        let project = Project::open(path, LockMode::Use).map_err(|error| error.to_string())?;
        collect_project_sources(project.root_directory())?
    };

    let mut changed = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file)
            .map_err(|error| format!("failed to read `{}`: {error}", file.display()))?;
        let formatted = format_source(file, &source)?;
        if formatted != source {
            changed.push(file.clone());
            if !check {
                fs::write(file, formatted)
                    .map_err(|error| format!("failed to write `{}`: {error}", file.display()))?;
            }
        }
    }

    if check && !changed.is_empty() {
        let paths = changed
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "{} file(s) need formatting:\n{paths}",
            changed.len()
        ));
    }
    if check {
        println!("{} file(s) are formatted", files.len());
    } else {
        println!(
            "formatted {} file(s), changed {}",
            files.len(),
            changed.len()
        );
    }
    Ok(())
}

fn format_source(path: &Path, source: &str) -> Result<String, String> {
    let tokens = reimer_lexer::lex(source)
        .map_err(|diagnostics| render_source_diagnostics(path, source, &diagnostics))?;
    let syntax = reimer_parser::parse(&tokens)
        .map_err(|diagnostics| render_source_diagnostics(path, source, &diagnostics))?;
    let mut formatted = source.to_owned();
    if let Some(fix) = reimer_lint::organize_imports(source, &syntax) {
        formatted.replace_range(fix.span.start..fix.span.end, &fix.replacement);
    }

    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut normalized = formatted
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join(newline);
    normalized.push_str(newline);
    Ok(normalized)
}

fn clean(path: &Path) -> Result<(), String> {
    let project = Project::open(path, LockMode::Use).map_err(|error| error.to_string())?;
    let root = project.root_directory().canonicalize().map_err(|error| {
        format!(
            "failed to resolve `{}`: {error}",
            project.root_directory().display()
        )
    })?;
    let target = root.join("target").join("reimer");
    if !target.starts_with(&root) {
        return Err(format!(
            "refusing to clean path outside project: `{}`",
            target.display()
        ));
    }
    if target.exists() {
        fs::remove_dir_all(&target)
            .map_err(|error| format!("failed to remove `{}`: {error}", target.display()))?;
        println!("removed {}", target.display());
    } else {
        println!("nothing to clean");
    }
    Ok(())
}

fn add_dependency(options: &AddOptions) -> Result<(), String> {
    validate_alias(&options.alias)?;
    let project =
        Project::open(&options.project, LockMode::Use).map_err(|error| error.to_string())?;
    let manifest = project.manifest_path();
    let original = fs::read_to_string(manifest)
        .map_err(|error| format!("failed to read `{}`: {error}", manifest.display()))?;
    let line = render_dependency(options)?;
    let updated = insert_dependency(&original, &options.alias, &line)?;
    update_manifest(manifest, &original, &updated)?;
    println!("added dependency `{}`", options.alias);
    Ok(())
}

fn remove_dependency(project_path: &Path, alias: &str) -> Result<(), String> {
    validate_alias(alias)?;
    let project = Project::open(project_path, LockMode::Use).map_err(|error| error.to_string())?;
    let manifest = project.manifest_path();
    let original = fs::read_to_string(manifest)
        .map_err(|error| format!("failed to read `{}`: {error}", manifest.display()))?;
    let updated = delete_dependency(&original, alias)?;
    update_manifest(manifest, &original, &updated)?;
    println!("removed dependency `{alias}`");
    Ok(())
}

fn update_manifest(path: &Path, original: &str, updated: &str) -> Result<(), String> {
    fs::write(path, updated)
        .map_err(|error| format!("failed to update `{}`: {error}", path.display()))?;
    if let Err(error) = Project::open(path, LockMode::Refresh) {
        let restoration = fs::write(path, original);
        return match restoration {
            Ok(()) => Err(format!(
                "dependency update was rejected and the manifest was restored: {error}"
            )),
            Err(restore_error) => Err(format!(
                "dependency update failed: {error}; restoring `{}` also failed: {restore_error}",
                path.display()
            )),
        };
    }
    Ok(())
}

fn insert_dependency(source: &str, alias: &str, line: &str) -> Result<String, String> {
    if let Some((start, end)) = dependency_section(source) {
        let section = &source[start..end];
        if section
            .lines()
            .filter_map(dependency_key)
            .any(|key| key == alias)
        {
            return Err(format!("dependency `{alias}` already exists"));
        }
        let insertion_at = start.saturating_add(section.trim_end().len());
        let mut insertion = String::new();
        if start < insertion_at && !source[..insertion_at].ends_with('\n') {
            insertion.push('\n');
        }
        insertion.push_str(line);
        insertion.push('\n');
        let mut updated = source.to_owned();
        updated.insert_str(insertion_at, &insertion);
        return Ok(updated);
    }

    let mut updated = source.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() && !updated.ends_with("\n\n") {
        updated.push('\n');
    }
    updated.push_str("[dependencies]\n");
    updated.push_str(line);
    updated.push('\n');
    Ok(updated)
}

fn delete_dependency(source: &str, alias: &str) -> Result<String, String> {
    let Some((start, end)) = dependency_section(source) else {
        return Err(format!("dependency `{alias}` does not exist"));
    };
    let section = &source[start..end];
    let mut offset = start;
    for line in section.split_inclusive('\n') {
        if dependency_key(line) == Some(alias) {
            let mut updated = source.to_owned();
            updated.replace_range(offset..offset + line.len(), "");
            return Ok(updated);
        }
        offset = offset.saturating_add(line.len());
    }
    if !section.ends_with('\n')
        && dependency_key(section.rsplit('\n').next().unwrap_or(section)) == Some(alias)
    {
        let line_start = source[..end]
            .rfind('\n')
            .map_or(start, |position| position.saturating_add(1));
        let mut updated = source.to_owned();
        updated.replace_range(line_start..end, "");
        return Ok(updated);
    }
    Err(format!("dependency `{alias}` does not exist"))
}

fn dependency_section(source: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut body_start = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if body_start.is_some() && trimmed.starts_with('[') && trimmed.ends_with(']') {
            return body_start.map(|start| (start, offset));
        }
        if trimmed == "[dependencies]" {
            body_start = Some(offset.saturating_add(line.len()));
        }
        offset = offset.saturating_add(line.len());
    }
    body_start.map(|start| (start, source.len()))
}

fn dependency_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    Some(key.trim())
}

fn render_dependency(options: &AddOptions) -> Result<String, String> {
    let mut fields = match &options.source {
        AddSource::Path(path) => vec![format!("path = {}", quote_toml(&path.to_string_lossy())?)],
        AddSource::Git(repository, selector) => {
            let mut fields = vec![format!("git = {}", quote_toml(repository)?)];
            if let Some(selector) = selector {
                let (name, value) = match selector {
                    GitSelector::Revision(value) => ("rev", value),
                    GitSelector::Branch(value) => ("branch", value),
                    GitSelector::Tag(value) => ("tag", value),
                };
                fields.push(format!("{name} = {}", quote_toml(value)?));
            }
            fields
        }
    };
    if let Some(package) = &options.package {
        fields.push(format!("package = {}", quote_toml(package)?));
    }
    if let Some(version) = &options.version {
        fields.push(format!("version = {}", quote_toml(version)?));
    }
    Ok(format!("{} = {{ {} }}", options.alias, fields.join(", ")))
}

fn quote_toml(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("dependency values cannot contain control characters".to_owned());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn create_project(path: &Path, mode: CreationMode) -> Result<(), String> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    if mode == CreationMode::New {
        if path.exists() {
            return Err(format!("destination `{}` already exists", path.display()));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
        }
        fs::create_dir(path)
            .map_err(|error| format!("failed to create `{}`: {error}", path.display()))?;
    } else {
        fs::create_dir_all(path)
            .map_err(|error| format!("failed to create `{}`: {error}", path.display()))?;
    }

    let package = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("app");
    validate_package_name(package)?;

    for directory in ["src", "tests", "examples", "assets"] {
        let directory = path.join(directory);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("failed to create `{}`: {error}", directory.display()))?;
    }
    let manifest = format!(
        "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n\
         [dependencies]\n\n\
         [profile.debug]\noptimization = 0\n\n\
         [profile.release]\noptimization = 3\n"
    );
    create_file(&path.join(MANIFEST_FILE), manifest.as_bytes())?;
    create_file(
        &path.join("src").join("main.reim"),
        b"fn main() -> i32 {\n    0\n}\n",
    )?;
    println!("created package `{package}` at {}", path.display());
    Ok(())
}

fn create_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("failed to create `{}`: {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))
}

fn open_project(options: &ProjectOptions) -> Result<Project, String> {
    Project::open(&options.path, options.lock_mode).map_err(|error| error.to_string())
}

fn selected_optimization(
    project: &Project,
    profile: BuildProfile,
) -> Result<OptimizationLevel, String> {
    match project.optimization(profile) {
        0 => Ok(OptimizationLevel::None),
        1 | 2 => Ok(OptimizationLevel::Speed),
        3 => Ok(OptimizationLevel::SpeedAndSize),
        value => Err(format!(
            "profile optimization `{value}` cannot be represented by the native backend"
        )),
    }
}

fn collect_project_sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for directory in ["src", "tests", "examples"] {
        collect_source_files(&root.join(directory), &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect_source_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to read `{}`: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to read `{}`: {error}", directory.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("failed to inspect `{}`: {error}", path.display()))?;
        if kind.is_dir() {
            collect_source_files(&path, files)?;
        } else if kind.is_file() && is_source_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn render_diagnostics(diagnostics: &[reimer_package::FileDiagnostic]) -> String {
    diagnostics
        .iter()
        .map(reimer_package::FileDiagnostic::render)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_source_diagnostics(
    path: &Path,
    source: &str,
    diagnostics: &[reimer_diagnostics::Diagnostic],
) -> String {
    let name = path.display().to_string();
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.render(&name, source))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_source_path(path: &Path) -> Result<(), String> {
    if !is_source_file(path) {
        return Err(format!(
            "expected a `.reim` source file, found `{}`",
            path.display()
        ));
    }
    Ok(())
}

fn is_source_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("reim")
}

fn validate_package_name(name: &str) -> Result<(), String> {
    let mut characters = name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic());
    let valid_rest = characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    if valid_start && valid_rest {
        Ok(())
    } else {
        Err(format!("invalid package name `{name}`"))
    }
}

fn validate_alias(alias: &str) -> Result<(), String> {
    let mut characters = alias.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest =
        characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if valid_start && valid_rest && !matches!(alias, "self" | "super" | "std") {
        Ok(())
    } else {
        Err(format!("invalid dependency alias `{alias}`"))
    }
}

fn write_output(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
    }
    fs::write(path, contents)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))
}

fn default_source_object_output(source: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or(source.as_os_str()).to_owned();
    PathBuf::from("target")
        .join("reimer")
        .join(stem)
        .with_extension(object_extension())
}

fn default_source_executable_output(source: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or(source.as_os_str()).to_owned();
    executable_path(PathBuf::from("target").join("reimer").join(stem))
}

fn source_artifact_directory(source: &Path, profile: BuildProfile) -> PathBuf {
    let stem = source.file_stem().unwrap_or(source.as_os_str()).to_owned();
    PathBuf::from("target")
        .join("reimer")
        .join("artifacts")
        .join(profile_name(profile))
        .join(stem)
}

fn default_project_object_output(project: &Project, profile: BuildProfile) -> PathBuf {
    project
        .root_directory()
        .join("target")
        .join("reimer")
        .join(profile_name(profile))
        .join(project.package_name())
        .with_extension(object_extension())
}

fn default_project_executable_output(project: &Project, profile: BuildProfile) -> PathBuf {
    executable_path(
        project
            .root_directory()
            .join("target")
            .join("reimer")
            .join(profile_name(profile))
            .join(project.package_name()),
    )
}

fn project_artifact_directory(project: &Project, profile: BuildProfile) -> PathBuf {
    project
        .root_directory()
        .join("target")
        .join("reimer")
        .join("artifacts")
        .join(profile_name(profile))
        .join(project.package_name())
}

fn executable_path(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        path.with_extension("exe")
    } else {
        path
    }
}

fn is_library_source(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("package.reim")
}

const fn object_extension() -> &'static str {
    if cfg!(windows) { "obj" } else { "o" }
}

const fn profile_name(profile: BuildProfile) -> &'static str {
    match profile {
        BuildProfile::Debug => "debug",
        BuildProfile::Release => "release",
    }
}

fn display_version(project: &Project) -> String {
    project
        .package_version()
        .map_or_else(|| "<unknown>".to_owned(), ToString::to_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationMode {
    New,
    Init,
}

#[derive(Debug)]
enum Invocation {
    Help,
    Version,
    New {
        path: PathBuf,
    },
    Init {
        path: PathBuf,
    },
    Check(ProjectOptions),
    EmitObject {
        source: PathBuf,
        output: Option<PathBuf>,
    },
    Build {
        options: ProjectOptions,
        output: Option<PathBuf>,
    },
    Run {
        options: ProjectOptions,
        arguments: Vec<OsString>,
    },
    Test(ProjectOptions),
    Document {
        options: ProjectOptions,
        output: Option<PathBuf>,
    },
    RunUnitTest {
        options: ProjectOptions,
        test_index: usize,
    },
    Format {
        path: PathBuf,
        check: bool,
    },
    Clean {
        path: PathBuf,
    },
    Add(AddOptions),
    Remove {
        alias: String,
        project: PathBuf,
    },
}

impl Invocation {
    fn parse(arguments: Vec<OsString>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let Some(command) = arguments.next() else {
            return Ok(Self::Help);
        };
        let command = command
            .into_string()
            .map_err(|_| format!("command is not valid Unicode\n\n{}", usage()))?;
        match command.as_str() {
            "help" | "-h" | "--help" => Ok(Self::Help),
            "-V" | "--version" => Ok(Self::Version),
            "new" => Ok(Self::New {
                path: parse_required_path(arguments, "destination")?,
            }),
            "init" => Ok(Self::Init {
                path: parse_optional_path(arguments, Path::new("."))?,
            }),
            "check" => Ok(Self::Check(parse_project_options(arguments, false)?.0)),
            "emit-object" => {
                let (source, output) = parse_source_output(arguments)?;
                Ok(Self::EmitObject { source, output })
            }
            "build" => {
                let (options, output) = parse_project_options(arguments, true)?;
                Ok(Self::Build { options, output })
            }
            "run" => parse_run(arguments),
            "test" => Ok(Self::Test(parse_project_options(arguments, false)?.0)),
            "doc" => {
                let (options, output) = parse_project_options(arguments, true)?;
                Ok(Self::Document { options, output })
            }
            "__run-unit-test" => parse_unit_test(arguments),
            "fmt" => parse_format(arguments),
            "clean" => Ok(Self::Clean {
                path: parse_optional_path(arguments, Path::new("."))?,
            }),
            "add" => parse_add(arguments),
            "remove" => parse_remove(arguments),
            _ => Err(format!("unknown command `{command}`\n\n{}", usage())),
        }
    }
}

fn parse_run(arguments: impl Iterator<Item = OsString>) -> Result<Invocation, String> {
    let mut compiler_arguments = Vec::new();
    let mut program_arguments = Vec::new();
    let mut after_separator = false;
    for argument in arguments {
        if !after_separator && argument == "--" {
            after_separator = true;
        } else if after_separator {
            program_arguments.push(argument);
        } else {
            compiler_arguments.push(argument);
        }
    }
    let (options, _) = parse_project_options(compiler_arguments.into_iter(), false)?;
    Ok(Invocation::Run {
        options,
        arguments: program_arguments,
    })
}

fn parse_unit_test(mut arguments: impl Iterator<Item = OsString>) -> Result<Invocation, String> {
    let index = arguments
        .next()
        .ok_or_else(|| "isolated unit-test index is missing".to_owned())?
        .into_string()
        .map_err(|_| "isolated unit-test index is not valid Unicode".to_owned())?
        .parse::<usize>()
        .map_err(|error| format!("invalid isolated unit-test index: {error}"))?;
    let (options, _) = parse_project_options(arguments, false)?;
    Ok(Invocation::RunUnitTest {
        options,
        test_index: index,
    })
}

#[derive(Debug)]
struct ProjectOptions {
    path: PathBuf,
    lock_mode: LockMode,
    profile: BuildProfile,
}

fn parse_project_options(
    arguments: impl Iterator<Item = OsString>,
    accept_output: bool,
) -> Result<(ProjectOptions, Option<PathBuf>), String> {
    let mut path = None;
    let mut output = None;
    let mut lock_mode = LockMode::Use;
    let mut lock_flag = None;
    let mut profile = BuildProfile::Debug;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--locked") => {
                set_lock_mode(&mut lock_mode, &mut lock_flag, LockMode::Locked, "--locked")?;
            }
            Some("--refresh") => {
                set_lock_mode(
                    &mut lock_mode,
                    &mut lock_flag,
                    LockMode::Refresh,
                    "--refresh",
                )?;
            }
            Some("--release") => profile = BuildProfile::Release,
            Some("--debug") => profile = BuildProfile::Debug,
            Some("-o" | "--output") if accept_output => {
                output = Some(next_path(&mut arguments, "--output")?);
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option `{value}`\n\n{}", usage()));
            }
            _ if path.is_none() => path = Some(PathBuf::from(argument)),
            _ => {
                return Err(format!(
                    "unexpected argument `{}`\n\n{}",
                    argument.to_string_lossy(),
                    usage()
                ));
            }
        }
    }
    Ok((
        ProjectOptions {
            path: path.unwrap_or_else(|| PathBuf::from(".")),
            lock_mode,
            profile,
        },
        output,
    ))
}

fn set_lock_mode(
    mode: &mut LockMode,
    previous: &mut Option<&'static str>,
    next: LockMode,
    flag: &'static str,
) -> Result<(), String> {
    if let Some(previous) = previous
        && *previous != flag
    {
        return Err(format!("`{previous}` and `{flag}` cannot be used together"));
    }
    *mode = next;
    *previous = Some(flag);
    Ok(())
}

fn parse_source_output(
    arguments: impl Iterator<Item = OsString>,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let mut source = None;
    let mut output = None;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("-o" | "--output") => output = Some(next_path(&mut arguments, "--output")?),
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option `{value}`\n\n{}", usage()));
            }
            _ if source.is_none() => source = Some(PathBuf::from(argument)),
            _ => {
                return Err(format!(
                    "unexpected argument `{}`\n\n{}",
                    argument.to_string_lossy(),
                    usage()
                ));
            }
        }
    }
    let source = source.ok_or_else(|| format!("missing source path\n\n{}", usage()))?;
    Ok((source, output))
}

fn parse_format(arguments: impl Iterator<Item = OsString>) -> Result<Invocation, String> {
    let mut path = None;
    let mut check = false;
    for argument in arguments {
        match argument.to_str() {
            Some("--check") => check = true,
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option `{value}`\n\n{}", usage()));
            }
            _ if path.is_none() => path = Some(PathBuf::from(argument)),
            _ => {
                return Err(format!(
                    "unexpected argument `{}`\n\n{}",
                    argument.to_string_lossy(),
                    usage()
                ));
            }
        }
    }
    Ok(Invocation::Format {
        path: path.unwrap_or_else(|| PathBuf::from(".")),
        check,
    })
}

#[derive(Debug)]
struct AddOptions {
    alias: String,
    project: PathBuf,
    source: AddSource,
    package: Option<String>,
    version: Option<String>,
}

#[derive(Debug)]
enum AddSource {
    Path(PathBuf),
    Git(String, Option<GitSelector>),
}

#[derive(Debug)]
enum GitSelector {
    Revision(String),
    Branch(String),
    Tag(String),
}

fn parse_add(arguments: impl Iterator<Item = OsString>) -> Result<Invocation, String> {
    let mut alias = None;
    let mut project = PathBuf::from(".");
    let mut path = None;
    let mut git = None;
    let mut package = None;
    let mut version = None;
    let mut selector = None;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--project") => project = next_path(&mut arguments, "--project")?,
            Some("--path") => path = Some(next_path(&mut arguments, "--path")?),
            Some("--git") => git = Some(next_string(&mut arguments, "--git")?),
            Some("--package") => package = Some(next_string(&mut arguments, "--package")?),
            Some("--version") => version = Some(next_string(&mut arguments, "--version")?),
            Some("--rev") => {
                set_selector(
                    &mut selector,
                    GitSelector::Revision(next_string(&mut arguments, "--rev")?),
                )?;
            }
            Some("--branch") => {
                set_selector(
                    &mut selector,
                    GitSelector::Branch(next_string(&mut arguments, "--branch")?),
                )?;
            }
            Some("--tag") => {
                set_selector(
                    &mut selector,
                    GitSelector::Tag(next_string(&mut arguments, "--tag")?),
                )?;
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option `{value}`\n\n{}", usage()));
            }
            _ if alias.is_none() => {
                alias = Some(
                    argument
                        .into_string()
                        .map_err(|_| "dependency alias is not valid Unicode".to_owned())?,
                );
            }
            _ => {
                return Err(format!(
                    "unexpected argument `{}`\n\n{}",
                    argument.to_string_lossy(),
                    usage()
                ));
            }
        }
    }
    let alias = alias.ok_or_else(|| format!("missing dependency alias\n\n{}", usage()))?;
    let source = match (path, git) {
        (Some(path), None) if selector.is_none() => AddSource::Path(path),
        (None, Some(repository)) => AddSource::Git(repository, selector),
        (Some(_), Some(_)) => return Err("`--path` and `--git` cannot be used together".to_owned()),
        (Some(_), None) => return Err("Git selectors require `--git`".to_owned()),
        (None, None) => {
            return Err("`add` requires either `--path <directory>` or `--git <url>`".to_owned());
        }
    };
    Ok(Invocation::Add(AddOptions {
        alias,
        project,
        source,
        package,
        version,
    }))
}

fn set_selector(selector: &mut Option<GitSelector>, value: GitSelector) -> Result<(), String> {
    if selector.is_some() {
        return Err("only one of `--rev`, `--branch`, or `--tag` may be used".to_owned());
    }
    *selector = Some(value);
    Ok(())
}

fn parse_remove(arguments: impl Iterator<Item = OsString>) -> Result<Invocation, String> {
    let mut alias = None;
    let mut project = PathBuf::from(".");
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--project") => project = next_path(&mut arguments, "--project")?,
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option `{value}`\n\n{}", usage()));
            }
            _ if alias.is_none() => {
                alias = Some(
                    argument
                        .into_string()
                        .map_err(|_| "dependency alias is not valid Unicode".to_owned())?,
                );
            }
            _ => {
                return Err(format!(
                    "unexpected argument `{}`\n\n{}",
                    argument.to_string_lossy(),
                    usage()
                ));
            }
        }
    }
    Ok(Invocation::Remove {
        alias: alias.ok_or_else(|| format!("missing dependency alias\n\n{}", usage()))?,
        project,
    })
}

fn parse_required_path(
    mut arguments: impl Iterator<Item = OsString>,
    role: &str,
) -> Result<PathBuf, String> {
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {role}\n\n{}", usage()))?;
    if let Some(extra) = arguments.next() {
        return Err(format!(
            "unexpected argument `{}`\n\n{}",
            extra.to_string_lossy(),
            usage()
        ));
    }
    Ok(path)
}

fn parse_optional_path(
    mut arguments: impl Iterator<Item = OsString>,
    default: &Path,
) -> Result<PathBuf, String> {
    let path = arguments
        .next()
        .map_or_else(|| default.to_path_buf(), PathBuf::from);
    if let Some(extra) = arguments.next() {
        return Err(format!(
            "unexpected argument `{}`\n\n{}",
            extra.to_string_lossy(),
            usage()
        ));
    }
    Ok(path)
}

fn next_path(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing path after `{flag}`"))
}

fn next_string(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("missing value after `{flag}`"))?
        .into_string()
        .map_err(|_| format!("value after `{flag}` is not valid Unicode"))
}

fn usage() -> &'static str {
    "usage:\n  \
     reimer [--help|--version]\n  \
     reimer new <path>\n  \
     reimer init [path]\n  \
     reimer check [path] [--locked|--refresh]\n  \
     reimer build [path] [--release] [--locked|--refresh] [-o <executable>]\n  \
     reimer run [path] [--release] [--locked|--refresh] [-- <arguments>...]\n  \
     reimer test [path] [--release] [--locked|--refresh]\n  \
     reimer doc [path] [--locked|--refresh] [-o <file.md>]\n  \
     reimer fmt [path] [--check]\n  \
     reimer clean [path]\n  \
     reimer add <alias> (--path <path>|--git <url>) [--package <name>] [--version <req>]\n  \
     reimer remove <alias> [--project <path>]\n  \
     reimer emit-object <file.reim> [-o <file.obj>]"
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{
        AddSource, BuildProfile, Invocation, LockMode, dependency_section, insert_dependency,
    };

    #[test]
    fn invocation_should_parse_help_flag() {
        let invocation =
            Invocation::parse(vec![OsString::from("--help")]).expect("help flag should parse");

        assert!(matches!(invocation, Invocation::Help));
    }

    #[test]
    fn invocation_should_parse_version_flag() {
        let invocation = Invocation::parse(vec![OsString::from("--version")])
            .expect("version flag should parse");

        assert!(matches!(invocation, Invocation::Version));
    }

    #[test]
    fn invocation_should_parse_project_build_options() {
        let arguments = ["build", "demo", "--release", "--locked", "-o", "demo.exe"]
            .map(OsString::from)
            .to_vec();

        let invocation = Invocation::parse(arguments).expect("fixture should parse");

        let Invocation::Build { options, output } = invocation else {
            panic!("expected build invocation");
        };
        assert_eq!(options.path, Path::new("demo"));
        assert_eq!(options.profile, BuildProfile::Release);
        assert_eq!(options.lock_mode, LockMode::Locked);
        assert_eq!(output, Some(PathBuf::from("demo.exe")));
    }

    #[test]
    fn invocation_should_forward_run_arguments_after_separator() {
        let arguments = ["run", "demo", "--release", "--", "--name", "Ada"]
            .map(OsString::from)
            .to_vec();

        let invocation = Invocation::parse(arguments).expect("run invocation should parse");

        let Invocation::Run { options, arguments } = invocation else {
            panic!("expected run invocation");
        };
        assert_eq!(options.path, Path::new("demo"));
        assert_eq!(options.profile, BuildProfile::Release);
        assert_eq!(arguments, [OsString::from("--name"), OsString::from("Ada")]);
    }

    #[test]
    fn invocation_should_parse_documentation_output() {
        let arguments = ["doc", "demo", "--locked", "-o", "api.md"]
            .map(OsString::from)
            .to_vec();

        let invocation = Invocation::parse(arguments).expect("fixture should parse");

        let Invocation::Document { options, output } = invocation else {
            panic!("expected documentation invocation");
        };
        assert_eq!(options.path, Path::new("demo"));
        assert_eq!(options.lock_mode, LockMode::Locked);
        assert_eq!(output, Some(PathBuf::from("api.md")));
    }

    #[test]
    fn invocation_should_parse_path_dependency() {
        let arguments = ["add", "math", "--path", "../math"]
            .map(OsString::from)
            .to_vec();

        let invocation = Invocation::parse(arguments).expect("fixture should parse");

        let Invocation::Add(options) = invocation else {
            panic!("expected add invocation");
        };
        assert!(matches!(options.source, AddSource::Path(path) if path == Path::new("../math")));
    }

    #[test]
    fn insert_dependency_should_preserve_other_manifest_sections() {
        let source =
            "[package]\nname = \"app\"\n\n[dependencies]\n\n[profile.debug]\noptimization = 0\n";

        let updated = insert_dependency(source, "math", "math = { path = \"../math\" }")
            .expect("dependency should be inserted");

        assert!(
            updated.contains("[dependencies]\nmath = { path = \"../math\" }\n\n[profile.debug]")
        );
    }

    #[test]
    fn dependency_section_should_stop_before_the_next_table() {
        let source = "[dependencies]\na = { path = \"a\" }\n\n[profile.debug]\noptimization = 0\n";

        let bounds = dependency_section(source).expect("section should exist");

        assert_eq!(&source[bounds.0..bounds.1], "a = { path = \"a\" }\n\n");
    }
}
