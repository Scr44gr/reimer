//! End-to-end native compiler operations used by the `reimer` command.

use std::path::Path;

use reimer_codegen_native::OptimizationLevel;
use reimer_diagnostics::Diagnostic;
use reimer_package::{FileDiagnostic, SourceGraph};

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
    let (package, program) = analyze_file(path)?;
    reimer_codegen_native::emit_object_with_options(&program, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
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
    let (package, program) = analyze_graph(graph)?;
    reimer_codegen_native::emit_object_with_options(&program, optimization)
        .map_err(|diagnostics| package.map_diagnostics(diagnostics))
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

fn test_names(program: &reimer_hir::Program) -> Vec<String> {
    program
        .tests
        .iter()
        .filter_map(|test| {
            program
                .functions
                .iter()
                .find(|function| function.id == *test)
                .map(|function| function.name.clone())
        })
        .collect()
}

fn analyze(source: &str) -> Result<reimer_hir::Program, Vec<Diagnostic>> {
    let tokens = reimer_lexer::lex(source)?;
    let program = reimer_parser::parse(&tokens)?;
    reimer_resolver::resolve(&program)
}

fn analyze_file(
    path: &Path,
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    let package = reimer_package::load(path)?;
    let resolved = if is_library_entry(path) {
        reimer_resolver::resolve_library(&package.program)
    } else {
        reimer_resolver::resolve(&package.program)
    };
    match resolved {
        Ok(program) => Ok((package, program)),
        Err(diagnostics) => Err(package.map_diagnostics(diagnostics)),
    }
}

fn analyze_graph(
    graph: &SourceGraph,
) -> Result<(reimer_package::Package, reimer_hir::Program), Vec<FileDiagnostic>> {
    let package = reimer_package::load_graph(graph)?;
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
        Ok(program) => Ok((package, program)),
        Err(diagnostics) => Err(package.map_diagnostics(diagnostics)),
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
    use std::path::Path;

    use super::{
        check_file, check_source, compile_file_to_object, compile_to_object, execute_file,
        execute_source,
    };

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
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/m3_missing_display_error.reim");

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
