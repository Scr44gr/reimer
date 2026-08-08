//! End-to-end native compiler operations used by the `reimer` command.

use std::ffi::OsString;
use std::path::Path;

use reimer_codegen_native::OptimizationLevel;
use reimer_diagnostics::Diagnostic;
use reimer_package::{FileDiagnostic, SourceGraph};
use reimer_project::NativeDependencies;

/// One observable stage in the frontend or native backend pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerStage {
    /// Reads, lexes, parses, and joins the reachable source modules.
    LoadingSources,
    /// Resolves names, types, ownership, and semantic constraints.
    Resolving,
    /// Lowers typed HIR to native object or JIT machine code.
    GeneratingCode,
}

/// Progress emitted by compiler operations that opt into reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerProgress<'path> {
    /// A compiler stage has started.
    StageStarted(CompilerStage),
    /// An exact source file was included in the compiled module graph.
    Source {
        /// Source path as loaded by the package graph.
        path: &'path Path,
        /// One-based position in deterministic load order.
        index: usize,
        /// Total number of reachable source files.
        total: usize,
    },
    /// A compiler stage completed successfully.
    StageFinished(CompilerStage),
}

/// Runs the complete frontend without native code generation.
///
/// # Errors
///
/// Returns lexer, parser, name-resolution, or type-checking diagnostics.
pub fn check_source(source: &str) -> Result<(), Vec<Diagnostic>> {
    analyze(source).map(|_| ())
}

/// Compiles Reimer source text to a host-native object.
///
/// # Errors
///
/// Returns frontend or native backend diagnostics.
pub fn compile_to_object(source: &str) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let program = analyze(source)?;
    reimer_codegen_native::emit_object(&program)
}

/// Compiles and executes Reimer source text through the host JIT.
///
/// # Errors
///
/// Returns frontend or native backend diagnostics.
pub fn execute_source(source: &str) -> Result<i32, Vec<Diagnostic>> {
    let program = analyze(source)?;
    reimer_codegen_native::execute(&program)
}

/// Checks an entry file and all of its statically imported modules.
///
/// # Errors
///
/// Returns file-aware loader, frontend, or type-checking diagnostics.
pub fn check_file(path: &Path) -> Result<(), Vec<FileDiagnostic>> {
    analyze_file(path).map(|_| ())
}

/// Compiles an entry file and its module graph to a host-native object.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn compile_file_to_object(path: &Path) -> Result<Vec<u8>, Vec<FileDiagnostic>> {
    compile_file_to_object_with_options(path, OptimizationLevel::None)
}

/// Compiles an entry file and its module graph with the selected optimization.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn compile_file_to_object_with_options(
    path: &Path,
    optimization: OptimizationLevel,
) -> Result<Vec<u8>, Vec<FileDiagnostic>> {
    compile_file_to_object_with_progress(path, optimization, |_| {})
}

/// Compiles a file module graph while reporting sources and compiler stages.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn compile_file_to_object_with_progress(
    path: &Path,
    optimization: OptimizationLevel,
    mut progress: impl FnMut(CompilerProgress<'_>),
) -> Result<Vec<u8>, Vec<FileDiagnostic>> {
    let (package, program) = analyze_file_with_progress(path, &mut progress)?;
    progress(CompilerProgress::StageStarted(
        CompilerStage::GeneratingCode,
    ));
    let object = reimer_codegen_native::emit_object_with_options(&program, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))?;
    progress(CompilerProgress::StageFinished(
        CompilerStage::GeneratingCode,
    ));
    Ok(object)
}

/// JIT-compiles and executes an entry file and its module graph.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn execute_file(path: &Path) -> Result<i32, Vec<FileDiagnostic>> {
    execute_file_with_options(path, OptimizationLevel::None)
}

