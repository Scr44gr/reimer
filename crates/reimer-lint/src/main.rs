use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use reimer_lint::{Finding, Severity, analyze, apply_spelling_fixes};
use reimer_project::{LockMode, Project, ProjectError};

fn main() -> ExitCode {
    run(env::args_os().skip(1))
}

fn run(arguments: impl Iterator<Item = OsString>) -> ExitCode {
    let mut deny_warnings = false;
    let mut paths = Vec::new();
    for argument in arguments {
        if argument == "--deny-warnings" {
            deny_warnings = true;
        } else if argument.to_string_lossy().starts_with('-') {
            eprintln!("error: unknown option `{}`", argument.to_string_lossy());
            return ExitCode::from(2);
        } else {
            paths.push(argument);
        }
    }
    if paths.is_empty() {
        eprintln!("usage: reimer-lint [--deny-warnings] <entry.reim>...");
        return ExitCode::from(2);
    }

    let mut failed = false;
    for path in paths {
        if !check_path(Path::new(&path), deny_warnings) {
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn check_path(path: &Path, deny_warnings: bool) -> bool {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("error: could not read `{}`: {error}", path.display());
            return false;
        }
    };
    let analysis = analyze(&source);
    let mut accepted = true;
    for finding in analysis
        .findings
        .iter()
        .filter(|finding| finding.severity != Severity::Error)
    {
        print_finding(path, &source, finding);
        if deny_warnings && finding.severity == Severity::Warning {
            accepted = false;
        }
    }

    let package = match Project::open(path, LockMode::Use) {
        Ok(project) => reimer_package::load_graph(&project.source_graph(path)),
        Err(ProjectError::ManifestNotFound { .. }) => reimer_package::load(path),
        Err(error) => {
            eprintln!("error: {error}");
            return false;
        }
    };
    let package = match package {
        Ok(package) => package,
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprint!("{}", diagnostic.render());
            }
            return false;
        }
    };
    let has_main = analysis.syntax.as_ref().is_some_and(|syntax| {
        syntax.items.iter().any(|item| {
            matches!(
                item,
                reimer_ast::Item::Function(function) if function.name.name == "main"
            )
        })
    });
    let resolved = if has_main {
        reimer_resolver::resolve(&package.program)
    } else {
        reimer_resolver::resolve_library(&package.program)
    };
    if let Err(diagnostics) = resolved {
        for diagnostic in package.map_diagnostics(diagnostics) {
            if diagnostic.path == path {
                let mut finding = Finding::from_compiler(diagnostic.diagnostic);
                if let Some(syntax) = &analysis.syntax {
                    apply_spelling_fixes(&source, syntax, std::slice::from_mut(&mut finding));
                }
                print_finding(path, &source, &finding);
            } else {
                eprint!("{}", diagnostic.render());
            }
        }
        return false;
    }
    accepted
}

fn print_finding(path: &Path, source: &str, finding: &Finding) {
    let bounded = finding.span.start.min(source.len());
    let prefix = &source[..bounded];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..bounded].chars().count() + 1;
    let severity = match finding.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Information => "info",
        Severity::Hint => "hint",
    };
    eprintln!(
        "{severity}[{}]: {}\n --> {}:{line}:{column}",
        finding.code,
        finding.message,
        path.display()
    );
    if let Some(help) = &finding.help {
        eprintln!(" help: {help}");
    }
}