/// JIT-compiles and executes an entry file with the selected optimization.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn execute_file_with_options(
    path: &Path,
    optimization: OptimizationLevel,
) -> Result<i32, Vec<FileDiagnostic>> {
    let (package, program) = analyze_file(path)?;
    reimer_codegen_native::execute_with_options(&program, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// JIT-compiles and executes an entry file with explicit process-style arguments.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn execute_file_with_arguments(
    path: &Path,
    optimization: OptimizationLevel,
    arguments: Vec<OsString>,
) -> Result<i32, Vec<FileDiagnostic>> {
    execute_file_with_arguments_and_progress(path, optimization, arguments, |_| {})
}

/// JIT-compiles a file module graph with arguments while reporting progress.
///
/// The code-generation stage finishes immediately before source code receives
/// control, so long-running programs do not keep compilation progress active.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn execute_file_with_arguments_and_progress(
    path: &Path,
    optimization: OptimizationLevel,
    arguments: Vec<OsString>,
    mut progress: impl FnMut(CompilerProgress<'_>),
) -> Result<i32, Vec<FileDiagnostic>> {
    let (package, program) = analyze_file_with_progress(path, &mut progress)?;
    progress(CompilerProgress::StageStarted(
        CompilerStage::GeneratingCode,
    ));
    reimer_codegen_native::execute_with_arguments_and_ready(
        &program,
        optimization,
        arguments,
        || {
            progress(CompilerProgress::StageFinished(
                CompilerStage::GeneratingCode,
            ));
        },
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Checks every source reachable through a resolved package graph.
///
/// # Errors
///
/// Returns file-aware loader, frontend, or type-checking diagnostics.
pub fn check_graph(graph: &SourceGraph) -> Result<(), Vec<FileDiagnostic>> {
    analyze_graph(graph).map(|_| ())
}

/// Compiles a resolved package graph to a host-native object.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn compile_graph_to_object(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
) -> Result<Vec<u8>, Vec<FileDiagnostic>> {
    compile_graph_to_object_with_progress(graph, optimization, |_| {})
}

/// Compiles a resolved graph while reporting sources and compiler stages.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn compile_graph_to_object_with_progress(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    mut progress: impl FnMut(CompilerProgress<'_>),
) -> Result<Vec<u8>, Vec<FileDiagnostic>> {
    let (package, program) = analyze_graph_with_progress(graph, &mut progress)?;
    progress(CompilerProgress::StageStarted(
        CompilerStage::GeneratingCode,
    ));
    let object = reimer_codegen_native::emit_object_with_options(&program, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))?;
    progress(CompilerProgress::StageFinished(
        CompilerStage::GeneratingCode,
    ));
    Ok(object)
}

/// JIT-compiles and executes a resolved package graph.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn execute_graph(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
) -> Result<i32, Vec<FileDiagnostic>> {
    let (package, program) = analyze_graph(graph)?;
    reimer_codegen_native::execute_with_options(&program, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// JIT-compiles and executes a resolved package graph with its native manifest inputs.
///
/// # Errors
///
/// Returns file-aware package, frontend, native-library, or backend diagnostics.
pub fn execute_graph_with_native_dependencies(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    native: &NativeDependencies,
) -> Result<i32, Vec<FileDiagnostic>> {
    let (package, program) = analyze_graph(graph)?;
    reimer_codegen_native::execute_with_native_libraries(
        &program,
        optimization,
        None,
        native.library_paths(),
        native.link_libraries(),
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// JIT-compiles and executes a resolved source graph with explicit arguments.
///
/// # Errors
///
/// Returns file-aware package, frontend, or backend diagnostics.
pub fn execute_graph_with_arguments(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    arguments: Vec<OsString>,
) -> Result<i32, Vec<FileDiagnostic>> {
    let (package, program) = analyze_graph(graph)?;
    reimer_codegen_native::execute_with_arguments(&program, optimization, arguments)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// JIT-compiles and executes a resolved graph with arguments and native manifest inputs.
///
/// # Errors
///
/// Returns file-aware package, frontend, native-library, or backend diagnostics.
pub fn execute_graph_with_arguments_and_native_dependencies(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    arguments: Vec<OsString>,
    native: &NativeDependencies,
) -> Result<i32, Vec<FileDiagnostic>> {
    execute_graph_with_arguments_and_native_dependencies_and_progress(
        graph,
        optimization,
        arguments,
        native,
        |_| {},
    )
}

/// JIT-compiles a resolved graph with native dependencies while reporting progress.
///
/// The code-generation stage finishes immediately before source code receives
/// control, so long-running programs do not keep compilation progress active.
///
/// # Errors
///
/// Returns file-aware package, frontend, native-library, or backend diagnostics.
pub fn execute_graph_with_arguments_and_native_dependencies_and_progress(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    arguments: Vec<OsString>,
    native: &NativeDependencies,
    mut progress: impl FnMut(CompilerProgress<'_>),
) -> Result<i32, Vec<FileDiagnostic>> {
    let (package, program) = analyze_graph_with_progress(graph, &mut progress)?;
    progress(CompilerProgress::StageStarted(
        CompilerStage::GeneratingCode,
    ));
    reimer_codegen_native::execute_with_native_libraries_and_ready(
        &program,
        optimization,
        Some(arguments),
        native.library_paths(),
        native.link_libraries(),
        || {
            progress(CompilerProgress::StageFinished(
                CompilerStage::GeneratingCode,
            ));
        },
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Discovers compiler-recognized `@test` functions in one source package.
///
/// # Errors
///
/// Returns file-aware frontend diagnostics when the package cannot be loaded
/// or checked.
pub fn file_test_names(path: &Path) -> Result<Vec<String>, Vec<FileDiagnostic>> {
    let (_, program) = analyze_test_file(path)?;
    Ok(test_names(&program))
}

/// Executes one compiler-recognized `@test` function in the current process.
///
/// Callers should isolate this operation in a child process because a source
/// panic deliberately aborts the process.
///
/// # Errors
///
/// Returns file-aware frontend or backend diagnostics.
pub fn execute_file_test(
    path: &Path,
    test_index: usize,
    optimization: OptimizationLevel,
) -> Result<(), Vec<FileDiagnostic>> {
    let (package, program) = analyze_test_file(path)?;
    reimer_codegen_native::execute_test_with_options(&program, test_index, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Executes every compiler-recognized `@test` function after compiling the
/// source package once.
///
/// Callers should isolate this operation in a child process because a source
/// panic deliberately aborts the process.
///
/// # Errors
///
/// Returns file-aware frontend or backend diagnostics.
pub fn execute_file_tests(
    path: &Path,
    optimization: OptimizationLevel,
) -> Result<(), Vec<FileDiagnostic>> {
    execute_file_tests_with_progress(path, optimization, |_, _| {})
}

/// Executes every compiler-recognized `@test` and reports each test before it
/// starts.
///
/// # Errors
///
/// Returns file-aware frontend or backend diagnostics.
pub fn execute_file_tests_with_progress(
    path: &Path,
    optimization: OptimizationLevel,
    mut progress: impl FnMut(usize, &str),
) -> Result<(), Vec<FileDiagnostic>> {
    let (package, program) = analyze_test_file(path)?;
    reimer_codegen_native::execute_tests_with_options_and_progress(
        &program,
        optimization,
        |test_index, name| progress(test_index, source_test_name(name)),
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Discovers compiler-recognized `@test` functions in a resolved source graph.
///
/// # Errors
///
/// Returns file-aware frontend diagnostics when the graph cannot be loaded or
/// checked.
pub fn graph_test_names(graph: &SourceGraph) -> Result<Vec<String>, Vec<FileDiagnostic>> {
    let (_, program) = analyze_test_graph(graph)?;
    Ok(test_names(&program))
}

/// Executes one graph-discovered `@test` function in the current process.
///
/// Callers should isolate this operation in a child process because a source
/// panic deliberately aborts the process.
///
/// # Errors
///
/// Returns file-aware frontend or backend diagnostics.
pub fn execute_graph_test(
    graph: &SourceGraph,
    test_index: usize,
    optimization: OptimizationLevel,
) -> Result<(), Vec<FileDiagnostic>> {
    let (package, program) = analyze_test_graph(graph)?;
    reimer_codegen_native::execute_test_with_options(&program, test_index, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Executes one graph-discovered `@test` with native manifest inputs.
///
/// # Errors
///
/// Returns file-aware frontend, native-library, or backend diagnostics.
pub fn execute_graph_test_with_native_dependencies(
    graph: &SourceGraph,
    test_index: usize,
    optimization: OptimizationLevel,
    native: &NativeDependencies,
) -> Result<(), Vec<FileDiagnostic>> {
    let (package, program) = analyze_test_graph(graph)?;
    reimer_codegen_native::execute_test_with_native_libraries(
        &program,
        test_index,
        optimization,
        native.library_paths(),
        native.link_libraries(),
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Executes every graph-discovered `@test` function after compiling the graph
/// once.
///
/// Callers should isolate this operation in a child process because a source
/// panic deliberately aborts the process.
///
/// # Errors
///
/// Returns file-aware frontend or backend diagnostics.
pub fn execute_graph_tests(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
) -> Result<(), Vec<FileDiagnostic>> {
    execute_graph_tests_with_progress(graph, optimization, |_, _| {})
}

/// Executes every graph-discovered `@test` and reports each test before it
/// starts.
///
/// # Errors
///
/// Returns file-aware frontend or backend diagnostics.
pub fn execute_graph_tests_with_progress(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    mut progress: impl FnMut(usize, &str),
) -> Result<(), Vec<FileDiagnostic>> {
    let (package, program) = analyze_test_graph(graph)?;
    reimer_codegen_native::execute_tests_with_options_and_progress(
        &program,
        optimization,
        |test_index, name| progress(test_index, source_test_name(name)),
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

/// Executes every graph-discovered `@test` with native manifest inputs.
///
/// # Errors
///
/// Returns file-aware frontend, native-library, or backend diagnostics.
pub fn execute_graph_tests_with_native_dependencies_and_progress(
    graph: &SourceGraph,
    optimization: OptimizationLevel,
    native: &NativeDependencies,
    mut progress: impl FnMut(usize, &str),
) -> Result<(), Vec<FileDiagnostic>> {
    let (package, program) = analyze_test_graph(graph)?;
    reimer_codegen_native::execute_tests_with_native_libraries_and_progress(
        &program,
        optimization,
        native.library_paths(),
        native.link_libraries(),
        |test_index, name| progress(test_index, source_test_name(name)),
    )
    .map_err(|diagnostics| package.map_diagnostics(diagnostics))
}

fn test_names(program: &reimer_hir::Program) -> Vec<String> {
    program
        .tests
        .iter()
        .filter_map(|test| {
            program
                .functions
                .iter()
                .find(|function| function.id == *test)
                .map(|function| source_test_name(&function.name).to_owned())
        })
        .collect()
}

fn source_test_name(name: &str) -> &str {
    name.rsplit_once('$').map_or(name, |(_, suffix)| suffix)
}

fn analyze(source: &str) -> Result<reimer_hir::Program, Vec<Diagnostic>> {
    let tokens = reimer_lexer::lex(source)?;
    let program = reimer_parser::parse(&tokens)?;
    reimer_resolver::resolve(&program)
}

fn analyze_file(
    path: &Path,
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    analyze_file_with_progress(path, &mut |_| {})
}

fn analyze_file_with_progress(
    path: &Path,
    progress: &mut impl FnMut(CompilerProgress<'_>),
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    progress(CompilerProgress::StageStarted(
        CompilerStage::LoadingSources,
    ));
    let package = reimer_package::load(path)?;
    report_sources(&package, progress);
    progress(CompilerProgress::StageFinished(
        CompilerStage::LoadingSources,
    ));
    progress(CompilerProgress::StageStarted(CompilerStage::Resolving));
    let resolved = if is_library_entry(path) {
        reimer_resolver::resolve_library(&package.program)
    } else {
        reimer_resolver::resolve(&package.program)
    };
    match resolved {
        Ok(program) => {
            progress(CompilerProgress::StageFinished(CompilerStage::Resolving));
            Ok((package, program))
        }
        Err(diagnostics) => Err(package.map_diagnostics(diagnostics)),
    }
}

fn analyze_graph(
    graph: &SourceGraph,
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    analyze_graph_with_progress(graph, &mut |_| {})
}

fn analyze_graph_with_progress(
    graph: &SourceGraph,
    progress: &mut impl FnMut(CompilerProgress<'_>),
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    progress(CompilerProgress::StageStarted(
        CompilerStage::LoadingSources,
    ));
    let package = reimer_package::load_graph(graph)?;
    report_sources(&package, progress);
    progress(CompilerProgress::StageFinished(
        CompilerStage::LoadingSources,
    ));
    progress(CompilerProgress::StageStarted(CompilerStage::Resolving));
    let library = graph
        .packages
        .iter()
        .find(|candidate| candidate.id == graph.root)
        .is_some_and(|root| is_library_entry(&root.entry));
    let resolved = if library {
        reimer_resolver::resolve_library(&package.program)
    } else {
        reimer_resolver::resolve(&package.program)
    };
    match resolved {
        Ok(program) => {
            progress(CompilerProgress::StageFinished(CompilerStage::Resolving));
            Ok((package, program))
        }
        Err(diagnostics) => Err(package.map_diagnostics(diagnostics)),
    }
}

fn report_sources(
    package: &reimer_package::Package,
    progress: &mut impl FnMut(CompilerProgress<'_>),
) {
    let total = package.source_paths().count();
    for (index, path) in package.source_paths().enumerate() {
        progress(CompilerProgress::Source {
            path,
            index: index.saturating_add(1),
            total,
        });
    }
}

fn analyze_test_file(
    path: &Path,
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    let package = reimer_package::load(path)?;
    match reimer_resolver::resolve_library(&package.program) {
        Ok(program) => Ok((package, program)),
        Err(diagnostics) => Err(package.map_diagnostics(diagnostics)),
    }
}

fn analyze_test_graph(
    graph: &SourceGraph,
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    let package = reimer_package::load_graph(graph)?;
    match reimer_resolver::resolve_library(&package.program) {
        Ok(program) => Ok((package, program)),
        Err(diagnostics) => Err(package.map_diagnostics(diagnostics)),
    }
}

fn is_library_entry(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("package.reim")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::{
        check_file, check_source, compile_file_to_object, compile_to_object, execute_file,
        execute_file_with_arguments, execute_source, source_test_name,
    };
    use reimer_codegen_native::OptimizationLevel;

    #[test]
    fn source_test_name_should_hide_internal_module_mangling() {
        let name = "__module_4_game_7_systems$camera_motion_should_prioritize_commands";

        assert_eq!(
            source_test_name(name),
            "camera_motion_should_prioritize_commands"
        );
    }

    #[test]
    fn compile_to_object_should_complete_m0_vertical_slice() {
        let object = compile_to_object("fn main() -> i32 { return 42; }")
            .expect("reference program should compile");

        assert!(!object.is_empty());
    }

    #[test]
    fn execute_source_should_complete_m0_vertical_slice() {
        let result = execute_source("fn main() -> i32 { return 42; }")
            .expect("reference program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_source_should_complete_m1_vertical_slice() {
        let source = "fn double(value: i32) -> i32 { value * 2 }
            fn main() -> i32 {
                let mut value = 1;
                while value < 21 { value += 1; }
                if true { double(value) } else { 0 }
            }";

        let result = execute_source(source).expect("reference program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_source_should_complete_m2_scalar_vertical_slice() {
        let source = include_str!("../../../examples/m2_scalars.reim");

        let result = execute_source(source).expect("reference program should execute");
        let object = compile_to_object(source).expect("reference program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_numeric_literal_vertical_slice() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m2_numeric_literals.reim");

        let result = execute_file(&path).expect("numeric literal program should execute");
        let object = compile_file_to_object(&path).expect("numeric literal program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_source_should_complete_m2_composite_vertical_slice() {
        let source = include_str!("../../../examples/m2_composites.reim");

        let result = execute_source(source).expect("reference program should execute");
        let object = compile_to_object(source).expect("reference program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_source_should_complete_m2_control_vertical_slice() {
        let source = include_str!("../../../examples/m2_control.reim");

        let result = execute_source(source).expect("reference program should execute");
        let object = compile_to_object(source).expect("reference program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_source_should_complete_m3_views_vertical_slice() {
        let source = include_str!("../../../examples/m3_views.reim");

        let result = execute_source(source).expect("reference program should execute");
        let object = compile_to_object(source).expect("reference program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_source_should_complete_m3_errors_vertical_slice() {
        let source = include_str!("../../../examples/m3_errors.reim");

        let result = execute_source(source).expect("reference program should execute");
        let object = compile_to_object(source).expect("reference program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_source_should_complete_m3_memory_vertical_slice() {
        let source = include_str!("../../../examples/m3_memory.reim");

        let result = execute_source(source).expect("reference program should execute");
        let object = compile_to_object(source).expect("reference program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_m3_allocator_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_allocators.reim");

        let result = execute_file(&path).expect("allocator program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn compile_file_should_emit_the_m3_allocator_object() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_allocators.reim");

        let object = compile_file_to_object(&path).expect("allocator program should compile");

        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_cover_arena_and_fixed_buffer_allocators() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_allocator_kinds.reim");

        let result = execute_file(&path).expect("allocator-kind program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_complete_safe_filesystem_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_filesystem.reim");

        let result = execute_file(&path).expect("file-system program should execute");
        let object = compile_file_to_object(&path).expect("file-system program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_scalar_and_vector_math_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_math.reim");

        let result = execute_file(&path).expect("math program should execute");
        let object = compile_file_to_object(&path).expect("math program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_time_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_time.reim");

        let result = execute_file(&path).expect("time program should execute");
        let object = compile_file_to_object(&path).expect("time program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_receive_explicit_environment_arguments() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platform_environment.reim");
        let arguments = [path.as_os_str().to_owned(), OsString::from("hello")].to_vec();

        let result = execute_file_with_arguments(&path, OptimizationLevel::None, arguments)
            .expect("environment program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_complete_process_vertical_slice() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platform_process.reim");

        let result = execute_file(&path).expect("process program should execute");
        let object = compile_file_to_object(&path).expect("process program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_target_correct_c_types_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m5_c_types.reim");

        let result = execute_file(&path).expect("C type program should execute");
        let object = compile_file_to_object(&path).expect("C type program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_explicit_integer_overflow_modes() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_integer_overflow.reim");

        let result = execute_file(&path).expect("integer overflow program should execute");
        let object =
            compile_file_to_object(&path).expect("integer overflow program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_recoverable_slice_access() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_slice_access.reim");

        let result = execute_file(&path).expect("slice access program should execute");
        let object = compile_file_to_object(&path).expect("slice access program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_utf8_views_and_iteration() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_utf8.reim");

        let result = execute_file(&path).expect("UTF-8 program should execute");
        let object = compile_file_to_object(&path).expect("UTF-8 program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_safe_standard_output_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_io.reim");

        let result = execute_file(&path).expect("standard I/O program should execute");
        let object = compile_file_to_object(&path).expect("standard I/O program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_owned_string_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_string.reim");

        let result = execute_file(&path).expect("owned string program should execute");
        let object = compile_file_to_object(&path).expect("owned string program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_text_formatting_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_text.reim");

        let result = execute_file(&path).expect("text formatting program should execute");
        let object = compile_file_to_object(&path).expect("text formatting program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_interpolated_string_vertical_slice() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_interpolation.reim");

        let result = execute_file(&path).expect("interpolation program should execute");
        let object = compile_file_to_object(&path).expect("interpolation program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn check_file_should_require_display_for_interpolated_structs() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/missing_display.reim");

        let diagnostics = check_file(&path).expect_err("missing Display should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.diagnostic.code == "E3161"
                && diagnostic.diagnostic.message.contains("Undisplayable")
                && !diagnostic.diagnostic.message.contains("__module_")
                && diagnostic
                    .diagnostic
                    .help
                    .as_deref()
                    .is_some_and(|help| help.contains("std::fmt::Display"))
        }));
    }

    #[test]
    fn check_file_should_require_debug_for_debug_interpolation() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/missing_debug.reim");

        let diagnostics = check_file(&path).expect_err("missing Debug should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.diagnostic.code == "E3161"
                && diagnostic.diagnostic.message.contains("Undebuggable")
                && !diagnostic.diagnostic.message.contains("__module_")
                && diagnostic
                    .diagnostic
                    .help
                    .as_deref()
                    .is_some_and(|help| help.contains("std::fmt::Debug"))
        }));
    }

    #[test]
    fn execute_file_should_complete_owned_vector_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_vec.reim");

        let result = execute_file(&path).expect("owned vector program should execute");
        let object = compile_file_to_object(&path).expect("owned vector program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_owned_collections_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_collections.reim");

        let result = execute_file(&path).expect("owned collections program should execute");
        let object =
            compile_file_to_object(&path).expect("owned collections program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_preserve_hash_map_entries_through_growth_and_deletion() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m3_hash_map.reim");

        let result = execute_file(&path).expect("hash map stress program should execute");
        let object = compile_file_to_object(&path).expect("hash map stress program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_m4_module_vertical_slice() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m4_modules/main.reim");

        let result = execute_file(&path).expect("module program should execute");
        let object = compile_file_to_object(&path).expect("module program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_m5_ffi_vertical_slice() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m5_ffi.reim");

        let result = execute_file(&path).expect("FFI program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn compile_file_should_emit_the_m5_ffi_object() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m5_ffi.reim");

        let object = compile_file_to_object(&path).expect("FFI program should compile");

        assert!(!object.is_empty());
    }

    #[test]
    fn compile_file_should_emit_the_sdl_window_demo_object() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m5_sdl_window.reim");

        let object = compile_file_to_object(&path).expect("SDL window demo should compile");

        assert!(!object.is_empty());
    }

    #[test]
    fn compile_file_should_emit_the_integrated_graphics_demo_object() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m5_sdl_opengl.reim");

        let object =
            compile_file_to_object(&path).expect("integrated graphics demo should compile");

        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_m6_method_dispatch() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m6_methods.reim");

        let result = execute_file(&path).expect("method program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_complete_m7_tensor_operations() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m7_tensor.reim");

        let result = execute_file(&path).expect("tensor program should execute");
        let object = compile_file_to_object(&path).expect("tensor program should compile");

        assert_eq!(result, 42);
        assert!(!object.is_empty());
    }

    #[test]
    fn execute_file_should_complete_m7_matrix_multiplication() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m7_matmul.reim");

        let result = execute_file(&path).expect("matrix multiplication should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_complete_native_and_scoped_threads() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m9_threads/main.reim");

        let result = execute_file(&path).expect("thread program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_coordinate_shared_synchronization_resources() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m9_synchronization/main.reim");

        let result = execute_file(&path).expect("synchronization program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_complete_jobs_on_a_fixed_worker_pool() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m9_jobs/main.reim");

        let result = execute_file(&path).expect("job-pool program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_apply_parallel_tensor_chunks() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m9_tensor_parallel/main.reim");

        let result = execute_file(&path).expect("parallel tensor program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_file_should_share_atomic_scalar_cells() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/m9_atomics/main.reim");

        let result = execute_file(&path).expect("atomic scalar program should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn check_file_should_reject_borrows_that_escape_into_native_threads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m9_threads/unscoped_borrow_error.reim");

        let diagnostics = check_file(&path).expect_err("unscoped borrow should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == "E3152")
        );
    }

    #[test]
    fn check_file_should_reject_overlapping_parallel_mutable_borrows() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m9_jobs/overlapping_borrow_error.reim");

        let diagnostics = check_file(&path).expect_err("overlapping borrow should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| { matches!(diagnostic.diagnostic.code, "E3138" | "E3116") })
        );
    }

    #[test]
    fn check_file_should_reject_raw_pointer_job_arguments() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m9_jobs/raw_pointer_job_error.reim");

        let diagnostics = check_file(&path).expect_err("raw pointer job should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == "E6014")
        );
    }

    #[test]
    fn check_source_should_report_type_errors_without_codegen() {
        let diagnostics =
            check_source("fn main() -> i32 { true }").expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3111")
        );
    }

    #[test]
    fn compile_file_should_accept_a_library_facade_without_main() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m8_packages/vectors/src/package.reim");

        let object = compile_file_to_object(&path).expect("library facade should compile");

        assert!(!object.is_empty());
    }
}
