//! Cranelift object generation and host JIT execution for typed Reimer HIR.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
use cranelift_codegen::ir::{
    self, AbiParam, BlockArg, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, TrapCode,
    Value, types,
};
use cranelift_codegen::isa::{OwnedTargetIsa, TargetFrontendConfig};
use cranelift_codegen::settings;
use cranelift_codegen::settings::Configurable;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::object::{BinaryFormat, SectionKind};
use cranelift_object::{ObjectBuilder, ObjectModule};
use reimer_diagnostics::{Diagnostic, Span};
use reimer_hir::{
    AssertionMode, AssignmentOperator, BinaryOperator, Block, Expression, ExpressionKind,
    ForIteration, Function, FunctionId, IntegerAdditionMode, LocalId, MatchExpression, Pattern,
    PatternKind, Place, PlaceKind, Program, Statement, Static, StaticId, SynchronizationKind,
    TypeDefinitionKind, UnaryOperator,
};
use reimer_layout::{AggregateLayoutKind, Layouts};
use reimer_runtime::Failure;
use reimer_types::{Type, TypeId};

const ENTRY_SYMBOL: &str = "program_main";
const BUILTIN_RUNTIME_LIBRARY: &str = "runtime";

/// Optimization strategy used by the native backend.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationLevel {
    /// Prioritizes compilation speed and debuggability.
    #[default]
    None,
    /// Prioritizes execution speed.
    Speed,
    /// Balances execution speed with generated code size.
    SpeedAndSize,
}

impl OptimizationLevel {
    const fn setting(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Speed => "speed",
            Self::SpeedAndSize => "speed_and_size",
        }
    }

    const fn debug_assertions(self) -> bool {
        matches!(self, Self::None)
    }
}

struct NativeLibraries {
    libraries: Vec<libloading::Library>,
}

impl NativeLibraries {
    fn load(program: &Program) -> Result<Self, String> {
        let names = linked_native_libraries(program);
        let mut libraries = Vec::with_capacity(names.len());
        for name in names {
            libraries.push(load_native_library(name)?);
        }
        Ok(Self { libraries })
    }

    fn is_empty(&self) -> bool {
        self.libraries.is_empty()
    }

    #[expect(
        unsafe_code,
        reason = "symbol lookup returns an address while this owner keeps every library loaded"
    )]
    fn lookup(&self, name: &str) -> Option<*const u8> {
        self.libraries.iter().find_map(|library| {
            // SAFETY: The pointer remains valid because the closure owns all loaded libraries.
            unsafe { library.get::<*const u8>(name.as_bytes()).ok() }.map(|symbol| *symbol)
        })
    }
}

#[expect(
    unsafe_code,
    reason = "loading a user-declared native library may execute its platform initializer"
)]
fn load_native_library(name: &str) -> Result<libloading::Library, String> {
    let conventional = libloading::library_filename(name);
    // SAFETY: `@link` explicitly opts the program into loading this native library.
    match unsafe { libloading::Library::new(name) } {
        Ok(library) => Ok(library),
        Err(first_error) if conventional != name => {
            // SAFETY: This is the platform-conventional spelling of the same requested library.
            unsafe { libloading::Library::new(&conventional) }.map_err(|second_error| {
                format!(
                    "failed to load native library `{name}` ({first_error}); `{}` also failed ({second_error})",
                    conventional.to_string_lossy()
                )
            })
        }
        Err(error) => Err(format!("failed to load native library `{name}`: {error}")),
    }
}

/// Emits a native object for the current host.
///
/// # Errors
///
/// Returns a backend diagnostic when Cranelift rejects valid typed HIR.
pub fn emit_object(program: &Program) -> Result<Vec<u8>, Vec<Diagnostic>> {
    emit_object_with_options(program, OptimizationLevel::None)
}

/// Emits a native object for the current host with the selected optimization.
///
/// # Errors
///
/// Returns a backend diagnostic when Cranelift rejects valid typed HIR.
pub fn emit_object_with_options(
    program: &Program,
    optimization: OptimizationLevel,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let isa = host_isa(optimization).map_err(backend_error)?;
    let builder = ObjectBuilder::new(isa, "program", default_libcall_names())
        .map_err(|error| backend_error(error.to_string()))?;
    let mut module = ObjectModule::new(builder);
    compile_program(&mut module, program, optimization).map_err(backend_error)?;
    let mut product = module.finish();
    add_object_link_metadata(&mut product.object, program);
    product
        .emit()
        .map_err(|error| backend_error(error.to_string()))
}

fn linked_native_libraries(program: &Program) -> BTreeSet<&str> {
    program
        .extern_functions
        .iter()
        .filter_map(|function| function.link.as_deref())
        .filter(|name| *name != BUILTIN_RUNTIME_LIBRARY)
        .collect()
}

fn add_object_link_metadata(
    object: &mut cranelift_object::object::write::Object<'_>,
    program: &Program,
) {
    let libraries = linked_native_libraries(program);
    if object.format() != BinaryFormat::Coff || libraries.is_empty() {
        return;
    }
    let mut directives = Vec::new();
    for library in libraries {
        let filename = if library.to_ascii_lowercase().ends_with(".lib") {
            library.to_owned()
        } else {
            format!("{library}.lib")
        };
        directives.extend_from_slice(format!(" /DEFAULTLIB:\"{filename}\"").as_bytes());
    }
    let section = object.add_section(Vec::new(), b".drectve".to_vec(), SectionKind::Linker);
    object.append_section_data(section, &directives, 1);
}

/// Compiles and executes the validated entry point in the host process.
///
/// # Errors
///
/// Returns a backend diagnostic when Cranelift rejects the program or JIT
/// memory cannot be finalized.
pub fn execute(program: &Program) -> Result<i32, Vec<Diagnostic>> {
    execute_with_options(program, OptimizationLevel::None)
}

/// Compiles and executes the validated entry point with the selected optimization.
///
/// # Errors
///
/// Returns a backend diagnostic when Cranelift rejects the program or JIT
/// memory cannot be finalized.
pub fn execute_with_options(
    program: &Program,
    optimization: OptimizationLevel,
) -> Result<i32, Vec<Diagnostic>> {
    let flags = [
        ("enable_llvm_abi_extensions", "true"),
        ("opt_level", optimization.setting()),
    ];
    let mut builder = JITBuilder::with_flags(&flags, default_libcall_names())
        .map_err(|error| backend_error(error.to_string()))?;
    register_runtime_symbols(&mut builder);
    let libraries = NativeLibraries::load(program).map_err(backend_error)?;
    if !libraries.is_empty() {
        builder.symbol_lookup_fn(Box::new(move |name| libraries.lookup(name)));
    }
    let mut module = JITModule::new(builder);
    let functions = compile_program(&mut module, program, optimization).map_err(backend_error)?;
    module
        .finalize_definitions()
        .map_err(|error| backend_error(error.to_string()))?;
    let entry = program
        .entry
        .ok_or_else(|| backend_error("typed HIR does not contain an executable entry point"))?;
    let entry = functions
        .get(&entry)
        .copied()
        .ok_or_else(|| backend_error("typed HIR entry function is missing"))?;
    let pointer = module.get_finalized_function(entry);
    let session = reimer_runtime::ExecutionSession::begin();
    let result = call_jit_entry(pointer);
    reimer_runtime::shutdown_job_pools(session.id());
    reimer_runtime::join_session_threads(session.id());
    result
}

/// Compiles and executes one `@test` function by discovery index.
///
/// This entry point is intended to run in an isolated child process because a
/// checked runtime panic terminates the current process.
///
/// # Errors
///
/// Returns a backend diagnostic when the test index is invalid or native code
/// generation fails.
pub fn execute_test(program: &Program, test_index: usize) -> Result<(), Vec<Diagnostic>> {
    execute_test_with_options(program, test_index, OptimizationLevel::None)
}

/// Compiles and executes one `@test` function with the selected optimization.
///
/// # Errors
///
/// Returns a backend diagnostic when the test index is invalid or native code
/// generation fails.
pub fn execute_test_with_options(
    program: &Program,
    test_index: usize,
    optimization: OptimizationLevel,
) -> Result<(), Vec<Diagnostic>> {
    let test = program
        .tests
        .get(test_index)
        .copied()
        .ok_or_else(|| backend_error(format!("unit test index {test_index} does not exist")))?;
    let declaration = program
        .functions
        .iter()
        .find(|function| function.id == test)
        .ok_or_else(|| backend_error("typed HIR unit test function is missing"))?;
    if !declaration.parameters.is_empty() || declaration.return_type != Type::Unit {
        return Err(backend_error(
            "typed HIR unit test must take no parameters and return `()`",
        ));
    }

    let flags = [
        ("enable_llvm_abi_extensions", "true"),
        ("opt_level", optimization.setting()),
    ];
    let mut builder = JITBuilder::with_flags(&flags, default_libcall_names())
        .map_err(|error| backend_error(error.to_string()))?;
    register_runtime_symbols(&mut builder);
    let libraries = NativeLibraries::load(program).map_err(backend_error)?;
    if !libraries.is_empty() {
        builder.symbol_lookup_fn(Box::new(move |name| libraries.lookup(name)));
    }
    let mut module = JITModule::new(builder);
    let functions = compile_program(&mut module, program, optimization).map_err(backend_error)?;
    module
        .finalize_definitions()
        .map_err(|error| backend_error(error.to_string()))?;
    let function = functions
        .get(&test)
        .copied()
        .ok_or_else(|| backend_error("compiled unit test function is missing"))?;
    let pointer = module.get_finalized_function(function);
    let session = reimer_runtime::ExecutionSession::begin();
    let result = call_jit_unit(pointer);
    reimer_runtime::shutdown_job_pools(session.id());
    reimer_runtime::join_session_threads(session.id());
    result
}

fn register_runtime_symbols(builder: &mut JITBuilder) {
    register_core_symbols(builder);
    register_target_symbols(builder);
    register_filesystem_symbols(builder);
    register_storage_symbols(builder);
    register_coordination_symbols(builder);
}

type RuntimeSymbol = (&'static str, *const u8);

fn register_symbol_group<const COUNT: usize>(
    builder: &mut JITBuilder,
    symbols: [RuntimeSymbol; COUNT],
) {
    for (name, address) in symbols {
        builder.symbol(name, address);
    }
}

fn register_core_symbols(builder: &mut JITBuilder) {
    let symbols = [
        (
            reimer_runtime::FAIL_SYMBOL,
            reimer_runtime::runtime_fail as *const u8,
        ),
        (
            reimer_runtime::PANIC_SYMBOL,
            reimer_runtime::runtime_panic as *const u8,
        ),
        (
            reimer_runtime::ALLOCATE_BYTES_SYMBOL,
            reimer_runtime::allocate_bytes as *const u8,
        ),
        (
            reimer_runtime::DEALLOCATE_BYTES_SYMBOL,
            reimer_runtime::deallocate_bytes as *const u8,
        ),
        (
            reimer_runtime::ABS_I32_SYMBOL,
            reimer_runtime::absolute_i32 as *const u8,
        ),
        (
            reimer_runtime::ARENA_INIT_SYMBOL,
            reimer_runtime::arena_allocator_init as *const u8,
        ),
        (
            reimer_runtime::ARENA_DEINIT_SYMBOL,
            reimer_runtime::arena_allocator_deinit as *const u8,
        ),
        (
            reimer_runtime::FIXED_INIT_SYMBOL,
            reimer_runtime::fixed_buffer_allocator_init as *const u8,
        ),
        (
            reimer_runtime::FIXED_DEINIT_SYMBOL,
            reimer_runtime::fixed_buffer_allocator_deinit as *const u8,
        ),
        (
            reimer_runtime::OUTPUT_WRITE_SYMBOL,
            reimer_runtime::output_write as *const u8,
        ),
        (
            reimer_runtime::OUTPUT_WRITE_ALL_SYMBOL,
            reimer_runtime::output_write_all as *const u8,
        ),
        (
            reimer_runtime::OUTPUT_FLUSH_SYMBOL,
            reimer_runtime::output_flush as *const u8,
        ),
        (
            reimer_runtime::OUTPUT_IS_TERMINAL_SYMBOL,
            reimer_runtime::output_is_terminal as *const u8,
        ),
        (
            reimer_runtime::INPUT_READ_SYMBOL,
            reimer_runtime::input_read as *const u8,
        ),
        (
            reimer_runtime::INPUT_READ_EXACT_SYMBOL,
            reimer_runtime::input_read_exact as *const u8,
        ),
        (
            reimer_runtime::INPUT_READ_LINE_SYMBOL,
            reimer_runtime::input_read_line as *const u8,
        ),
        (
            reimer_runtime::INPUT_READ_TO_END_SYMBOL,
            reimer_runtime::input_read_to_end as *const u8,
        ),
        (
            reimer_runtime::INPUT_IS_TERMINAL_SYMBOL,
            reimer_runtime::input_is_terminal as *const u8,
        ),
        (
            reimer_runtime::BUFFER_EQUALS_SYMBOL,
            reimer_runtime::buffer_equals as *const u8,
        ),
        (
            reimer_runtime::COPY_BYTES_SYMBOL,
            reimer_runtime::copy_bytes as *const u8,
        ),
        (
            reimer_runtime::UTF8_IS_VALID_SYMBOL,
            reimer_runtime::utf8_is_valid as *const u8,
        ),
        (
            reimer_runtime::UTF8_DECODE_NEXT_SYMBOL,
            reimer_runtime::utf8_decode_next as *const u8,
        ),
        (
            reimer_runtime::THREAD_SPAWN_SYMBOL,
            reimer_runtime::thread_spawn as *const u8,
        ),
        (
            reimer_runtime::THREAD_JOIN_SYMBOL,
            reimer_runtime::thread_join as *const u8,
        ),
    ];
    register_symbol_group(builder, symbols);
}

fn register_target_symbols(builder: &mut JITBuilder) {
    register_symbol_group(
        builder,
        [(
            reimer_runtime::TARGET_OS_SYMBOL,
            reimer_runtime::target_os_code as *const u8,
        )],
    );
}

fn register_filesystem_symbols(builder: &mut JITBuilder) {
    let symbols = [
        (
            reimer_runtime::FILE_OPEN_SYMBOL,
            reimer_runtime::file_open as *const u8,
        ),
        (
            reimer_runtime::FILE_CREATE_SYMBOL,
            reimer_runtime::file_create as *const u8,
        ),
        (
            reimer_runtime::FILE_APPEND_SYMBOL,
            reimer_runtime::file_append as *const u8,
        ),
        (
            reimer_runtime::FILE_CLOSE_SYMBOL,
            reimer_runtime::file_close as *const u8,
        ),
        (
            reimer_runtime::FILE_READ_SYMBOL,
            reimer_runtime::file_read as *const u8,
        ),
        (
            reimer_runtime::FILE_READ_EXACT_SYMBOL,
            reimer_runtime::file_read_exact as *const u8,
        ),
        (
            reimer_runtime::FILE_WRITE_SYMBOL,
            reimer_runtime::file_write as *const u8,
        ),
        (
            reimer_runtime::FILE_WRITE_ALL_SYMBOL,
            reimer_runtime::file_write_all as *const u8,
        ),
        (
            reimer_runtime::FILE_FLUSH_SYMBOL,
            reimer_runtime::file_flush as *const u8,
        ),
        (
            reimer_runtime::FILE_REMAINING_LEN_SYMBOL,
            reimer_runtime::file_remaining_len as *const u8,
        ),
        (
            reimer_runtime::PATH_EXISTS_SYMBOL,
            reimer_runtime::path_exists as *const u8,
        ),
        (
            reimer_runtime::PATH_REMOVE_FILE_SYMBOL,
            reimer_runtime::path_remove_file as *const u8,
        ),
        (
            reimer_runtime::PATH_RENAME_SYMBOL,
            reimer_runtime::path_rename as *const u8,
        ),
    ];
    register_symbol_group(builder, symbols);
}

fn register_storage_symbols(builder: &mut JITBuilder) {
    let symbols = [
        (
            reimer_runtime::MUTEX_CREATE_SYMBOL,
            reimer_runtime::mutex_create as *const u8,
        ),
        (
            reimer_runtime::MUTEX_CLONE_SYMBOL,
            reimer_runtime::mutex_clone as *const u8,
        ),
        (
            reimer_runtime::MUTEX_LOAD_SYMBOL,
            reimer_runtime::mutex_load as *const u8,
        ),
        (
            reimer_runtime::MUTEX_REPLACE_SYMBOL,
            reimer_runtime::mutex_replace as *const u8,
        ),
        (
            reimer_runtime::MUTEX_DESTROY_SYMBOL,
            reimer_runtime::mutex_destroy as *const u8,
        ),
        (
            reimer_runtime::RWLOCK_CREATE_SYMBOL,
            reimer_runtime::rwlock_create as *const u8,
        ),
        (
            reimer_runtime::RWLOCK_CLONE_SYMBOL,
            reimer_runtime::rwlock_clone as *const u8,
        ),
        (
            reimer_runtime::RWLOCK_LOAD_SYMBOL,
            reimer_runtime::rwlock_load as *const u8,
        ),
        (
            reimer_runtime::RWLOCK_REPLACE_SYMBOL,
            reimer_runtime::rwlock_replace as *const u8,
        ),
        (
            reimer_runtime::RWLOCK_DESTROY_SYMBOL,
            reimer_runtime::rwlock_destroy as *const u8,
        ),
        (
            reimer_runtime::CHANNEL_CREATE_SYMBOL,
            reimer_runtime::channel_create as *const u8,
        ),
        (
            reimer_runtime::CHANNEL_CLONE_SYMBOL,
            reimer_runtime::channel_clone as *const u8,
        ),
        (
            reimer_runtime::CHANNEL_SEND_SYMBOL,
            reimer_runtime::channel_send as *const u8,
        ),
        (
            reimer_runtime::CHANNEL_RECEIVE_SYMBOL,
            reimer_runtime::channel_receive as *const u8,
        ),
        (
            reimer_runtime::CHANNEL_CLOSE_SYMBOL,
            reimer_runtime::channel_close as *const u8,
        ),
        (
            reimer_runtime::CHANNEL_DESTROY_SYMBOL,
            reimer_runtime::channel_destroy as *const u8,
        ),
        (
            reimer_runtime::THREAD_LOCAL_CREATE_SYMBOL,
            reimer_runtime::thread_local_create as *const u8,
        ),
        (
            reimer_runtime::THREAD_LOCAL_CLONE_SYMBOL,
            reimer_runtime::thread_local_clone as *const u8,
        ),
        (
            reimer_runtime::THREAD_LOCAL_GET_SYMBOL,
            reimer_runtime::thread_local_get as *const u8,
        ),
        (
            reimer_runtime::THREAD_LOCAL_SET_SYMBOL,
            reimer_runtime::thread_local_set as *const u8,
        ),
        (
            reimer_runtime::THREAD_LOCAL_DESTROY_SYMBOL,
            reimer_runtime::thread_local_destroy as *const u8,
        ),
    ];
    register_symbol_group(builder, symbols);
}

fn register_coordination_symbols(builder: &mut JITBuilder) {
    let symbols = [
        (
            reimer_runtime::BARRIER_CREATE_SYMBOL,
            reimer_runtime::barrier_create as *const u8,
        ),
        (
            reimer_runtime::BARRIER_CLONE_SYMBOL,
            reimer_runtime::barrier_clone as *const u8,
        ),
        (
            reimer_runtime::BARRIER_WAIT_SYMBOL,
            reimer_runtime::barrier_wait as *const u8,
        ),
        (
            reimer_runtime::BARRIER_DESTROY_SYMBOL,
            reimer_runtime::barrier_destroy as *const u8,
        ),
        (
            reimer_runtime::SEMAPHORE_CREATE_SYMBOL,
            reimer_runtime::semaphore_create as *const u8,
        ),
        (
            reimer_runtime::SEMAPHORE_CLONE_SYMBOL,
            reimer_runtime::semaphore_clone as *const u8,
        ),
        (
            reimer_runtime::SEMAPHORE_ACQUIRE_SYMBOL,
            reimer_runtime::semaphore_acquire as *const u8,
        ),
        (
            reimer_runtime::SEMAPHORE_TRY_ACQUIRE_SYMBOL,
            reimer_runtime::semaphore_try_acquire as *const u8,
        ),
        (
            reimer_runtime::SEMAPHORE_RELEASE_SYMBOL,
            reimer_runtime::semaphore_release as *const u8,
        ),
        (
            reimer_runtime::SEMAPHORE_DESTROY_SYMBOL,
            reimer_runtime::semaphore_destroy as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_CREATE_SYMBOL,
            reimer_runtime::atomic_create as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_CLONE_SYMBOL,
            reimer_runtime::atomic_clone as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_LOAD_SYMBOL,
            reimer_runtime::atomic_load as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_STORE_SYMBOL,
            reimer_runtime::atomic_store as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_SWAP_SYMBOL,
            reimer_runtime::atomic_swap as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_FETCH_ADD_SYMBOL,
            reimer_runtime::atomic_fetch_add as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_COMPARE_EXCHANGE_SYMBOL,
            reimer_runtime::atomic_compare_exchange as *const u8,
        ),
        (
            reimer_runtime::ATOMIC_DESTROY_SYMBOL,
            reimer_runtime::atomic_destroy as *const u8,
        ),
        (
            reimer_runtime::JOB_POOL_CREATE_SYMBOL,
            reimer_runtime::job_pool_create as *const u8,
        ),
        (
            reimer_runtime::JOB_POOL_CLONE_SYMBOL,
            reimer_runtime::job_pool_clone as *const u8,
        ),
        (
            reimer_runtime::JOB_POOL_DESTROY_SYMBOL,
            reimer_runtime::job_pool_destroy as *const u8,
        ),
        (
            reimer_runtime::JOB_SUBMIT_SYMBOL,
            reimer_runtime::job_submit as *const u8,
        ),
        (
            reimer_runtime::JOB_WAIT_SYMBOL,
            reimer_runtime::job_wait as *const u8,
        ),
        (
            reimer_runtime::JOB_PARALLEL_FOR_SYMBOL,
            reimer_runtime::job_parallel_for as *const u8,
        ),
    ];
    register_symbol_group(builder, symbols);
}

fn host_isa(optimization: OptimizationLevel) -> Result<OwnedTargetIsa, String> {
    let mut flag_builder = settings::builder();
    flag_builder
        .set("enable_llvm_abi_extensions", "true")
        .map_err(|error| error.to_string())?;
    flag_builder
        .set("opt_level", optimization.setting())
        .map_err(|error| error.to_string())?;
    let flags = settings::Flags::new(flag_builder);
    let builder = cranelift_native::builder().map_err(str::to_owned)?;
    builder.finish(flags).map_err(|error| error.to_string())
}

fn compile_program<M: Module>(
    module: &mut M,
    program: &Program,
    optimization: OptimizationLevel,
) -> Result<HashMap<FunctionId, FuncId>, String> {
    let layouts = Layouts::build(&program.types)?;
    let statics = declare_statics(module, program, &layouts)?;
    let functions = declare_functions(module, program)?;
    let thread_thunks = prepare_thread_thunks(module, program)?;
    for function in &program.functions {
        define_function(
            module,
            function,
            &functions,
            &statics,
            &thread_thunks,
            &layouts,
            optimization.debug_assertions(),
        )?;
    }
    Ok(functions)
}

fn declare_statics<M: Module>(
    module: &mut M,
    program: &Program,
    layouts: &Layouts,
) -> Result<HashMap<StaticId, DataId>, String> {
    let mut declarations = HashMap::with_capacity(program.statics.len());
    for value in &program.statics {
        let linkage = if value.is_public {
            Linkage::Export
        } else {
            Linkage::Local
        };
        let data = module
            .declare_data(
                &mangle_static_name(&value.name),
                linkage,
                value.mutable,
                false,
            )
            .map_err(|error| error.to_string())?;
        let layout = layouts.value_layout(value.ty)?;
        let mut description = DataDescription::new();
        description.set_align(u64::from(layout.align));
        description.define(serialize_static_initializer(value, layouts)?.into_boxed_slice());
        module
            .define_data(data, &description)
            .map_err(|error| error.to_string())?;
        declarations.insert(value.id, data);
    }
    Ok(declarations)
}

fn serialize_static_initializer(value: &Static, layouts: &Layouts) -> Result<Vec<u8>, String> {
    let layout = layouts.value_layout(value.ty)?;
    let length = usize::try_from(layout.size.max(1))
        .map_err(|_| "static storage size exceeds the host address space".to_owned())?;
    let mut bytes = vec![0; length];
    write_static_value(&mut bytes, 0, &value.initializer, layouts)?;
    Ok(bytes)
}

fn write_static_value(
    bytes: &mut [u8],
    offset: u32,
    expression: &Expression,
    layouts: &Layouts,
) -> Result<(), String> {
    match &expression.kind {
        ExpressionKind::Integer(value) => {
            write_static_integer(bytes, offset, expression.ty, *value, layouts)
        }
        ExpressionKind::Unary {
            operator: UnaryOperator::Negate,
            operand,
        } if operand.ty.is_integer() => {
            let ExpressionKind::Integer(magnitude) = operand.kind else {
                return Err("static negative integer initializer is not canonical".to_owned());
            };
            let bits = integer_width(expression.ty)?;
            let mask = if bits == 128 {
                u128::MAX
            } else {
                (1_u128 << bits) - 1
            };
            let value = 0_u128.wrapping_sub(magnitude) & mask;
            write_static_integer(bytes, offset, expression.ty, value, layouts)
        }
        ExpressionKind::Float32(value) => write_static_bytes(bytes, offset, &value.to_ne_bytes()),
        ExpressionKind::Float64(value) => write_static_bytes(bytes, offset, &value.to_ne_bytes()),
        ExpressionKind::Character(value) => {
            write_static_bytes(bytes, offset, &u32::from(*value).to_ne_bytes())
        }
        ExpressionKind::Boolean(value) => write_static_bytes(bytes, offset, &[u8::from(*value)]),
        ExpressionKind::Unit => Ok(()),
        ExpressionKind::Tuple(fields) | ExpressionKind::Struct(fields) => {
            let layout = layouts.aggregate(expression.ty)?;
            let AggregateLayoutKind::Product { offsets } = &layout.kind else {
                return Err("static product initializer has no product layout".to_owned());
            };
            if fields.len() != offsets.len() {
                return Err("static product initializer does not match its layout".to_owned());
            }
            for (field, field_offset) in fields.iter().zip(offsets) {
                write_static_value(
                    bytes,
                    offset
                        .checked_add(*field_offset)
                        .ok_or_else(|| "static field offset overflowed".to_owned())?,
                    field,
                    layouts,
                )?;
            }
            Ok(())
        }
        ExpressionKind::Array(elements) => {
            let layout = layouts.aggregate(expression.ty)?;
            let AggregateLayoutKind::Array { stride, length } = layout.kind else {
                return Err("static array initializer has no array layout".to_owned());
            };
            if u64::try_from(elements.len()).ok() != Some(length) {
                return Err("static array initializer does not match its layout".to_owned());
            }
            for (index, element) in elements.iter().enumerate() {
                let index = u32::try_from(index)
                    .map_err(|_| "static array index exceeds u32".to_owned())?;
                write_static_value(
                    bytes,
                    offset
                        .checked_add(
                            stride
                                .checked_mul(index)
                                .ok_or_else(|| "static array offset overflowed".to_owned())?,
                        )
                        .ok_or_else(|| "static array offset overflowed".to_owned())?,
                    element,
                    layouts,
                )?;
            }
            Ok(())
        }
        ExpressionKind::Enum { variant, fields } => {
            let layout = layouts.aggregate(expression.ty)?;
            let AggregateLayoutKind::Enum { variants } = &layout.kind else {
                return Err("static enum initializer has no enum layout".to_owned());
            };
            let offsets = variants
                .get(type_index(TypeId(*variant))?)
                .ok_or_else(|| format!("static enum variant {variant} has no layout"))?;
            if fields.len() != offsets.len() {
                return Err("static enum initializer does not match its layout".to_owned());
            }
            write_static_bytes(bytes, offset, &variant.to_ne_bytes())?;
            for (field, field_offset) in fields.iter().zip(offsets) {
                write_static_value(
                    bytes,
                    offset
                        .checked_add(*field_offset)
                        .ok_or_else(|| "static enum field offset overflowed".to_owned())?,
                    field,
                    layouts,
                )?;
            }
            Ok(())
        }
        _ => Err("static initializer contains a runtime-only expression".to_owned()),
    }
}

fn write_static_integer(
    bytes: &mut [u8],
    offset: u32,
    ty: Type,
    value: u128,
    layouts: &Layouts,
) -> Result<(), String> {
    let size = usize::try_from(layouts.value_layout(ty)?.size)
        .map_err(|_| "static integer size exceeds usize".to_owned())?;
    let encoded = value.to_ne_bytes();
    write_static_bytes(bytes, offset, &encoded[..size])
}

fn write_static_bytes(bytes: &mut [u8], offset: u32, value: &[u8]) -> Result<(), String> {
    let start =
        usize::try_from(offset).map_err(|_| "static byte offset exceeds usize".to_owned())?;
    let end = start
        .checked_add(value.len())
        .ok_or_else(|| "static byte range overflowed".to_owned())?;
    let destination = bytes
        .get_mut(start..end)
        .ok_or_else(|| "static initializer exceeds its declared layout".to_owned())?;
    destination.copy_from_slice(value);
    Ok(())
}

fn integer_width(ty: Type) -> Result<u32, String> {
    match ty {
        Type::I8 | Type::U8 => Ok(8),
        Type::I16 | Type::U16 => Ok(16),
        Type::I32 | Type::U32 | Type::Char => Ok(32),
        Type::I64 | Type::U64 => Ok(64),
        Type::I128 | Type::U128 => Ok(128),
        Type::Isize | Type::Usize => Ok(usize::BITS),
        _ => Err(format!("type `{ty}` is not an integer")),
    }
}

fn prepare_thread_thunks<M: Module>(
    module: &mut M,
    program: &Program,
) -> Result<HashMap<ThreadThunkKey, FuncId>, String> {
    let mut thunks = HashMap::new();
    for definition in &program.types {
        let TypeDefinitionKind::Function {
            parameters,
            return_type,
        } = &definition.kind
        else {
            continue;
        };
        let [argument_type] = parameters.as_slice() else {
            continue;
        };
        let key = ThreadThunkKey {
            argument_type: *argument_type,
            result_type: *return_type,
        };
        if thunks.contains_key(&key) {
            continue;
        }
        let signature = thread_thunk_signature(module);
        let name = format!("thread_thunk_{}", thunks.len());
        let thunk = module
            .declare_function(&name, Linkage::Local, &signature)
            .map_err(|error| error.to_string())?;
        define_thread_thunk(module, key, thunk)?;
        thunks.insert(key, thunk);
    }
    Ok(thunks)
}

fn thread_thunk_signature<M: Module>(module: &M) -> ir::Signature {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature
}

fn define_thread_thunk<M: Module>(
    module: &mut M,
    key: ThreadThunkKey,
    thunk: FuncId,
) -> Result<(), String> {
    let target_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = thread_thunk_signature(module);
    let mut function_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry = builder.create_block();
        builder.switch_to_block(entry);
        builder.append_block_params_for_function_params(entry);
        let parameters = builder.block_params(entry);
        let [callback, argument_address, result_address] = parameters else {
            return Err("thread thunk ABI parameters are missing".to_owned());
        };
        let callback = *callback;
        let argument_address = *argument_address;
        let result_address = *result_address;
        let signature = typed_function_signature(module, [key.argument_type], key.result_type)?;
        let signature = builder.import_signature(signature);
        let mut arguments = Vec::with_capacity(2);
        if key.result_type.is_composite() {
            arguments.push(result_address);
        }
        if key.argument_type.has_runtime_value() {
            let argument = if key.argument_type.is_composite() {
                argument_address
            } else {
                builder.ins().load(
                    runtime_type(key.argument_type)?,
                    MemFlagsData::new(),
                    argument_address,
                    0,
                )
            };
            arguments.push(argument);
        }
        let call = builder.ins().call_indirect(signature, callback, &arguments);
        if key.result_type.has_runtime_value() && !key.result_type.is_composite() {
            let value =
                builder.inst_results(call).first().copied().ok_or_else(|| {
                    "thread callback did not return its declared value".to_owned()
                })?;
            builder
                .ins()
                .store(MemFlagsData::new(), value, result_address, 0);
        }
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize(target_config);
    }
    module
        .define_function(thunk, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(())
}

fn declare_functions<M: Module>(
    module: &mut M,
    program: &Program,
) -> Result<HashMap<FunctionId, FuncId>, String> {
    let mut declarations =
        HashMap::with_capacity(program.functions.len() + program.extern_functions.len());

    for function in &program.extern_functions {
        let signature = extern_function_signature(module, function)?;
        let id = module
            .declare_function(&function.symbol, Linkage::Import, &signature)
            .map_err(|error| error.to_string())?;
        declarations.insert(function.id, id);
    }

    for function in &program.functions {
        let signature = function_signature(module, function)?;
        let linkage = if function.is_public || Some(function.id) == program.entry {
            Linkage::Export
        } else {
            Linkage::Local
        };
        let symbol = symbol_name(function, program.entry);
        let id = module
            .declare_function(&symbol, linkage, &signature)
            .map_err(|error| error.to_string())?;
        declarations.insert(function.id, id);
    }
    Ok(declarations)
}

fn extern_function_signature<M: Module>(
    module: &M,
    function: &reimer_hir::ExternFunction,
) -> Result<ir::Signature, String> {
    let mut signature = module.make_signature();
    for parameter in &function.parameters {
        if parameter.ty.has_runtime_value() {
            signature
                .params
                .push(AbiParam::new(runtime_type(parameter.ty)?));
        }
    }
    if function.return_type.has_runtime_value() {
        signature
            .returns
            .push(AbiParam::new(runtime_type(function.return_type)?));
    }
    Ok(signature)
}

fn function_signature<M: Module>(module: &M, function: &Function) -> Result<ir::Signature, String> {
    typed_function_signature(
        module,
        function.parameters.iter().map(|parameter| parameter.ty),
        function.return_type,
    )
}

fn typed_function_signature<M: Module>(
    module: &M,
    parameters: impl IntoIterator<Item = Type>,
    return_type: Type,
) -> Result<ir::Signature, String> {
    let mut signature = module.make_signature();
    if return_type.is_composite() {
        signature.params.push(AbiParam::new(pointer_type()));
    }
    for parameter in parameters {
        if parameter.has_runtime_value() {
            signature
                .params
                .push(AbiParam::new(runtime_type(parameter)?));
        }
    }
    if return_type.has_runtime_value() && !return_type.is_composite() {
        signature
            .returns
            .push(AbiParam::new(runtime_type(return_type)?));
    }
    Ok(signature)
}

fn runtime_fail_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(reimer_runtime::FAIL_SYMBOL, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_panic_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    let function = module
        .declare_function(reimer_runtime::PANIC_SYMBOL, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_allocate_bytes_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature
        .params
        .extend([AbiParam::new(pointer_type()), AbiParam::new(pointer_type())]);
    signature.returns.push(AbiParam::new(pointer_type()));
    let function = module
        .declare_function(
            reimer_runtime::ALLOCATE_BYTES_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_deallocate_bytes_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    let function = module
        .declare_function(
            reimer_runtime::DEALLOCATE_BYTES_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_buffer_equals_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I8));
    let function = module
        .declare_function(
            reimer_runtime::BUFFER_EQUALS_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_utf8_decode_next_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(
            reimer_runtime::UTF8_DECODE_NEXT_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_thread_spawn_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(pointer_type()));
    let function = module
        .declare_function(
            reimer_runtime::THREAD_SPAWN_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_thread_join_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(
            reimer_runtime::THREAD_JOIN_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_job_submit_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(pointer_type()));
    let function = module
        .declare_function(
            reimer_runtime::JOB_SUBMIT_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_job_wait_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(reimer_runtime::JOB_WAIT_SYMBOL, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_parallel_for_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(pointer_type()));
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(
            reimer_runtime::JOB_PARALLEL_FOR_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_synchronization_create_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    synchronization: SynchronizationKind,
) -> Result<ir::FuncRef, String> {
    let symbol = match synchronization {
        SynchronizationKind::Mutex => reimer_runtime::MUTEX_CREATE_SYMBOL,
        SynchronizationKind::RwLock => reimer_runtime::RWLOCK_CREATE_SYMBOL,
        SynchronizationKind::ThreadLocal => reimer_runtime::THREAD_LOCAL_CREATE_SYMBOL,
    };
    let mut signature = module.make_signature();
    signature
        .params
        .extend([AbiParam::new(pointer_type()), AbiParam::new(pointer_type())]);
    signature.returns.push(AbiParam::new(pointer_type()));
    let function = module
        .declare_function(symbol, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_synchronization_load_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    synchronization: SynchronizationKind,
) -> Result<ir::FuncRef, String> {
    let symbol = match synchronization {
        SynchronizationKind::Mutex => reimer_runtime::MUTEX_LOAD_SYMBOL,
        SynchronizationKind::RwLock => reimer_runtime::RWLOCK_LOAD_SYMBOL,
        SynchronizationKind::ThreadLocal => reimer_runtime::THREAD_LOCAL_GET_SYMBOL,
    };
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(symbol, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_synchronization_replace_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    synchronization: SynchronizationKind,
) -> Result<ir::FuncRef, String> {
    let symbol = match synchronization {
        SynchronizationKind::Mutex => reimer_runtime::MUTEX_REPLACE_SYMBOL,
        SynchronizationKind::RwLock => reimer_runtime::RWLOCK_REPLACE_SYMBOL,
        SynchronizationKind::ThreadLocal => {
            return Err("thread-local storage does not support replace-and-load".to_owned());
        }
    };
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(symbol, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_thread_local_store_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    runtime_sized_buffer_reference(builder, module, reimer_runtime::THREAD_LOCAL_SET_SYMBOL)
}

fn runtime_channel_send_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    runtime_sized_buffer_reference(builder, module, reimer_runtime::CHANNEL_SEND_SYMBOL)
}

fn runtime_channel_receive_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    runtime_sized_buffer_reference(builder, module, reimer_runtime::CHANNEL_RECEIVE_SYMBOL)
}

fn runtime_sized_buffer_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    symbol: &str,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature.params.extend([
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
        AbiParam::new(pointer_type()),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(symbol, Linkage::Import, &signature)
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn runtime_channel_create_reference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
) -> Result<ir::FuncRef, String> {
    let mut signature = module.make_signature();
    signature
        .params
        .extend([AbiParam::new(pointer_type()), AbiParam::new(pointer_type())]);
    signature.returns.push(AbiParam::new(pointer_type()));
    let function = module
        .declare_function(
            reimer_runtime::CHANNEL_CREATE_SYMBOL,
            Linkage::Import,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    Ok(module.declare_func_in_func(function, builder.func))
}

fn emit_runtime_failure<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    failure: Failure,
) -> Result<(), String> {
    let function = runtime_fail_reference(builder, module)?;
    let code = builder.ins().iconst(types::I32, i64::from(failure as u32));
    builder.ins().call(function, &[code]);
    builder.ins().trap(TrapCode::unwrap_user(1));
    Ok(())
}

fn emit_runtime_failure_if<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    condition: Value,
    failure: Failure,
) -> Result<(), String> {
    let failed = builder.create_block();
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(condition, failed, &[], continuation, &[]);
    builder.switch_to_block(failed);
    emit_runtime_failure(builder, module, failure)?;
    builder.switch_to_block(continuation);
    Ok(())
}

fn symbol_name(function: &Function, entry: Option<FunctionId>) -> String {
    if Some(function.id) == entry {
        ENTRY_SYMBOL.to_owned()
    } else {
        mangle_function_name(&function.name)
    }
}

fn mangle_function_name(name: &str) -> String {
    mangle_symbol_name("function_", name)
}

fn mangle_static_name(name: &str) -> String {
    mangle_symbol_name("static_", name)
}

fn mangle_symbol_name(prefix: &str, name: &str) -> String {
    let mut symbol = prefix.to_owned();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            symbol.push(char::from(byte));
        } else {
            symbol.push('_');
            let _ = write!(symbol, "{byte:02x}");
        }
    }
    symbol
}

fn define_function<M: Module>(
    module: &mut M,
    function: &Function,
    functions: &HashMap<FunctionId, FuncId>,
    statics: &HashMap<StaticId, DataId>,
    thread_thunks: &HashMap<ThreadThunkKey, FuncId>,
    layouts: &Layouts,
    debug_assertions: bool,
) -> Result<(), String> {
    let function_id = functions
        .get(&function.id)
        .copied()
        .ok_or_else(|| format!("function `{}` was not declared", function.name))?;
    let signature = function_signature(module, function)?;
    let target_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = signature;
    let mut function_context = FunctionBuilderContext::new();

    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);
        builder.append_block_params_for_function_params(entry_block);
        let parameter_values = builder.block_params(entry_block).to_vec();
        let mut parameter_values = parameter_values.into_iter();
        let return_destination = if function.return_type.is_composite() {
            Some(
                parameter_values
                    .next()
                    .ok_or_else(|| "aggregate return destination is missing".to_owned())?,
            )
        } else {
            None
        };
        let mut state = CodegenState::new(
            layouts,
            statics,
            thread_thunks,
            return_destination,
            target_config,
            debug_assertions,
        );

        for parameter in &function.parameters {
            if !parameter.ty.has_runtime_value() {
                continue;
            }
            let value = parameter_values
                .next()
                .ok_or_else(|| format!("parameter `{}` has no ABI value", parameter.name))?;
            define_local(
                &mut builder,
                &mut state,
                parameter.local,
                parameter.ty,
                value,
            )?;
        }

        let emitted = emit_block(&mut builder, module, functions, &mut state, &function.body)?;
        if !emitted.terminated {
            if function.return_type.is_composite() {
                let source = require_value(emitted, "function body")?;
                let destination = state
                    .return_destination
                    .ok_or_else(|| "aggregate return destination is missing".to_owned())?;
                copy_composite(
                    &mut builder,
                    state.layouts,
                    state.target_config,
                    function.return_type,
                    destination,
                    source,
                )?;
                builder.ins().return_(&[]);
            } else if function.return_type.has_runtime_value() {
                let value = require_value(emitted, "function body")?;
                builder.ins().return_(&[value]);
            } else {
                builder.ins().return_(&[]);
            }
        }
        builder.seal_all_blocks();
        builder.finalize(target_config);
    }

    module
        .define_function(function_id, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct LoopTargets {
    continue_target: ir::Block,
    exit: ir::Block,
    result_type: Type,
    defer_depth: usize,
}

struct ForParts<'hir> {
    pattern: &'hir Pattern,
    element_type: Type,
    iteration: ForIteration,
    iterable: &'hir Expression,
    body: &'hir Block,
}

struct AllocateBytesParts<'hir> {
    allocator: &'hir Expression,
    length: &'hir Expression,
    result_type: Type,
    allocation_type: Type,
    error_type: Type,
    error_variant: u32,
}

struct ThreadCallbackParts<'hir> {
    callback: &'hir Expression,
    argument: &'hir Expression,
    output_type: Type,
}

#[derive(Debug, Clone, Copy)]
struct ThreadJoinFailures {
    invalid_handle: u32,
    worker_panicked: u32,
    result_mismatch: u32,
}

#[derive(Debug, Clone, Copy)]
struct ThreadScopeFailures {
    spawn_failed: u32,
    join: ThreadJoinFailures,
}

struct ThreadSpawnParts<'hir> {
    callback: ThreadCallbackParts<'hir>,
    result_type: Type,
    error_type: Type,
    spawn_failed_variant: u32,
}

struct ThreadJoinParts<'hir> {
    handle: &'hir Expression,
    result_type: Type,
    output_type: Type,
    error_type: Type,
    failures: ThreadJoinFailures,
}

struct ThreadScopeParts<'hir> {
    callback: ThreadCallbackParts<'hir>,
    result_type: Type,
    error_type: Type,
    failures: ThreadScopeFailures,
}

struct SynchronizationCreateParts<'hir> {
    value: &'hir Expression,
    synchronization: SynchronizationKind,
}

struct SynchronizationLoadParts<'hir> {
    handle: &'hir Expression,
    value_type: Type,
    synchronization: SynchronizationKind,
}

struct SynchronizationReplaceParts<'hir> {
    handle: &'hir Expression,
    value: &'hir Expression,
    synchronization: SynchronizationKind,
}

struct ChannelSendParts<'hir> {
    handle: &'hir Expression,
    value: &'hir Expression,
    result_type: Type,
    error_type: Type,
    closed_variant: u32,
}

struct ChannelReceiveParts<'hir> {
    handle: &'hir Expression,
    result_type: Type,
    value_type: Type,
    error_type: Type,
    closed_variant: u32,
}

#[derive(Debug, Clone, Copy)]
struct ChannelResultLowering {
    status: Value,
    result_type: Type,
    value_type: Type,
    destination: Option<Value>,
    error_type: Type,
    closed_variant: u32,
}

struct JobSubmitParts<'hir> {
    pool: &'hir Expression,
    callback: ThreadCallbackParts<'hir>,
    result_type: Type,
    error_type: Type,
    submit_failed_variant: u32,
}

struct JobWaitParts<'hir> {
    handle: &'hir Expression,
    result_type: Type,
    output_type: Type,
    error_type: Type,
    failures: ThreadJoinFailures,
}

#[derive(Debug, Clone, Copy)]
struct ParallelFailures {
    submit_failed: u32,
    worker_panicked: u32,
    result_mismatch: u32,
}

struct ParallelForParts<'hir> {
    pool: &'hir Expression,
    slice: &'hir Expression,
    chunk_type: Type,
    array_length: Option<u64>,
    callback: &'hir Expression,
    minimum_chunk: &'hir Expression,
    result_type: Type,
    error_type: Type,
    failures: ParallelFailures,
}

#[derive(Debug, Clone, Copy)]
struct ParallelRequestValues {
    pool: Value,
    thunk: Value,
    callback: Value,
    data: Value,
    length: Value,
    element_size: Value,
    minimum_chunk: Value,
    descriptor_size: Value,
    data_offset: Value,
    length_offset: Value,
}

#[derive(Debug, Clone, Copy)]
struct ThreadJoinLowering {
    handle: Value,
    result_type: Type,
    output_type: Type,
    error_type: Type,
    failures: ThreadJoinFailures,
}

#[derive(Debug, Clone, Copy)]
enum CallbackWaitKind {
    Thread,
    Job,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ThreadThunkKey {
    argument_type: Type,
    result_type: Type,
}

#[derive(Debug, Clone, Copy)]
struct BranchTargets {
    success: ir::Block,
    failure: ir::Block,
}

struct CodegenState<'layouts> {
    locals: HashMap<LocalId, LocalStorage>,
    loops: Vec<LoopTargets>,
    layouts: &'layouts Layouts,
    statics: &'layouts HashMap<StaticId, DataId>,
    thread_thunks: &'layouts HashMap<ThreadThunkKey, FuncId>,
    return_destination: Option<Value>,
    target_config: TargetFrontendConfig,
    debug_assertions: bool,
    defer_scopes: Vec<Vec<Expression>>,
}

#[derive(Debug, Clone, Copy)]
enum LocalStorage {
    Variable(Variable),
    Address(Value),
}

impl<'layouts> CodegenState<'layouts> {
    fn new(
        layouts: &'layouts Layouts,
        statics: &'layouts HashMap<StaticId, DataId>,
        thread_thunks: &'layouts HashMap<ThreadThunkKey, FuncId>,
        return_destination: Option<Value>,
        target_config: TargetFrontendConfig,
        debug_assertions: bool,
    ) -> Self {
        Self {
            locals: HashMap::new(),
            loops: Vec::new(),
            layouts,
            statics,
            thread_thunks,
            return_destination,
            target_config,
            debug_assertions,
            defer_scopes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Emitted {
    value: Option<Value>,
    terminated: bool,
}

impl Emitted {
    const fn value(value: Value) -> Self {
        Self {
            value: Some(value),
            terminated: false,
        }
    }

    const fn unit() -> Self {
        Self {
            value: None,
            terminated: false,
        }
    }

    const fn terminated() -> Self {
        Self {
            value: None,
            terminated: true,
        }
    }
}

fn emit_block<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    block: &Block,
) -> Result<Emitted, String> {
    state.defer_scopes.push(Vec::new());
    for statement in &block.statements {
        let emitted = emit_statement(builder, module, functions, state, statement)?;
        if emitted.terminated {
            state.defer_scopes.pop();
            return Ok(emitted);
        }
    }

    let emitted = block.tail.as_deref().map_or(Ok(Emitted::unit()), |tail| {
        emit_expression(builder, module, functions, state, tail)
    })?;
    if emitted.terminated {
        state.defer_scopes.pop();
        return Ok(emitted);
    }
    let emitted = preserve_deferred_value(builder, state, block.ty, emitted)?;
    let cleanup = emit_cleanup_range(
        builder,
        module,
        functions,
        state,
        state.defer_scopes.len().saturating_sub(1),
    )?;
    state.defer_scopes.pop();
    if cleanup.terminated {
        Ok(cleanup)
    } else {
        Ok(emitted)
    }
}

fn emit_statement<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    statement: &Statement,
) -> Result<Emitted, String> {
    match statement {
        Statement::Let {
            local,
            ty,
            initializer,
            ..
        } => {
            let emitted = emit_expression(builder, module, functions, state, initializer)?;
            if emitted.terminated {
                return Ok(emitted);
            }
            if ty.has_runtime_value() {
                let value = require_value(emitted, "binding initializer")?;
                define_local(builder, state, *local, *ty, value)?;
            }
            Ok(Emitted::unit())
        }
        Statement::Expression(expression) => {
            emit_expression(builder, module, functions, state, expression)
        }
        Statement::Defer { action, .. } => {
            let scope = state
                .defer_scopes
                .last_mut()
                .ok_or_else(|| "defer statement has no active scope".to_owned())?;
            scope.push(action.clone());
            Ok(Emitted::unit())
        }
        Statement::Return { value, .. } => {
            emit_return_statement(builder, module, functions, state, value.as_ref())
        }
        Statement::While {
            condition, body, ..
        } => emit_while(builder, module, functions, state, condition, body),
        Statement::For { .. } => emit_for_statement(builder, module, functions, state, statement),
        Statement::Break { value, .. } => {
            let targets = state
                .loops
                .last()
                .copied()
                .ok_or_else(|| "resolved `break` has no loop target".to_owned())?;
            let emitted = value.as_ref().map_or(Ok(Emitted::unit()), |value| {
                emit_expression(builder, module, functions, state, value)
            })?;
            if emitted.terminated {
                return Ok(emitted);
            }
            let emitted = preserve_value_for_cleanup(
                builder,
                state,
                targets.result_type,
                emitted,
                targets.defer_depth,
            )?;
            let cleanup =
                emit_cleanup_range(builder, module, functions, state, targets.defer_depth)?;
            if cleanup.terminated {
                return Ok(cleanup);
            }
            jump_to_merge(
                builder,
                targets.exit,
                targets.result_type,
                emitted,
                "break value",
            )?;
            Ok(Emitted::terminated())
        }
        Statement::Continue(_) => {
            let targets = state
                .loops
                .last()
                .copied()
                .ok_or_else(|| "resolved `continue` has no loop target".to_owned())?;
            let cleanup =
                emit_cleanup_range(builder, module, functions, state, targets.defer_depth)?;
            if cleanup.terminated {
                return Ok(cleanup);
            }
            builder.ins().jump(targets.continue_target, &[]);
            Ok(Emitted::terminated())
        }
    }
}

fn emit_for_statement<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    statement: &Statement,
) -> Result<Emitted, String> {
    let Statement::For {
        pattern,
        element_type,
        iteration,
        iterable,
        body,
        ..
    } = statement
    else {
        return Err("for lowering received a different statement".to_owned());
    };
    emit_for(
        builder,
        module,
        functions,
        state,
        &ForParts {
            pattern,
            element_type: *element_type,
            iteration: *iteration,
            iterable,
            body,
        },
    )
}

fn preserve_deferred_value(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    ty: Type,
    emitted: Emitted,
) -> Result<Emitted, String> {
    let start = state.defer_scopes.len().saturating_sub(1);
    preserve_value_for_cleanup(builder, state, ty, emitted, start)
}

fn preserve_value_for_cleanup(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    ty: Type,
    emitted: Emitted,
    start: usize,
) -> Result<Emitted, String> {
    let has_deferred_actions = state
        .defer_scopes
        .iter()
        .skip(start)
        .any(|scope| !scope.is_empty());
    if !has_deferred_actions || !ty.is_composite() {
        return Ok(emitted);
    }
    let source = require_value(emitted, "scope result")?;
    let destination = allocate_composite(builder, state.layouts, ty)?;
    copy_composite(
        builder,
        state.layouts,
        state.target_config,
        ty,
        destination,
        source,
    )?;
    Ok(Emitted::value(destination))
}

fn emit_cleanup_range<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    start: usize,
) -> Result<Emitted, String> {
    let actions = state
        .defer_scopes
        .iter()
        .skip(start)
        .rev()
        .flat_map(|scope| scope.iter().rev())
        .cloned()
        .collect::<Vec<_>>();
    for action in &actions {
        let emitted = emit_expression(builder, module, functions, state, action)?;
        if emitted.terminated {
            return Ok(emitted);
        }
    }
    Ok(Emitted::unit())
}

fn emit_return_statement<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    value: Option<&Expression>,
) -> Result<Emitted, String> {
    let emitted = value.map_or(Ok(Emitted::unit()), |value| {
        emit_expression(builder, module, functions, state, value)
    })?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let scalar_result = if let Some(value) = value {
        if value.ty.is_composite() {
            let source = require_value(emitted, "return value")?;
            let destination = state
                .return_destination
                .ok_or_else(|| "aggregate return destination is missing".to_owned())?;
            copy_composite(
                builder,
                state.layouts,
                state.target_config,
                value.ty,
                destination,
                source,
            )?;
            None
        } else if value.ty.has_runtime_value() {
            Some(require_value(emitted, "return value")?)
        } else {
            None
        }
    } else {
        None
    };
    let cleanup = emit_cleanup_range(builder, module, functions, state, 0)?;
    if cleanup.terminated {
        return Ok(cleanup);
    }
    if let Some(result) = scalar_result {
        builder.ins().return_(&[result]);
    } else {
        builder.ins().return_(&[]);
    }
    Ok(Emitted::terminated())
}

fn emit_while<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    condition: &Expression,
    body: &Block,
) -> Result<Emitted, String> {
    let header = builder.create_block();
    let body_block = builder.create_block();
    let exit = builder.create_block();
    builder.ins().jump(header, &[]);
    builder.switch_to_block(header);

    let condition = emit_expression(builder, module, functions, state, condition)?;
    if condition.terminated {
        return Ok(condition);
    }
    let condition = require_value(condition, "while condition")?;
    builder.ins().brif(condition, body_block, &[], exit, &[]);

    builder.switch_to_block(body_block);
    state.loops.push(LoopTargets {
        continue_target: header,
        exit,
        result_type: Type::Unit,
        defer_depth: state.defer_scopes.len(),
    });
    let emitted = emit_block(builder, module, functions, state, body)?;
    state.loops.pop();
    if !emitted.terminated {
        builder.ins().jump(header, &[]);
    }

    builder.switch_to_block(exit);
    Ok(Emitted::unit())
}

fn emit_for<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ForParts<'_>,
) -> Result<Emitted, String> {
    match parts.iteration {
        ForIteration::Indexed => emit_indexed_for(builder, module, functions, state, parts),
        ForIteration::Chars => emit_chars_for(builder, module, functions, state, parts),
    }
}

fn emit_indexed_for<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ForParts<'_>,
) -> Result<Emitted, String> {
    let iterable = emit_expression(builder, module, functions, state, parts.iterable)?;
    if iterable.terminated {
        return Ok(iterable);
    }
    let iterable = require_value(iterable, "for iterable")?;
    let (base, length_value, stride) =
        indexed_sequence_parts(builder, state.layouts, parts.iterable.ty, iterable)?;
    let index = builder.declare_var(pointer_type());
    let zero = builder.ins().iconst(pointer_type(), 0);
    builder.def_var(index, zero);
    let header = builder.create_block();
    let body_block = builder.create_block();
    let latch = builder.create_block();
    let exit = builder.create_block();
    builder.ins().jump(header, &[]);

    builder.switch_to_block(header);
    let current = builder.use_var(index);
    let has_next = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, current, length_value);
    builder.ins().brif(has_next, body_block, &[], exit, &[]);

    builder.switch_to_block(body_block);
    let byte_offset = builder.ins().imul_imm_u(current, i64::from(stride));
    let source = builder.ins().iadd(base, byte_offset);
    let element = copy_or_load_for_binding(builder, state, parts.element_type, source)?;
    bind_pattern(builder, state, parts.pattern, element)?;
    state.loops.push(LoopTargets {
        continue_target: latch,
        exit,
        result_type: Type::Unit,
        defer_depth: state.defer_scopes.len(),
    });
    let emitted = emit_block(builder, module, functions, state, parts.body)?;
    state.loops.pop();
    if !emitted.terminated {
        builder.ins().jump(latch, &[]);
    }

    builder.switch_to_block(latch);
    let current = builder.use_var(index);
    let next = builder.ins().iadd_imm_u(current, 1);
    builder.def_var(index, next);
    builder.ins().jump(header, &[]);

    builder.switch_to_block(exit);
    Ok(Emitted::unit())
}

fn emit_chars_for<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ForParts<'_>,
) -> Result<Emitted, String> {
    let iterable = emit_expression(builder, module, functions, state, parts.iterable)?;
    if iterable.terminated {
        return Ok(iterable);
    }
    let iterator = require_value(iterable, "character iterator")?;
    let (data, length, cursor_address) =
        chars_iterator_parts(builder, state.layouts, parts.iterable.ty, iterator)?;
    let header = builder.create_block();
    let body_block = builder.create_block();
    let exit = builder.create_block();
    builder.ins().jump(header, &[]);

    builder.switch_to_block(header);
    let cursor = builder
        .ins()
        .load(pointer_type(), MemFlagsData::new(), cursor_address, 0);
    let (has_character, character, next_cursor) =
        decode_next_character(builder, module, data, length, cursor)?;
    builder
        .ins()
        .store(MemFlagsData::new(), next_cursor, cursor_address, 0);
    builder
        .ins()
        .brif(has_character, body_block, &[], exit, &[]);

    builder.switch_to_block(body_block);
    bind_pattern(builder, state, parts.pattern, character)?;
    state.loops.push(LoopTargets {
        continue_target: header,
        exit,
        result_type: Type::Unit,
        defer_depth: state.defer_scopes.len(),
    });
    let emitted = emit_block(builder, module, functions, state, parts.body)?;
    state.loops.pop();
    if !emitted.terminated {
        builder.ins().jump(header, &[]);
    }

    builder.switch_to_block(exit);
    Ok(Emitted::unit())
}

fn copy_or_load_for_binding(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    ty: Type,
    source: Value,
) -> Result<Value, String> {
    if !ty.has_runtime_value() {
        Ok(builder.ins().iconst(types::I8, 0))
    } else if ty.is_composite() {
        let destination = allocate_composite(builder, state.layouts, ty)?;
        copy_composite(
            builder,
            state.layouts,
            state.target_config,
            ty,
            destination,
            source,
        )?;
        Ok(destination)
    } else {
        let emitted = load_at_address(builder, ty, source)?;
        require_value(emitted, "for element")
    }
}

fn emit_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    let result_type = expression.ty;
    match &expression.kind {
        ExpressionKind::Integer(_)
        | ExpressionKind::Float32(_)
        | ExpressionKind::Float64(_)
        | ExpressionKind::Character(_)
        | ExpressionKind::String(_)
        | ExpressionKind::CString(_)
        | ExpressionKind::Boolean(_)
        | ExpressionKind::Unit => emit_literal(builder, state, expression),
        ExpressionKind::Tuple(elements)
        | ExpressionKind::Array(elements)
        | ExpressionKind::Struct(elements) => {
            emit_product(builder, module, functions, state, expression.ty, elements)
        }
        ExpressionKind::Enum { .. } => {
            emit_enum_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::Local(local) => emit_local(builder, state, *local, expression.ty),
        ExpressionKind::Static(value) => emit_static(builder, module, state, *value, expression.ty),
        ExpressionKind::Unary { operator, operand } => {
            emit_unary(builder, module, functions, state, *operator, operand)
        }
        ExpressionKind::Binary {
            operator,
            left,
            right,
        } => emit_binary(builder, module, functions, state, *operator, left, right),
        ExpressionKind::IntegerAddition { .. } => {
            emit_integer_addition(builder, module, functions, state, expression)
        }
        ExpressionKind::Call { .. }
        | ExpressionKind::Function(_)
        | ExpressionKind::IndirectCall { .. } => {
            emit_callable_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::If(expression) => {
            emit_if(builder, module, functions, state, expression, result_type)
        }
        ExpressionKind::Match(expression) => {
            emit_match(builder, module, functions, state, expression, result_type)
        }
        ExpressionKind::Loop(expression) => emit_loop(
            builder,
            module,
            functions,
            state,
            &expression.body,
            result_type,
        ),
        ExpressionKind::Block(block) => emit_block(builder, module, functions, state, block),
        ExpressionKind::Assign {
            target,
            operator,
            value,
        } => emit_assignment(builder, module, functions, state, target, *operator, value),
        ExpressionKind::Cast { value, target } => {
            emit_cast(builder, module, functions, state, value, *target)
        }
        ExpressionKind::Borrow { .. } | ExpressionKind::Dereference(_) => {
            emit_pointer_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::Assert { .. } => emit_assert(builder, module, functions, state, expression),
        ExpressionKind::Panic { message } => emit_panic(builder, module, functions, state, message),
        ExpressionKind::StringData(_)
        | ExpressionKind::StringLength(_)
        | ExpressionKind::SliceLength(_)
        | ExpressionKind::StringBytes(_)
        | ExpressionKind::StringChars(_)
        | ExpressionKind::CharsNext { .. }
        | ExpressionKind::StringFromParts { .. }
        | ExpressionKind::SliceFromParts { .. }
        | ExpressionKind::TypeStride { .. }
        | ExpressionKind::AllocateBytes { .. }
        | ExpressionKind::DeallocateBytes { .. }
        | ExpressionKind::ThreadSpawn { .. }
        | ExpressionKind::ThreadJoin { .. }
        | ExpressionKind::ThreadScope { .. }
        | ExpressionKind::SynchronizationCreate { .. }
        | ExpressionKind::SynchronizationLoad { .. }
        | ExpressionKind::SynchronizationReplace { .. }
        | ExpressionKind::ThreadLocalStore { .. }
        | ExpressionKind::ChannelCreate { .. }
        | ExpressionKind::ChannelSend { .. }
        | ExpressionKind::ChannelReceive { .. }
        | ExpressionKind::JobSubmit { .. }
        | ExpressionKind::JobWait { .. }
        | ExpressionKind::ParallelFor { .. } => {
            emit_runtime_intrinsic_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::Try { .. } => {
            emit_try_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::Field { .. }
        | ExpressionKind::Index { .. }
        | ExpressionKind::SliceGet { .. } => {
            emit_access_expression(builder, module, functions, state, expression)
        }
    }
}

fn emit_pointer_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::Borrow {
            place,
            slice_length,
            ..
        } => emit_borrow(
            builder,
            module,
            functions,
            state,
            place,
            *slice_length,
            expression.ty,
        ),
        ExpressionKind::Dereference(pointer) => {
            emit_dereference(builder, module, functions, state, pointer, expression.ty)
        }
        _ => Err("pointer expression lowering received an incompatible expression".to_owned()),
    }
}

fn emit_callable_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::Call {
            function,
            arguments,
        } => emit_call(
            builder,
            module,
            functions,
            state,
            *function,
            arguments,
            expression.ty,
        ),
        ExpressionKind::Function(function) => {
            emit_function_address(builder, module, functions, *function)
        }
        ExpressionKind::IndirectCall { callee, arguments } => emit_indirect_call(
            builder,
            module,
            functions,
            state,
            callee,
            arguments,
            expression.ty,
        ),
        _ => Err("non-callable expression reached callable code generation".to_owned()),
    }
}

fn emit_access_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::Field { base, field } => emit_field(
            builder,
            module,
            functions,
            state,
            base,
            *field,
            expression.ty,
        ),
        ExpressionKind::Index { base, index } => emit_index(
            builder,
            module,
            functions,
            state,
            base,
            index,
            expression.ty,
        ),
        ExpressionKind::SliceGet { .. } => {
            emit_slice_get(builder, module, functions, state, expression)
        }
        _ => Err("non-access expression reached access code generation".to_owned()),
    }
}

fn emit_runtime_intrinsic_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::StringData(value) => emit_string_view_part(
            builder,
            module,
            functions,
            state,
            value,
            StringViewPart::Data,
        ),
        ExpressionKind::StringLength(value) => emit_string_view_part(
            builder,
            module,
            functions,
            state,
            value,
            StringViewPart::Length,
        ),
        ExpressionKind::SliceLength(value) => {
            emit_slice_length(builder, module, functions, state, value)
        }
        ExpressionKind::StringBytes(value) => {
            emit_string_bytes(builder, module, functions, state, expression.ty, value)
        }
        ExpressionKind::StringChars(value) => {
            emit_string_chars(builder, module, functions, state, expression.ty, value)
        }
        ExpressionKind::CharsNext { iterator } => {
            emit_chars_next(builder, module, functions, state, expression.ty, iterator)
        }
        ExpressionKind::StringFromParts { data, length } => {
            emit_string_from_parts(builder, module, functions, state, data, length)
        }
        ExpressionKind::SliceFromParts { data, length } => emit_slice_from_parts(
            builder,
            module,
            functions,
            state,
            expression.ty,
            data,
            length,
        ),
        ExpressionKind::TypeStride { target } => emit_type_stride(builder, state.layouts, *target),
        ExpressionKind::AllocateBytes {
            allocator,
            length,
            allocation_type,
            error_type,
            error_variant,
        } => emit_allocate_bytes(
            builder,
            module,
            functions,
            state,
            &AllocateBytesParts {
                allocator,
                length,
                result_type: expression.ty,
                allocation_type: *allocation_type,
                error_type: *error_type,
                error_variant: *error_variant,
            },
        ),
        ExpressionKind::DeallocateBytes {
            allocator,
            data,
            length,
        } => emit_deallocate_bytes(builder, module, functions, state, allocator, data, length),
        ExpressionKind::ThreadSpawn { .. }
        | ExpressionKind::ThreadJoin { .. }
        | ExpressionKind::ThreadScope { .. } => {
            emit_thread_intrinsic_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::SynchronizationCreate { .. }
        | ExpressionKind::SynchronizationLoad { .. }
        | ExpressionKind::SynchronizationReplace { .. }
        | ExpressionKind::ThreadLocalStore { .. } => {
            emit_synchronization_intrinsic_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::ChannelCreate { .. }
        | ExpressionKind::ChannelSend { .. }
        | ExpressionKind::ChannelReceive { .. } => {
            emit_channel_intrinsic_expression(builder, module, functions, state, expression)
        }
        ExpressionKind::JobSubmit { .. }
        | ExpressionKind::JobWait { .. }
        | ExpressionKind::ParallelFor { .. } => {
            emit_job_intrinsic_expression(builder, module, functions, state, expression)
        }
        _ => Err("non-runtime intrinsic reached intrinsic code generation".to_owned()),
    }
}

fn emit_synchronization_intrinsic_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::SynchronizationCreate {
            value,
            synchronization,
        } => emit_synchronization_create(
            builder,
            module,
            functions,
            state,
            &SynchronizationCreateParts {
                value,
                synchronization: *synchronization,
            },
        ),
        ExpressionKind::SynchronizationLoad {
            handle,
            value_type,
            synchronization,
        } => emit_synchronization_load(
            builder,
            module,
            functions,
            state,
            &SynchronizationLoadParts {
                handle,
                value_type: *value_type,
                synchronization: *synchronization,
            },
        ),
        ExpressionKind::SynchronizationReplace {
            handle,
            value,
            synchronization,
        } => emit_synchronization_replace(
            builder,
            module,
            functions,
            state,
            &SynchronizationReplaceParts {
                handle,
                value,
                synchronization: *synchronization,
            },
        ),
        ExpressionKind::ThreadLocalStore { handle, value } => {
            emit_thread_local_store(builder, module, functions, state, handle, value)
        }
        _ => {
            Err("non-synchronization intrinsic reached synchronization code generation".to_owned())
        }
    }
}

fn emit_channel_intrinsic_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::ChannelCreate {
            probe,
            capacity,
            element_type,
        } => emit_channel_create(
            builder,
            module,
            functions,
            state,
            probe,
            capacity,
            *element_type,
        ),
        ExpressionKind::ChannelSend {
            handle,
            value,
            error_type,
            closed_variant,
        } => emit_channel_send(
            builder,
            module,
            functions,
            state,
            &ChannelSendParts {
                handle,
                value,
                result_type: expression.ty,
                error_type: *error_type,
                closed_variant: *closed_variant,
            },
        ),
        ExpressionKind::ChannelReceive {
            handle,
            value_type,
            error_type,
            closed_variant,
        } => emit_channel_receive(
            builder,
            module,
            functions,
            state,
            &ChannelReceiveParts {
                handle,
                result_type: expression.ty,
                value_type: *value_type,
                error_type: *error_type,
                closed_variant: *closed_variant,
            },
        ),
        _ => Err("non-channel intrinsic reached channel code generation".to_owned()),
    }
}

fn emit_job_intrinsic_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::JobSubmit {
            pool,
            callback,
            argument,
            output_type,
            error_type,
            submit_failed_variant,
        } => emit_job_submit(
            builder,
            module,
            functions,
            state,
            &JobSubmitParts {
                pool,
                callback: ThreadCallbackParts {
                    callback,
                    argument,
                    output_type: *output_type,
                },
                result_type: expression.ty,
                error_type: *error_type,
                submit_failed_variant: *submit_failed_variant,
            },
        ),
        ExpressionKind::JobWait {
            handle,
            output_type,
            error_type,
            invalid_handle_variant,
            worker_panicked_variant,
            result_mismatch_variant,
        } => emit_job_wait(
            builder,
            module,
            functions,
            state,
            &JobWaitParts {
                handle,
                result_type: expression.ty,
                output_type: *output_type,
                error_type: *error_type,
                failures: ThreadJoinFailures {
                    invalid_handle: *invalid_handle_variant,
                    worker_panicked: *worker_panicked_variant,
                    result_mismatch: *result_mismatch_variant,
                },
            },
        ),
        ExpressionKind::ParallelFor {
            pool,
            slice,
            chunk_type,
            array_length,
            callback,
            minimum_chunk,
            error_type,
            submit_failed_variant,
            worker_panicked_variant,
            result_mismatch_variant,
        } => emit_parallel_for(
            builder,
            module,
            functions,
            state,
            &ParallelForParts {
                pool,
                slice,
                chunk_type: *chunk_type,
                array_length: *array_length,
                callback,
                minimum_chunk,
                result_type: expression.ty,
                error_type: *error_type,
                failures: ParallelFailures {
                    submit_failed: *submit_failed_variant,
                    worker_panicked: *worker_panicked_variant,
                    result_mismatch: *result_mismatch_variant,
                },
            },
        ),
        _ => Err("non-job intrinsic reached job code generation".to_owned()),
    }
}

fn emit_thread_intrinsic_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::ThreadSpawn {
            callback,
            argument,
            output_type,
            error_type,
            spawn_failed_variant,
        } => emit_thread_spawn(
            builder,
            module,
            functions,
            state,
            &ThreadSpawnParts {
                callback: ThreadCallbackParts {
                    callback,
                    argument,
                    output_type: *output_type,
                },
                result_type: expression.ty,
                error_type: *error_type,
                spawn_failed_variant: *spawn_failed_variant,
            },
        ),
        ExpressionKind::ThreadJoin {
            handle,
            output_type,
            error_type,
            invalid_handle_variant,
            worker_panicked_variant,
            result_mismatch_variant,
        } => emit_thread_join(
            builder,
            module,
            functions,
            state,
            &ThreadJoinParts {
                handle,
                result_type: expression.ty,
                output_type: *output_type,
                error_type: *error_type,
                failures: ThreadJoinFailures {
                    invalid_handle: *invalid_handle_variant,
                    worker_panicked: *worker_panicked_variant,
                    result_mismatch: *result_mismatch_variant,
                },
            },
        ),
        ExpressionKind::ThreadScope {
            callback,
            argument,
            output_type,
            error_type,
            spawn_failed_variant,
            invalid_handle_variant,
            worker_panicked_variant,
            result_mismatch_variant,
        } => emit_thread_scope(
            builder,
            module,
            functions,
            state,
            &ThreadScopeParts {
                callback: ThreadCallbackParts {
                    callback,
                    argument,
                    output_type: *output_type,
                },
                result_type: expression.ty,
                error_type: *error_type,
                failures: ThreadScopeFailures {
                    spawn_failed: *spawn_failed_variant,
                    join: ThreadJoinFailures {
                        invalid_handle: *invalid_handle_variant,
                        worker_panicked: *worker_panicked_variant,
                        result_mismatch: *result_mismatch_variant,
                    },
                },
            },
        ),
        _ => Err("non-thread intrinsic reached thread code generation".to_owned()),
    }
}

fn emit_synchronization_create<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &SynchronizationCreateParts<'_>,
) -> Result<Emitted, String> {
    let value = emit_expression(builder, module, functions, state, parts.value)?;
    if value.terminated {
        return Ok(value);
    }
    let address = materialize_value_address(builder, state, parts.value.ty, value)?;
    let size = value_size(state.layouts, parts.value.ty)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function =
        runtime_synchronization_create_reference(builder, module, parts.synchronization)?;
    let call = builder.ins().call(function, &[address, size]);
    let handle = builder
        .inst_results(call)
        .first()
        .copied()
        .ok_or_else(|| "synchronization create call has no handle".to_owned())?;
    let failed = builder.ins().icmp_imm_u(IntCC::Equal, handle, 0);
    emit_runtime_failure_if(builder, module, failed, Failure::InvalidSynchronization)?;
    Ok(Emitted::value(handle))
}

fn emit_synchronization_load<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &SynchronizationLoadParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, parts.handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "synchronization handle")?;
    let destination = allocate_value_slot(builder, state.layouts, parts.value_type)?;
    let size = value_size(state.layouts, parts.value_type)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function = runtime_synchronization_load_reference(builder, module, parts.synchronization)?;
    let call = builder.ins().call(function, &[handle, destination, size]);
    let status = call_result(builder, call, "synchronization load status")?;
    emit_synchronization_status_check(builder, module, status)?;
    load_at_address(builder, parts.value_type, destination)
}

fn emit_synchronization_replace<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &SynchronizationReplaceParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, parts.handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "synchronization handle")?;
    let replacement = emit_expression(builder, module, functions, state, parts.value)?;
    if replacement.terminated {
        return Ok(replacement);
    }
    let source = materialize_value_address(builder, state, parts.value.ty, replacement)?;
    let destination = allocate_value_slot(builder, state.layouts, parts.value.ty)?;
    let size = value_size(state.layouts, parts.value.ty)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function =
        runtime_synchronization_replace_reference(builder, module, parts.synchronization)?;
    let call = builder
        .ins()
        .call(function, &[handle, source, destination, size]);
    let status = call_result(builder, call, "synchronization replace status")?;
    emit_synchronization_status_check(builder, module, status)?;
    load_at_address(builder, parts.value.ty, destination)
}

fn emit_thread_local_store<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    handle: &Expression,
    value: &Expression,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "thread-local handle")?;
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let source = materialize_value_address(builder, state, value.ty, emitted)?;
    let size = value_size(state.layouts, value.ty)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function = runtime_thread_local_store_reference(builder, module)?;
    let call = builder.ins().call(function, &[handle, source, size]);
    let status = call_result(builder, call, "thread-local store status")?;
    emit_synchronization_status_check(builder, module, status)?;
    Ok(Emitted::unit())
}

fn emit_channel_create<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    probe: &Expression,
    capacity: &Expression,
    element_type: Type,
) -> Result<Emitted, String> {
    let probe = emit_expression(builder, module, functions, state, probe)?;
    if probe.terminated {
        return Ok(probe);
    }
    let capacity = emit_expression(builder, module, functions, state, capacity)?;
    if capacity.terminated {
        return Ok(capacity);
    }
    let capacity = require_value(capacity, "channel capacity")?;
    let size = value_size(state.layouts, element_type)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function = runtime_channel_create_reference(builder, module)?;
    let call = builder.ins().call(function, &[capacity, size]);
    let handle = call_result(builder, call, "channel handle")?;
    let failed = builder.ins().icmp_imm_u(IntCC::Equal, handle, 0);
    emit_runtime_failure_if(builder, module, failed, Failure::InvalidSynchronization)?;
    Ok(Emitted::value(handle))
}

fn emit_channel_send<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ChannelSendParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, parts.handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "channel handle")?;
    let emitted = emit_expression(builder, module, functions, state, parts.value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let source = materialize_value_address(builder, state, parts.value.ty, emitted)?;
    let size = value_size(state.layouts, parts.value.ty)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function = runtime_channel_send_reference(builder, module)?;
    let call = builder.ins().call(function, &[handle, source, size]);
    let status = call_result(builder, call, "channel send status")?;
    emit_channel_result(
        builder,
        module,
        state,
        ChannelResultLowering {
            status,
            result_type: parts.result_type,
            value_type: Type::Unit,
            destination: None,
            error_type: parts.error_type,
            closed_variant: parts.closed_variant,
        },
    )
}

fn emit_channel_receive<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ChannelReceiveParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, parts.handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "channel handle")?;
    let destination = allocate_value_slot(builder, state.layouts, parts.value_type)?;
    let size = value_size(state.layouts, parts.value_type)?;
    let size = builder.ins().iconst(pointer_type(), i64::from(size));
    let function = runtime_channel_receive_reference(builder, module)?;
    let call = builder.ins().call(function, &[handle, destination, size]);
    let status = call_result(builder, call, "channel receive status")?;
    emit_channel_result(
        builder,
        module,
        state,
        ChannelResultLowering {
            status,
            result_type: parts.result_type,
            value_type: parts.value_type,
            destination: Some(destination),
            error_type: parts.error_type,
            closed_variant: parts.closed_variant,
        },
    )
}

fn emit_channel_result<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    state: &CodegenState<'_>,
    lowering: ChannelResultLowering,
) -> Result<Emitted, String> {
    let succeeded = builder.ins().icmp_imm_u(
        IntCC::Equal,
        lowering.status,
        i64::from(reimer_runtime::SYNC_OK),
    );
    let closed = builder.ins().icmp_imm_u(
        IntCC::Equal,
        lowering.status,
        i64::from(reimer_runtime::SYNC_CLOSED),
    );
    let recognized = builder.ins().bor(succeeded, closed);
    let invalid = builder.ins().icmp_imm_u(IntCC::Equal, recognized, 0);
    emit_runtime_failure_if(builder, module, invalid, Failure::InvalidSynchronization)?;

    let success_block = builder.create_block();
    let closed_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    builder
        .ins()
        .brif(succeeded, success_block, &[], closed_block, &[]);

    builder.switch_to_block(success_block);
    let value = match lowering.destination {
        Some(destination) => require_value(
            load_at_address(builder, lowering.value_type, destination)?,
            "channel result value",
        )?,
        None => builder.ins().iconst(types::I8, 0),
    };
    let success = build_enum_from_values(
        builder,
        state,
        lowering.result_type,
        0,
        &[(lowering.value_type, value)],
    )?;
    builder.ins().jump(merge, &[BlockArg::from(success)]);

    builder.switch_to_block(closed_block);
    let error = build_enum_from_values(
        builder,
        state,
        lowering.error_type,
        lowering.closed_variant,
        &[],
    )?;
    let failure = build_enum_from_values(
        builder,
        state,
        lowering.result_type,
        1,
        &[(lowering.error_type, error)],
    )?;
    builder.ins().jump(merge, &[BlockArg::from(failure)]);

    builder.switch_to_block(merge);
    let result = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "channel result merge block has no value".to_owned())?;
    Ok(Emitted::value(result))
}

fn emit_synchronization_status_check<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    status: Value,
) -> Result<(), String> {
    let failed =
        builder
            .ins()
            .icmp_imm_u(IntCC::NotEqual, status, i64::from(reimer_runtime::SYNC_OK));
    emit_runtime_failure_if(builder, module, failed, Failure::InvalidSynchronization)
}

fn call_result(builder: &FunctionBuilder<'_>, call: ir::Inst, role: &str) -> Result<Value, String> {
    builder
        .inst_results(call)
        .first()
        .copied()
        .ok_or_else(|| format!("runtime {role} call has no result"))
}

fn emit_type_stride(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    target: Type,
) -> Result<Emitted, String> {
    let size = layouts.value_layout(target)?.size;
    Ok(Emitted::value(
        builder.ins().iconst(pointer_type(), i64::from(size.max(1))),
    ))
}

fn type_index(id: TypeId) -> Result<usize, String> {
    usize::try_from(id.0).map_err(|_| format!("type id {} does not fit this host", id.0))
}

fn emit_string_from_parts<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    data: &Expression,
    length: &Expression,
) -> Result<Emitted, String> {
    let data = emit_expression(builder, module, functions, state, data)?;
    if data.terminated {
        return Ok(data);
    }
    let length = emit_expression(builder, module, functions, state, length)?;
    if length.terminated {
        return Ok(length);
    }
    let descriptor = allocate_composite(builder, state.layouts, Type::Str)?;
    store_dynamic_fat_view(
        builder,
        state.layouts,
        Type::Str,
        descriptor,
        require_value(data, "string data pointer")?,
        require_value(length, "string byte length")?,
    )?;
    Ok(Emitted::value(descriptor))
}

fn emit_slice_from_parts<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    slice_type: Type,
    data: &Expression,
    length: &Expression,
) -> Result<Emitted, String> {
    let data = emit_expression(builder, module, functions, state, data)?;
    if data.terminated {
        return Ok(data);
    }
    let length = emit_expression(builder, module, functions, state, length)?;
    if length.terminated {
        return Ok(length);
    }
    let descriptor = allocate_composite(builder, state.layouts, slice_type)?;
    store_dynamic_fat_view(
        builder,
        state.layouts,
        slice_type,
        descriptor,
        require_value(data, "slice data pointer")?,
        require_value(length, "slice element count")?,
    )?;
    Ok(Emitted::value(descriptor))
}

#[derive(Clone, Copy)]
enum StringViewPart {
    Data,
    Length,
}

fn emit_string_view_part<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    value: &Expression,
    part: StringViewPart,
) -> Result<Emitted, String> {
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let descriptor = require_value(emitted, "string-view intrinsic")?;
    let (data, length) = load_string_view(builder, state.layouts, descriptor)?;
    Ok(Emitted::value(match part {
        StringViewPart::Data => data,
        StringViewPart::Length => length,
    }))
}

fn emit_slice_length<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    value: &Expression,
) -> Result<Emitted, String> {
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let descriptor = require_value(emitted, "slice length")?;
    let layout = state.layouts.aggregate(value.ty)?;
    let AggregateLayoutKind::Slice { length_offset, .. } = layout.kind else {
        return Err("slice length requires a bounded-view layout".to_owned());
    };
    let length = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        descriptor,
        native_offset(length_offset, "slice length")?,
    );
    Ok(Emitted::value(length))
}

fn emit_string_bytes<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    result_type: Type,
    value: &Expression,
) -> Result<Emitted, String> {
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let source = require_value(emitted, "string byte view")?;
    let (data, length) = load_string_view(builder, state.layouts, source)?;
    let descriptor = allocate_composite(builder, state.layouts, result_type)?;
    store_dynamic_fat_view(
        builder,
        state.layouts,
        result_type,
        descriptor,
        data,
        length,
    )?;
    Ok(Emitted::value(descriptor))
}

fn emit_string_chars<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    result_type: Type,
    value: &Expression,
) -> Result<Emitted, String> {
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let source = require_value(emitted, "string character iterator")?;
    let offset = builder.ins().iconst(pointer_type(), 0);
    build_product_from_values(
        builder,
        state,
        result_type,
        &[(Type::Str, source), (Type::Usize, offset)],
    )
    .map(Emitted::value)
}

fn emit_chars_next<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    result_type: Type,
    iterator: &Place,
) -> Result<Emitted, String> {
    let address = emit_place_address(builder, module, functions, state, iterator)?;
    let (data, length, cursor_address) =
        chars_iterator_parts(builder, state.layouts, iterator.ty, address)?;
    let cursor = builder
        .ins()
        .load(pointer_type(), MemFlagsData::new(), cursor_address, 0);
    let (has_character, character, stored_cursor) =
        decode_next_character(builder, module, data, length, cursor)?;
    builder
        .ins()
        .store(MemFlagsData::new(), stored_cursor, cursor_address, 0);
    let some = build_enum_from_values(builder, state, result_type, 0, &[(Type::Char, character)])?;
    let none = build_enum_from_values(builder, state, result_type, 1, &[])?;
    Ok(Emitted::value(builder.ins().select(
        has_character,
        some,
        none,
    )))
}

fn chars_iterator_parts(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    iterator_type: Type,
    address: Value,
) -> Result<(Value, Value, Value), String> {
    let layout = layouts.aggregate(iterator_type)?;
    let AggregateLayoutKind::Product { offsets } = &layout.kind else {
        return Err("character iterator requires a product layout".to_owned());
    };
    let [source_offset, cursor_offset] = offsets.as_slice() else {
        return Err("character iterator layout must contain source and cursor fields".to_owned());
    };
    let source = address_at_offset(builder, address, *source_offset);
    let cursor_address = address_at_offset(builder, address, *cursor_offset);
    let (data, length) = load_string_view(builder, layouts, source)?;
    Ok((data, length, cursor_address))
}

fn decode_next_character<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    data: Value,
    length: Value,
    cursor: Value,
) -> Result<(Value, Value, Value), String> {
    let function = runtime_utf8_decode_next_reference(builder, module)?;
    let call = builder.ins().call(function, &[data, length, cursor]);
    let encoded = call_result(builder, call, "UTF-8 decode")?;
    let has_character = builder.ins().icmp_imm_u(IntCC::NotEqual, encoded, 0);
    let width = builder.ins().band_imm_u(encoded, 0b111);
    let character = builder.ins().ushr_imm_u(encoded, 3);
    let width = if pointer_type() == types::I32 {
        width
    } else {
        builder.ins().uextend(pointer_type(), width)
    };
    let advanced = builder.ins().iadd(cursor, width);
    let next_cursor = builder.ins().select(has_character, advanced, cursor);
    Ok((has_character, character, next_cursor))
}

fn emit_panic<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    message: &Expression,
) -> Result<Emitted, String> {
    let emitted = emit_expression(builder, module, functions, state, message)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let descriptor = require_value(emitted, "panic message")?;
    let (data, length) = load_string_view(builder, state.layouts, descriptor)?;
    let byte_offset = builder.ins().iconst(
        pointer_type(),
        i64::try_from(message.span.start)
            .map_err(|_| "panic source offset exceeds i64".to_owned())?,
    );
    let function = runtime_panic_reference(builder, module)?;
    builder.ins().call(function, &[data, length, byte_offset]);
    builder.ins().trap(TrapCode::unwrap_user(2));
    Ok(Emitted::terminated())
}

fn emit_assert_parts<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    mode: AssertionMode,
    condition: &Expression,
    message: &Expression,
) -> Result<Emitted, String> {
    if mode == AssertionMode::Debug && !state.debug_assertions {
        return Ok(Emitted::unit());
    }
    let condition = emit_expression(builder, module, functions, state, condition)?;
    if condition.terminated {
        return Ok(condition);
    }
    let condition = require_value(condition, "assertion condition")?;
    let succeeded = builder.create_block();
    let failed = builder.create_block();
    builder.ins().brif(condition, succeeded, &[], failed, &[]);

    builder.switch_to_block(failed);
    let panicked = emit_panic(builder, module, functions, state, message)?;
    if !panicked.terminated {
        return Err("failed assertion did not terminate execution".to_owned());
    }

    builder.switch_to_block(succeeded);
    Ok(Emitted::unit())
}

fn emit_assert<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    let ExpressionKind::Assert {
        mode,
        condition,
        message,
    } = &expression.kind
    else {
        return Err("assertion lowering received a different expression".to_owned());
    };
    emit_assert_parts(builder, module, functions, state, *mode, condition, message)
}

fn load_string_view(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    descriptor: Value,
) -> Result<(Value, Value), String> {
    let layout = layouts.aggregate(Type::Str)?;
    let AggregateLayoutKind::Slice {
        data_offset,
        length_offset,
    } = layout.kind
    else {
        return Err("string value has no bounded-view layout".to_owned());
    };
    let data = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        descriptor,
        i32::try_from(data_offset).map_err(|_| "string data offset exceeds i32".to_owned())?,
    );
    let length = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        descriptor,
        i32::try_from(length_offset).map_err(|_| "string length offset exceeds i32".to_owned())?,
    );
    Ok((data, length))
}

fn emit_allocate_bytes<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &AllocateBytesParts<'_>,
) -> Result<Emitted, String> {
    let allocator = emit_expression(builder, module, functions, state, parts.allocator)?;
    if allocator.terminated {
        return Ok(allocator);
    }
    let allocator = require_value(allocator, "allocator handle")?;
    let length = emit_expression(builder, module, functions, state, parts.length)?;
    if length.terminated {
        return Ok(length);
    }
    let length = require_value(length, "allocation length")?;
    let function = runtime_allocate_bytes_reference(builder, module)?;
    let call = builder.ins().call(function, &[allocator, length]);
    let data = builder
        .inst_results(call)
        .first()
        .copied()
        .ok_or_else(|| "runtime allocator call has no result".to_owned())?;

    let failed = builder.ins().icmp_imm_u(IntCC::Equal, data, 0);
    let error_block = builder.create_block();
    let success_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    builder
        .ins()
        .brif(failed, error_block, &[], success_block, &[]);

    builder.switch_to_block(error_block);
    let error = build_enum_from_values(builder, state, parts.error_type, parts.error_variant, &[])?;
    let result = build_enum_from_values(
        builder,
        state,
        parts.result_type,
        1,
        &[(parts.error_type, error)],
    )?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(success_block);
    let allocation = build_product_from_values(
        builder,
        state,
        parts.allocation_type,
        &[
            (Type::Usize, data),
            (Type::Usize, length),
            (Type::Usize, allocator),
        ],
    )?;
    let result = build_enum_from_values(
        builder,
        state,
        parts.result_type,
        0,
        &[(parts.allocation_type, allocation)],
    )?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(merge);
    let result = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "allocator merge block has no result".to_owned())?;
    Ok(Emitted::value(result))
}

fn emit_deallocate_bytes<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    allocator: &Expression,
    data: &Expression,
    length: &Expression,
) -> Result<Emitted, String> {
    let allocator = emit_expression(builder, module, functions, state, allocator)?;
    if allocator.terminated {
        return Ok(allocator);
    }
    let data = emit_expression(builder, module, functions, state, data)?;
    if data.terminated {
        return Ok(data);
    }
    let length = emit_expression(builder, module, functions, state, length)?;
    if length.terminated {
        return Ok(length);
    }
    let arguments = [
        require_value(allocator, "allocator handle")?,
        require_value(data, "allocation pointer")?,
        require_value(length, "allocation length")?,
    ];
    let function = runtime_deallocate_bytes_reference(builder, module)?;
    builder.ins().call(function, &arguments);
    Ok(Emitted::unit())
}

fn emit_job_submit<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &JobSubmitParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_job_start(builder, module, functions, state, parts)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "submitted job handle")?;
    let failed = builder.ins().icmp_imm_u(IntCC::Equal, handle, 0);
    let error_block = builder.create_block();
    let success_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    builder
        .ins()
        .brif(failed, error_block, &[], success_block, &[]);

    builder.switch_to_block(error_block);
    let error = build_thread_failure_result(
        builder,
        state,
        parts.result_type,
        parts.error_type,
        parts.submit_failed_variant,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(error)]);

    builder.switch_to_block(success_block);
    let success =
        build_thread_success_result(builder, state, parts.result_type, Type::Usize, handle)?;
    builder.ins().jump(merge, &[BlockArg::from(success)]);

    builder.switch_to_block(merge);
    let result = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "job submission merge block has no result".to_owned())?;
    Ok(Emitted::value(result))
}

fn emit_job_start<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &JobSubmitParts<'_>,
) -> Result<Emitted, String> {
    let pool = emit_expression(builder, module, functions, state, parts.pool)?;
    if pool.terminated {
        return Ok(pool);
    }
    let pool = require_value(pool, "job pool handle")?;
    let callback = emit_expression(builder, module, functions, state, parts.callback.callback)?;
    if callback.terminated {
        return Ok(callback);
    }
    let callback = require_value(callback, "job callback")?;
    let argument = emit_expression(builder, module, functions, state, parts.callback.argument)?;
    if argument.terminated {
        return Ok(argument);
    }
    let argument_address =
        materialize_value_address(builder, state, parts.callback.argument.ty, argument)?;
    let key = ThreadThunkKey {
        argument_type: parts.callback.argument.ty,
        result_type: parts.callback.output_type,
    };
    let thunk = state.thread_thunks.get(&key).copied().ok_or_else(|| {
        format!(
            "job callback thunk for `fn({}) -> {}` is missing",
            parts.callback.argument.ty, parts.callback.output_type
        )
    })?;
    let thunk = module.declare_func_in_func(thunk, builder.func);
    let thunk = builder.ins().func_addr(pointer_type(), thunk);
    let argument_size = value_size(state.layouts, parts.callback.argument.ty)?;
    let result_size = value_size(state.layouts, parts.callback.output_type)?;
    let argument_size = builder
        .ins()
        .iconst(pointer_type(), i64::from(argument_size));
    let result_size = builder.ins().iconst(pointer_type(), i64::from(result_size));
    let function = runtime_job_submit_reference(builder, module)?;
    let call = builder.ins().call(
        function,
        &[
            pool,
            thunk,
            callback,
            argument_address,
            argument_size,
            result_size,
        ],
    );
    let handle = call_result(builder, call, "job submission")?;
    Ok(Emitted::value(handle))
}

fn emit_job_wait<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &JobWaitParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, parts.handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "job wait handle")?;
    let result = emit_callback_wait_handle(
        builder,
        module,
        state,
        CallbackWaitKind::Job,
        ThreadJoinLowering {
            handle,
            result_type: parts.result_type,
            output_type: parts.output_type,
            error_type: parts.error_type,
            failures: parts.failures,
        },
    )?;
    Ok(Emitted::value(result))
}

fn emit_parallel_for<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ParallelForParts<'_>,
) -> Result<Emitted, String> {
    let pool = emit_expression(builder, module, functions, state, parts.pool)?;
    if pool.terminated {
        return Ok(pool);
    }
    let pool = require_value(pool, "parallel job pool handle")?;
    let slice = emit_expression(builder, module, functions, state, parts.slice)?;
    if slice.terminated {
        return Ok(slice);
    }
    let slice = require_value(slice, "parallel mutable slice")?;
    let callback = emit_expression(builder, module, functions, state, parts.callback)?;
    if callback.terminated {
        return Ok(callback);
    }
    let callback = require_value(callback, "parallel callback")?;
    let minimum_chunk = emit_expression(builder, module, functions, state, parts.minimum_chunk)?;
    if minimum_chunk.terminated {
        return Ok(minimum_chunk);
    }
    let minimum_chunk = require_value(minimum_chunk, "minimum parallel chunk")?;

    let layout = state.layouts.aggregate(parts.chunk_type)?;
    let AggregateLayoutKind::Slice {
        data_offset,
        length_offset,
    } = layout.kind
    else {
        return Err("parallel iteration requires a slice descriptor layout".to_owned());
    };
    let (data, length) = if let Some(array_length) = parts.array_length {
        (
            slice,
            builder
                .ins()
                .iconst(pointer_type(), array_length.cast_signed()),
        )
    } else {
        let data = builder.ins().load(
            pointer_type(),
            MemFlagsData::new(),
            slice,
            native_offset(data_offset, "parallel slice data")?,
        );
        let length = builder.ins().load(
            pointer_type(),
            MemFlagsData::new(),
            slice,
            native_offset(length_offset, "parallel slice length")?,
        );
        (data, length)
    };
    let stride = state
        .layouts
        .slice_stride(parts.chunk_type)
        .ok_or_else(|| "parallel slice stride is missing".to_owned())?;
    let stride = builder.ins().iconst(pointer_type(), i64::from(stride));
    let descriptor_size = builder
        .ins()
        .iconst(pointer_type(), i64::from(layout.value.size));
    let data_offset_value = builder.ins().iconst(pointer_type(), i64::from(data_offset));
    let length_offset_value = builder
        .ins()
        .iconst(pointer_type(), i64::from(length_offset));

    let key = ThreadThunkKey {
        argument_type: parts.chunk_type,
        result_type: Type::Unit,
    };
    let thunk = state
        .thread_thunks
        .get(&key)
        .copied()
        .ok_or_else(|| "parallel callback thunk is missing".to_owned())?;
    let thunk = module.declare_func_in_func(thunk, builder.func);
    let thunk = builder.ins().func_addr(pointer_type(), thunk);
    let request = allocate_parallel_request(builder)?;
    write_parallel_request(
        builder,
        request,
        ParallelRequestValues {
            pool,
            thunk,
            callback,
            data,
            length,
            element_size: stride,
            minimum_chunk,
            descriptor_size,
            data_offset: data_offset_value,
            length_offset: length_offset_value,
        },
    )?;
    let function = runtime_parallel_for_reference(builder, module)?;
    let call = builder.ins().call(function, &[request]);
    let status = call_result(builder, call, "parallel iteration")?;
    emit_parallel_result(builder, state, status, parts)
}

fn allocate_parallel_request(builder: &mut FunctionBuilder<'_>) -> Result<Value, String> {
    let size = u32::try_from(std::mem::size_of::<reimer_runtime::ParallelForRequest>())
        .map_err(|_| "parallel request size exceeds u32".to_owned())?;
    let align = std::mem::align_of::<reimer_runtime::ParallelForRequest>();
    let align_shift = u8::try_from(align.trailing_zeros())
        .map_err(|_| "parallel request alignment exponent exceeds u8".to_owned())?;
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        size,
        align_shift,
    ));
    Ok(builder.ins().stack_addr(pointer_type(), slot, 0))
}

fn write_parallel_request(
    builder: &mut FunctionBuilder<'_>,
    request: Value,
    values: ParallelRequestValues,
) -> Result<(), String> {
    let words = [
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, pool),
            values.pool,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, thunk),
            values.thunk,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, callback),
            values.callback,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, data),
            values.data,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, length),
            values.length,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, element_size),
            values.element_size,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, minimum_chunk),
            values.minimum_chunk,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, descriptor_size),
            values.descriptor_size,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, data_offset),
            values.data_offset,
        ),
        (
            std::mem::offset_of!(reimer_runtime::ParallelForRequest, length_offset),
            values.length_offset,
        ),
    ];
    for (offset, value) in words {
        store_parallel_request_word(builder, request, offset, value)?;
    }
    Ok(())
}

fn store_parallel_request_word(
    builder: &mut FunctionBuilder<'_>,
    request: Value,
    offset: usize,
    value: Value,
) -> Result<(), String> {
    let offset = i32::try_from(offset)
        .map_err(|_| "parallel request field offset exceeds i32".to_owned())?;
    builder
        .ins()
        .store(MemFlagsData::new(), value, request, offset);
    Ok(())
}

fn native_offset(offset: u32, role: &str) -> Result<i32, String> {
    i32::try_from(offset).map_err(|_| format!("{role} offset exceeds i32"))
}

fn emit_parallel_result(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    status: Value,
    parts: &ParallelForParts<'_>,
) -> Result<Emitted, String> {
    let success_block = builder.create_block();
    let worker_check = builder.create_block();
    let worker_block = builder.create_block();
    let mismatch_check = builder.create_block();
    let mismatch_block = builder.create_block();
    let submit_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    let succeeded =
        builder
            .ins()
            .icmp_imm_u(IntCC::Equal, status, i64::from(reimer_runtime::JOB_JOIN_OK));
    builder
        .ins()
        .brif(succeeded, success_block, &[], worker_check, &[]);

    builder.switch_to_block(worker_check);
    let worker_panicked = builder.ins().icmp_imm_u(
        IntCC::Equal,
        status,
        i64::from(reimer_runtime::JOB_JOIN_WORKER_PANICKED),
    );
    builder
        .ins()
        .brif(worker_panicked, worker_block, &[], mismatch_check, &[]);

    builder.switch_to_block(mismatch_check);
    let mismatched = builder.ins().icmp_imm_u(
        IntCC::Equal,
        status,
        i64::from(reimer_runtime::JOB_JOIN_RESULT_MISMATCH),
    );
    builder
        .ins()
        .brif(mismatched, mismatch_block, &[], submit_block, &[]);

    build_parallel_result_branches(
        builder,
        state,
        parts,
        merge,
        [
            (success_block, None),
            (worker_block, Some(parts.failures.worker_panicked)),
            (mismatch_block, Some(parts.failures.result_mismatch)),
            (submit_block, Some(parts.failures.submit_failed)),
        ],
    )?;
    builder.switch_to_block(merge);
    let result = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "parallel result merge block has no value".to_owned())?;
    Ok(Emitted::value(result))
}

fn build_parallel_result_branches(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    parts: &ParallelForParts<'_>,
    merge: ir::Block,
    branches: [(ir::Block, Option<u32>); 4],
) -> Result<(), String> {
    for (block, failure) in branches {
        builder.switch_to_block(block);
        let result = if let Some(failure) = failure {
            build_thread_failure_result(
                builder,
                state,
                parts.result_type,
                parts.error_type,
                failure,
            )?
        } else {
            let unit = builder.ins().iconst(types::I8, 0);
            build_thread_success_result(builder, state, parts.result_type, Type::Unit, unit)?
        };
        builder.ins().jump(merge, &[BlockArg::from(result)]);
    }
    Ok(())
}

fn emit_thread_spawn<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ThreadSpawnParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_thread_start(builder, module, functions, state, &parts.callback)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "spawned thread handle")?;
    let failed = builder.ins().icmp_imm_u(IntCC::Equal, handle, 0);
    let error_block = builder.create_block();
    let success_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    builder
        .ins()
        .brif(failed, error_block, &[], success_block, &[]);

    builder.switch_to_block(error_block);
    let error = build_thread_failure_result(
        builder,
        state,
        parts.result_type,
        parts.error_type,
        parts.spawn_failed_variant,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(error)]);

    builder.switch_to_block(success_block);
    let result =
        build_thread_success_result(builder, state, parts.result_type, Type::Usize, handle)?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(merge);
    let result = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "thread spawn merge block has no result".to_owned())?;
    Ok(Emitted::value(result))
}

fn emit_thread_scope<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ThreadScopeParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_thread_start(builder, module, functions, state, &parts.callback)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "scoped thread handle")?;
    let failed = builder.ins().icmp_imm_u(IntCC::Equal, handle, 0);
    let error_block = builder.create_block();
    let join_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    builder
        .ins()
        .brif(failed, error_block, &[], join_block, &[]);

    builder.switch_to_block(error_block);
    let error = build_thread_failure_result(
        builder,
        state,
        parts.result_type,
        parts.error_type,
        parts.failures.spawn_failed,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(error)]);

    builder.switch_to_block(join_block);
    let joined = emit_callback_wait_handle(
        builder,
        module,
        state,
        CallbackWaitKind::Thread,
        ThreadJoinLowering {
            handle,
            result_type: parts.result_type,
            output_type: parts.callback.output_type,
            error_type: parts.error_type,
            failures: parts.failures.join,
        },
    )?;
    builder.ins().jump(merge, &[BlockArg::from(joined)]);

    builder.switch_to_block(merge);
    let result = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "scoped thread merge block has no result".to_owned())?;
    Ok(Emitted::value(result))
}

fn emit_thread_join<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ThreadJoinParts<'_>,
) -> Result<Emitted, String> {
    let handle = emit_expression(builder, module, functions, state, parts.handle)?;
    if handle.terminated {
        return Ok(handle);
    }
    let handle = require_value(handle, "thread join handle")?;
    let result = emit_callback_wait_handle(
        builder,
        module,
        state,
        CallbackWaitKind::Thread,
        ThreadJoinLowering {
            handle,
            result_type: parts.result_type,
            output_type: parts.output_type,
            error_type: parts.error_type,
            failures: parts.failures,
        },
    )?;
    Ok(Emitted::value(result))
}

fn emit_thread_start<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    parts: &ThreadCallbackParts<'_>,
) -> Result<Emitted, String> {
    let callback = emit_expression(builder, module, functions, state, parts.callback)?;
    if callback.terminated {
        return Ok(callback);
    }
    let callback = require_value(callback, "thread callback")?;
    let argument = emit_expression(builder, module, functions, state, parts.argument)?;
    if argument.terminated {
        return Ok(argument);
    }
    let argument_address = materialize_value_address(builder, state, parts.argument.ty, argument)?;
    let key = ThreadThunkKey {
        argument_type: parts.argument.ty,
        result_type: parts.output_type,
    };
    let thunk = state.thread_thunks.get(&key).copied().ok_or_else(|| {
        format!(
            "thread callback thunk for `fn({}) -> {}` is missing",
            parts.argument.ty, parts.output_type
        )
    })?;
    let thunk = module.declare_func_in_func(thunk, builder.func);
    let thunk = builder.ins().func_addr(pointer_type(), thunk);
    let argument_size = value_size(state.layouts, parts.argument.ty)?;
    let result_size = value_size(state.layouts, parts.output_type)?;
    let argument_size = builder
        .ins()
        .iconst(pointer_type(), i64::from(argument_size));
    let result_size = builder.ins().iconst(pointer_type(), i64::from(result_size));
    let function = runtime_thread_spawn_reference(builder, module)?;
    let call = builder.ins().call(
        function,
        &[
            thunk,
            callback,
            argument_address,
            argument_size,
            result_size,
        ],
    );
    let handle = builder
        .inst_results(call)
        .first()
        .copied()
        .ok_or_else(|| "runtime thread spawn call has no handle".to_owned())?;
    Ok(Emitted::value(handle))
}

fn emit_callback_wait_handle<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    state: &CodegenState<'_>,
    kind: CallbackWaitKind,
    lowering: ThreadJoinLowering,
) -> Result<Value, String> {
    let output_address = allocate_value_slot(builder, state.layouts, lowering.output_type)?;
    let output_size = value_size(state.layouts, lowering.output_type)?;
    let output_size_value = builder.ins().iconst(pointer_type(), i64::from(output_size));
    let (function, succeeded_status, invalid_status, panicked_status) = match kind {
        CallbackWaitKind::Thread => (
            runtime_thread_join_reference(builder, module)?,
            reimer_runtime::THREAD_JOIN_OK,
            reimer_runtime::THREAD_JOIN_INVALID_HANDLE,
            reimer_runtime::THREAD_JOIN_WORKER_PANICKED,
        ),
        CallbackWaitKind::Job => (
            runtime_job_wait_reference(builder, module)?,
            reimer_runtime::JOB_JOIN_OK,
            reimer_runtime::JOB_JOIN_INVALID_HANDLE,
            reimer_runtime::JOB_JOIN_WORKER_PANICKED,
        ),
    };
    let call = builder.ins().call(
        function,
        &[lowering.handle, output_address, output_size_value],
    );
    let status = builder
        .inst_results(call)
        .first()
        .copied()
        .ok_or_else(|| "runtime thread join call has no status".to_owned())?;

    let success_block = builder.create_block();
    let dispatch_block = builder.create_block();
    let invalid_block = builder.create_block();
    let panic_check_block = builder.create_block();
    let panic_block = builder.create_block();
    let mismatch_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, pointer_type());
    let succeeded = builder
        .ins()
        .icmp_imm_u(IntCC::Equal, status, i64::from(succeeded_status));
    builder
        .ins()
        .brif(succeeded, success_block, &[], dispatch_block, &[]);

    builder.switch_to_block(dispatch_block);
    let invalid = builder
        .ins()
        .icmp_imm_u(IntCC::Equal, status, i64::from(invalid_status));
    builder
        .ins()
        .brif(invalid, invalid_block, &[], panic_check_block, &[]);

    builder.switch_to_block(panic_check_block);
    let panicked = builder
        .ins()
        .icmp_imm_u(IntCC::Equal, status, i64::from(panicked_status));
    builder
        .ins()
        .brif(panicked, panic_block, &[], mismatch_block, &[]);

    builder.switch_to_block(success_block);
    let output = load_at_address(builder, lowering.output_type, output_address)?;
    let output = output
        .value
        .unwrap_or_else(|| builder.ins().iconst(types::I8, 0));
    let result = build_thread_success_result(
        builder,
        state,
        lowering.result_type,
        lowering.output_type,
        output,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(invalid_block);
    let result = build_thread_failure_result(
        builder,
        state,
        lowering.result_type,
        lowering.error_type,
        lowering.failures.invalid_handle,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(panic_block);
    let result = build_thread_failure_result(
        builder,
        state,
        lowering.result_type,
        lowering.error_type,
        lowering.failures.worker_panicked,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(mismatch_block);
    let result = build_thread_failure_result(
        builder,
        state,
        lowering.result_type,
        lowering.error_type,
        lowering.failures.result_mismatch,
    )?;
    builder.ins().jump(merge, &[BlockArg::from(result)]);

    builder.switch_to_block(merge);
    builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "thread join merge block has no result".to_owned())
}

fn build_thread_success_result(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    result_type: Type,
    output_type: Type,
    output: Value,
) -> Result<Value, String> {
    build_enum_from_values(builder, state, result_type, 0, &[(output_type, output)])
}

fn build_thread_failure_result(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    result_type: Type,
    error_type: Type,
    error_variant: u32,
) -> Result<Value, String> {
    let error = build_enum_from_values(builder, state, error_type, error_variant, &[])?;
    build_enum_from_values(builder, state, result_type, 1, &[(error_type, error)])
}

fn materialize_value_address(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    ty: Type,
    emitted: Emitted,
) -> Result<Value, String> {
    if !ty.has_runtime_value() {
        return Ok(builder.ins().iconst(pointer_type(), 0));
    }
    let value = require_value(emitted, "thread argument")?;
    if ty.is_composite() {
        Ok(value)
    } else {
        let address = allocate_value_slot(builder, state.layouts, ty)?;
        builder.ins().store(MemFlagsData::new(), value, address, 0);
        Ok(address)
    }
}

fn allocate_value_slot(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    ty: Type,
) -> Result<Value, String> {
    let layout = layouts.value_layout(ty)?;
    let align_shift = u8::try_from(layout.align.trailing_zeros())
        .map_err(|_| "thread value alignment exponent does not fit in u8".to_owned())?;
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        layout.size.max(1),
        align_shift,
    ));
    Ok(builder.ins().stack_addr(pointer_type(), slot, 0))
}

fn value_size(layouts: &Layouts, ty: Type) -> Result<u32, String> {
    if ty.has_runtime_value() {
        Ok(layouts.value_layout(ty)?.size)
    } else {
        Ok(0)
    }
}

fn emit_try_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    let ExpressionKind::Try {
        value,
        success_variant,
        output_type,
        failure_variant,
        failure_type,
        return_type,
    } = &expression.kind
    else {
        return Err("non-try expression reached try code generation".to_owned());
    };
    let lowering = TryLowering {
        value,
        success_variant: *success_variant,
        output_type: *output_type,
        failure_variant: *failure_variant,
        failure_type: *failure_type,
        return_type: *return_type,
    };
    emit_try(builder, module, functions, state, lowering)
}

#[derive(Clone, Copy)]
struct TryLowering<'hir> {
    value: &'hir Expression,
    success_variant: u32,
    output_type: Type,
    failure_variant: u32,
    failure_type: Option<Type>,
    return_type: Type,
}

fn emit_try<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    lowering: TryLowering<'_>,
) -> Result<Emitted, String> {
    let TryLowering {
        value: expression,
        success_variant,
        output_type,
        failure_variant,
        failure_type,
        return_type,
    } = lowering;
    let emitted = emit_expression(builder, module, functions, state, expression)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let enum_value = require_value(emitted, "try operand")?;
    let discriminant = builder
        .ins()
        .load(types::I32, MemFlagsData::new(), enum_value, 0);
    let expected = builder.ins().iconst(types::I32, i64::from(success_variant));
    let succeeded = builder.ins().icmp(IntCC::Equal, discriminant, expected);
    let success = builder.create_block();
    let failure = builder.create_block();
    builder.ins().brif(succeeded, success, &[], failure, &[]);

    builder.switch_to_block(failure);
    let destination = state
        .return_destination
        .ok_or_else(|| "`?` requires an aggregate return destination".to_owned())?;
    if expression.ty == return_type {
        copy_composite(
            builder,
            state.layouts,
            state.target_config,
            expression.ty,
            destination,
            enum_value,
        )?;
    } else {
        let failure_discriminant = builder.ins().iconst(types::I32, i64::from(failure_variant));
        builder
            .ins()
            .store(MemFlagsData::new(), failure_discriminant, destination, 0);
        if let Some(failure_type) = failure_type {
            let source = enum_payload_address(
                builder,
                state,
                expression.ty,
                enum_value,
                failure_variant,
                0,
            )?;
            let target =
                enum_payload_address(builder, state, return_type, destination, failure_variant, 0)?;
            let emitted = load_at_address(builder, failure_type, source)?;
            store_at_offset(builder, state, target, 0, failure_type, emitted)?;
        }
    }
    let cleanup = emit_cleanup_range(builder, module, functions, state, 0)?;
    if !cleanup.terminated {
        builder.ins().return_(&[]);
    }

    builder.switch_to_block(success);
    let payload = enum_payload_address(
        builder,
        state,
        expression.ty,
        enum_value,
        success_variant,
        0,
    )?;
    load_at_address(builder, output_type, payload)
}

fn enum_payload_address(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    enum_type: Type,
    base: Value,
    variant: u32,
    field: usize,
) -> Result<Value, String> {
    let layout = state.layouts.aggregate(enum_type)?;
    let AggregateLayoutKind::Enum { variants } = &layout.kind else {
        return Err("`?` requires an enum layout".to_owned());
    };
    let offset = variants
        .get(type_index(TypeId(variant))?)
        .and_then(|offsets| offsets.get(field))
        .copied()
        .ok_or_else(|| "`?` success variant has no payload".to_owned())?;
    Ok(address_at_offset(builder, base, offset))
}

fn emit_enum_expression<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    let ExpressionKind::Enum { variant, fields } = &expression.kind else {
        return Err("non-enum expression reached enum code generation".to_owned());
    };
    emit_enum(
        builder,
        module,
        functions,
        state,
        expression.ty,
        *variant,
        fields,
    )
}

fn emit_dereference<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    pointer: &Expression,
    target: Type,
) -> Result<Emitted, String> {
    let emitted = emit_expression(builder, module, functions, state, pointer)?;
    if emitted.terminated {
        Ok(emitted)
    } else {
        load_at_address(
            builder,
            target,
            require_value(emitted, "dereference operand")?,
        )
    }
}

fn emit_literal(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    match &expression.kind {
        ExpressionKind::Integer(value) => Ok(Emitted::value(emit_integer_constant(
            builder,
            expression.ty,
            *value,
        )?)),
        ExpressionKind::Float32(bits) => Ok(Emitted::value(
            builder.ins().f32const(Ieee32::with_bits(*bits)),
        )),
        ExpressionKind::Float64(bits) => Ok(Emitted::value(
            builder.ins().f64const(Ieee64::with_bits(*bits)),
        )),
        ExpressionKind::Character(value) => Ok(Emitted::value(
            builder
                .ins()
                .iconst(types::I32, i64::from(u32::from(*value))),
        )),
        ExpressionKind::String(value) => emit_string_literal(builder, state, value),
        ExpressionKind::CString(value) => emit_c_string_literal(builder, value),
        ExpressionKind::Boolean(value) => Ok(Emitted::value(
            builder.ins().iconst(types::I8, i64::from(*value)),
        )),
        ExpressionKind::Unit => Ok(Emitted::unit()),
        _ => Err("non-literal expression reached literal code generation".to_owned()),
    }
}

fn emit_local(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    local: LocalId,
    ty: Type,
) -> Result<Emitted, String> {
    if ty == Type::Unit {
        return Ok(Emitted::unit());
    }
    let variable = state
        .locals
        .get(&local)
        .copied()
        .ok_or_else(|| format!("local {} has no native storage", local.0))?;
    let value = match variable {
        LocalStorage::Variable(variable) => builder.use_var(variable),
        LocalStorage::Address(address) => {
            builder
                .ins()
                .load(runtime_type(ty)?, MemFlagsData::new(), address, 0)
        }
    };
    Ok(Emitted::value(value))
}

fn emit_static<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &M,
    state: &CodegenState<'_>,
    value: StaticId,
    ty: Type,
) -> Result<Emitted, String> {
    let address = static_address(builder, module, state, value)?;
    load_at_address(builder, ty, address)
}

fn static_address<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &M,
    state: &CodegenState<'_>,
    value: StaticId,
) -> Result<Value, String> {
    let data = state
        .statics
        .get(&value)
        .copied()
        .ok_or_else(|| format!("static {} has no native data object", value.0))?;
    let global = module.declare_data_in_func(data, builder.func);
    Ok(builder.ins().symbol_value(pointer_type(), global))
}

fn emit_integer_constant(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    value: u128,
) -> Result<Value, String> {
    let runtime_type = runtime_type(ty)?;
    let bytes = value.to_le_bytes();
    let low_bytes = bytes[..8]
        .try_into()
        .map_err(|_| "failed to lower the low half of a u128 constant".to_owned())?;
    let low = u64::from_le_bytes(low_bytes);
    if runtime_type == types::I128 {
        let high_bytes = bytes[8..]
            .try_into()
            .map_err(|_| "failed to lower the high half of a u128 constant".to_owned())?;
        let high = u64::from_le_bytes(high_bytes);
        let low = builder.ins().iconst(types::I64, low.cast_signed());
        let high = builder.ins().iconst(types::I64, high.cast_signed());
        Ok(builder.ins().iconcat(low, high))
    } else {
        Ok(builder.ins().iconst(runtime_type, low.cast_signed()))
    }
}

fn emit_string_literal(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    value: &str,
) -> Result<Emitted, String> {
    let data = emit_literal_bytes(builder, value.as_bytes(), false)?;
    let descriptor = allocate_composite(builder, state.layouts, Type::Str)?;
    let length =
        u64::try_from(value.len()).map_err(|_| "string length does not fit u64".to_owned())?;
    store_fat_view(builder, state.layouts, Type::Str, descriptor, data, length)?;
    Ok(Emitted::value(descriptor))
}

fn emit_c_string_literal(
    builder: &mut FunctionBuilder<'_>,
    value: &str,
) -> Result<Emitted, String> {
    emit_literal_bytes(builder, value.as_bytes(), true).map(Emitted::value)
}

fn emit_literal_bytes(
    builder: &mut FunctionBuilder<'_>,
    bytes: &[u8],
    append_nul: bool,
) -> Result<Value, String> {
    let stored_length = bytes
        .len()
        .checked_add(usize::from(append_nul))
        .ok_or_else(|| "literal byte length overflowed".to_owned())?;
    let size = u32::try_from(stored_length)
        .map_err(|_| "literal exceeds the native stack-slot limit".to_owned())?;
    let bytes_slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        size.max(1),
        0,
    ));
    let data = builder.ins().stack_addr(pointer_type(), bytes_slot, 0);
    for (offset, byte) in bytes
        .iter()
        .copied()
        .chain(append_nul.then_some(0))
        .enumerate()
    {
        let offset = i32::try_from(offset)
            .map_err(|_| "literal offset exceeds native addressing".to_owned())?;
        let byte = builder.ins().iconst(types::I8, i64::from(byte));
        builder.ins().store(MemFlagsData::new(), byte, data, offset);
    }
    Ok(data)
}

fn emit_borrow<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    place: &Place,
    slice_length: Option<u64>,
    result_type: Type,
) -> Result<Emitted, String> {
    let address = emit_place_address(builder, module, functions, state, place)?;
    let Some(length) = slice_length else {
        return Ok(Emitted::value(address));
    };
    let descriptor = allocate_composite(builder, state.layouts, result_type)?;
    store_fat_view(
        builder,
        state.layouts,
        result_type,
        descriptor,
        address,
        length,
    )?;
    Ok(Emitted::value(descriptor))
}

fn store_fat_view(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    ty: Type,
    descriptor: Value,
    data: Value,
    length: u64,
) -> Result<(), String> {
    let layout = layouts.aggregate(ty)?;
    let AggregateLayoutKind::Slice {
        data_offset,
        length_offset,
    } = &layout.kind
    else {
        return Err("fat view requires a slice layout".to_owned());
    };
    let length = builder.ins().iconst(pointer_type(), length.cast_signed());
    builder.ins().store(
        MemFlagsData::new(),
        data,
        descriptor,
        i32::try_from(*data_offset).map_err(|_| "view data offset exceeds i32".to_owned())?,
    );
    builder.ins().store(
        MemFlagsData::new(),
        length,
        descriptor,
        i32::try_from(*length_offset).map_err(|_| "view length offset exceeds i32".to_owned())?,
    );
    Ok(())
}

fn store_dynamic_fat_view(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    ty: Type,
    descriptor: Value,
    data: Value,
    length: Value,
) -> Result<(), String> {
    let layout = layouts.aggregate(ty)?;
    let AggregateLayoutKind::Slice {
        data_offset,
        length_offset,
    } = &layout.kind
    else {
        return Err("fat view requires a slice layout".to_owned());
    };
    builder.ins().store(
        MemFlagsData::new(),
        data,
        descriptor,
        i32::try_from(*data_offset).map_err(|_| "view data offset exceeds i32".to_owned())?,
    );
    builder.ins().store(
        MemFlagsData::new(),
        length,
        descriptor,
        i32::try_from(*length_offset).map_err(|_| "view length offset exceeds i32".to_owned())?,
    );
    Ok(())
}

fn emit_product<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    ty: Type,
    elements: &[Expression],
) -> Result<Emitted, String> {
    let layout = state.layouts.aggregate(ty)?;
    let offsets = match &layout.kind {
        AggregateLayoutKind::Product { offsets } => offsets.clone(),
        AggregateLayoutKind::Array { stride, length } => {
            if usize::try_from(*length).ok() != Some(elements.len()) {
                return Err("typed array element count does not match its layout".to_owned());
            }
            (0..elements.len())
                .map(|index| {
                    u32::try_from(index)
                        .ok()
                        .and_then(|index| index.checked_mul(*stride))
                        .ok_or_else(|| "array element offset overflowed".to_owned())
                })
                .collect::<Result<_, _>>()?
        }
        AggregateLayoutKind::Scalar
        | AggregateLayoutKind::Enum { .. }
        | AggregateLayoutKind::Slice { .. } => {
            return Err("enum layout reached product construction".to_owned());
        }
    };
    let destination = allocate_composite(builder, state.layouts, ty)?;
    for (element, offset) in elements.iter().zip(offsets) {
        let emitted = emit_expression(builder, module, functions, state, element)?;
        if emitted.terminated {
            return Ok(emitted);
        }
        store_at_offset(builder, state, destination, offset, element.ty, emitted)?;
    }
    Ok(Emitted::value(destination))
}

fn emit_enum<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    ty: Type,
    variant: u32,
    fields: &[Expression],
) -> Result<Emitted, String> {
    let layout = state.layouts.aggregate(ty)?;
    let AggregateLayoutKind::Enum { variants } = &layout.kind else {
        return Err("non-enum layout reached enum construction".to_owned());
    };
    let offsets = variants
        .get(type_index(TypeId(variant))?)
        .cloned()
        .ok_or_else(|| format!("enum variant {variant} has no native layout"))?;
    if offsets.len() != fields.len() {
        return Err("typed enum payload does not match its layout".to_owned());
    }
    let destination = allocate_composite(builder, state.layouts, ty)?;
    let discriminant = builder.ins().iconst(types::I32, i64::from(variant));
    builder
        .ins()
        .store(MemFlagsData::new(), discriminant, destination, 0);
    for (field, offset) in fields.iter().zip(offsets) {
        let emitted = emit_expression(builder, module, functions, state, field)?;
        if emitted.terminated {
            return Ok(emitted);
        }
        store_at_offset(builder, state, destination, offset, field.ty, emitted)?;
    }
    Ok(Emitted::value(destination))
}

fn build_product_from_values(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    ty: Type,
    fields: &[(Type, Value)],
) -> Result<Value, String> {
    let layout = state.layouts.aggregate(ty)?;
    let AggregateLayoutKind::Product { offsets } = &layout.kind else {
        return Err("runtime product construction requires a product layout".to_owned());
    };
    if offsets.len() != fields.len() {
        return Err("runtime product fields do not match their layout".to_owned());
    }
    let destination = allocate_composite(builder, state.layouts, ty)?;
    for ((field_type, value), offset) in fields.iter().zip(offsets) {
        store_at_offset(
            builder,
            state,
            destination,
            *offset,
            *field_type,
            Emitted::value(*value),
        )?;
    }
    Ok(destination)
}

fn build_enum_from_values(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    ty: Type,
    variant: u32,
    fields: &[(Type, Value)],
) -> Result<Value, String> {
    let layout = state.layouts.aggregate(ty)?;
    let AggregateLayoutKind::Enum { variants } = &layout.kind else {
        return Err("runtime enum construction requires an enum layout".to_owned());
    };
    let offsets = variants
        .get(type_index(TypeId(variant))?)
        .ok_or_else(|| format!("runtime enum variant {variant} has no layout"))?;
    if offsets.len() != fields.len() {
        return Err("runtime enum fields do not match their layout".to_owned());
    }
    let destination = allocate_composite(builder, state.layouts, ty)?;
    let discriminant = builder.ins().iconst(types::I32, i64::from(variant));
    builder
        .ins()
        .store(MemFlagsData::new(), discriminant, destination, 0);
    for ((field_type, value), offset) in fields.iter().zip(offsets) {
        store_at_offset(
            builder,
            state,
            destination,
            *offset,
            *field_type,
            Emitted::value(*value),
        )?;
    }
    Ok(destination)
}

fn emit_field<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    base: &Expression,
    field: u32,
    field_type: Type,
) -> Result<Emitted, String> {
    let layout = state.layouts.aggregate(base.ty)?;
    let AggregateLayoutKind::Product { offsets } = &layout.kind else {
        return Err("field access requires a product layout".to_owned());
    };
    let offset = offsets
        .get(type_index(TypeId(field))?)
        .copied()
        .ok_or_else(|| format!("field {field} has no native offset"))?;
    let base = emit_expression(builder, module, functions, state, base)?;
    if base.terminated {
        return Ok(base);
    }
    let address = address_at_offset(builder, require_value(base, "field base")?, offset);
    load_at_address(builder, field_type, address)
}

fn emit_index<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    base: &Expression,
    index: &Expression,
    element_type: Type,
) -> Result<Emitted, String> {
    let address = emit_array_element_address(builder, module, functions, state, base, index)?;
    load_at_address(builder, element_type, address)
}

fn emit_slice_get<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    let ExpressionKind::SliceGet {
        slice,
        index,
        reference_type,
        ..
    } = &expression.kind
    else {
        return Err("slice access lowering received an incompatible expression".to_owned());
    };
    let emitted_slice = emit_expression(builder, module, functions, state, slice)?;
    if emitted_slice.terminated {
        return Ok(emitted_slice);
    }
    let emitted_index = emit_expression(builder, module, functions, state, index)?;
    if emitted_index.terminated {
        return Ok(emitted_index);
    }
    let slice_value = require_value(emitted_slice, "recoverable slice access")?;
    let index_value = require_value(emitted_index, "recoverable slice index")?;
    let (data, length, stride) =
        indexed_sequence_parts(builder, state.layouts, slice.ty, slice_value)?;
    let in_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, index_value, length);
    let byte_offset = builder.ins().imul_imm_u(index_value, i64::from(stride));
    let address = builder.ins().iadd(data, byte_offset);
    let some = build_enum_from_values(
        builder,
        state,
        expression.ty,
        0,
        &[(*reference_type, address)],
    )?;
    let none = build_enum_from_values(builder, state, expression.ty, 1, &[])?;
    Ok(Emitted::value(builder.ins().select(in_bounds, some, none)))
}

fn emit_array_element_address<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    base: &Expression,
    index: &Expression,
) -> Result<Value, String> {
    let base_type = base.ty;
    let base = emit_expression(builder, module, functions, state, base)?;
    if base.terminated {
        return Err("terminating array base reached address generation".to_owned());
    }
    let index = emit_expression(builder, module, functions, state, index)?;
    if index.terminated {
        return Err("terminating array index reached address generation".to_owned());
    }
    let base = require_value(base, "indexed sequence")?;
    let index = require_value(index, "array index")?;
    let (base, length, stride) = indexed_sequence_parts(builder, state.layouts, base_type, base)?;
    let out_of_bounds = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
    emit_runtime_failure_if(builder, module, out_of_bounds, Failure::Bounds)?;
    let byte_offset = builder.ins().imul_imm_u(index, i64::from(stride));
    Ok(builder.ins().iadd(base, byte_offset))
}

fn indexed_sequence_parts(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    sequence_type: Type,
    value: Value,
) -> Result<(Value, Value, u32), String> {
    let array_type = if matches!(sequence_type, Type::Array(_)) {
        Some(sequence_type)
    } else {
        layouts
            .pointer_target(sequence_type)
            .filter(|target| matches!(target, Type::Array(_)))
    };
    if let Some(array_type) = array_type {
        let layout = layouts.aggregate(array_type)?;
        let AggregateLayoutKind::Array { stride, length } = &layout.kind else {
            return Err("indexed array type has no array layout".to_owned());
        };
        let length = builder.ins().iconst(pointer_type(), length.cast_signed());
        return Ok((value, length, *stride));
    }
    let Some(stride) = layouts.slice_stride(sequence_type) else {
        return Err("index access requires an array or slice layout".to_owned());
    };
    let layout = layouts.aggregate(sequence_type)?;
    let AggregateLayoutKind::Slice {
        data_offset,
        length_offset,
    } = &layout.kind
    else {
        return Err("indexed slice type has no fat-view layout".to_owned());
    };
    let data = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        value,
        i32::try_from(*data_offset).map_err(|_| "slice data offset exceeds i32".to_owned())?,
    );
    let length = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        value,
        i32::try_from(*length_offset).map_err(|_| "slice length offset exceeds i32".to_owned())?,
    );
    Ok((data, length, stride))
}

fn allocate_composite(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    ty: Type,
) -> Result<Value, String> {
    let layout = layouts.aggregate(ty)?.value;
    let size = layout.size.max(1);
    let align_shift = u8::try_from(layout.align.trailing_zeros())
        .map_err(|_| "aggregate alignment exponent does not fit in u8".to_owned())?;
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        size,
        align_shift,
    ));
    Ok(builder.ins().stack_addr(pointer_type(), slot, 0))
}

fn define_local(
    builder: &mut FunctionBuilder<'_>,
    state: &mut CodegenState<'_>,
    local: LocalId,
    ty: Type,
    value: Value,
) -> Result<(), String> {
    let storage = if ty.is_composite() {
        let destination = allocate_composite(builder, state.layouts, ty)?;
        copy_composite(
            builder,
            state.layouts,
            state.target_config,
            ty,
            destination,
            value,
        )?;
        let variable = builder.declare_var(runtime_type(ty)?);
        builder.def_var(variable, destination);
        LocalStorage::Variable(variable)
    } else {
        let runtime = runtime_type(ty)?;
        let size = runtime.bytes();
        let align_shift = u8::try_from(size.trailing_zeros())
            .map_err(|_| "local alignment exponent does not fit in u8".to_owned())?;
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            align_shift,
        ));
        let address = builder.ins().stack_addr(pointer_type(), slot, 0);
        builder.ins().store(MemFlagsData::new(), value, address, 0);
        LocalStorage::Address(address)
    };
    state.locals.insert(local, storage);
    Ok(())
}

fn store_at_offset(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    base: Value,
    offset: u32,
    ty: Type,
    emitted: Emitted,
) -> Result<(), String> {
    if !ty.has_runtime_value() {
        return Ok(());
    }
    let value = require_value(emitted, "aggregate field")?;
    let destination = address_at_offset(builder, base, offset);
    if ty.is_composite() {
        copy_composite(
            builder,
            state.layouts,
            state.target_config,
            ty,
            destination,
            value,
        )
    } else {
        builder
            .ins()
            .store(MemFlagsData::new(), value, destination, 0);
        Ok(())
    }
}

fn load_at_address(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    address: Value,
) -> Result<Emitted, String> {
    if !ty.has_runtime_value() {
        return Ok(Emitted::unit());
    }
    if ty.is_composite() {
        Ok(Emitted::value(address))
    } else {
        Ok(Emitted::value(builder.ins().load(
            runtime_type(ty)?,
            MemFlagsData::new(),
            address,
            0,
        )))
    }
}

fn copy_composite(
    builder: &mut FunctionBuilder<'_>,
    layouts: &Layouts,
    target_config: TargetFrontendConfig,
    ty: Type,
    destination: Value,
    source: Value,
) -> Result<(), String> {
    let layout = layouts.aggregate(ty)?.value;
    let align = u8::try_from(layout.align)
        .map_err(|_| "aggregate alignment does not fit in u8".to_owned())?;
    builder.emit_small_memory_copy(
        target_config,
        destination,
        source,
        u64::from(layout.size),
        align,
        align,
        false,
        MemFlagsData::new(),
    );
    Ok(())
}

fn address_at_offset(builder: &mut FunctionBuilder<'_>, base: Value, offset: u32) -> Value {
    if offset == 0 {
        base
    } else {
        builder.ins().iadd_imm_u(base, i64::from(offset))
    }
}

fn emit_unary<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    operator: UnaryOperator,
    operand: &Expression,
) -> Result<Emitted, String> {
    let operand_type = operand.ty;
    let emitted = emit_expression(builder, module, functions, state, operand)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let operand = require_value(emitted, "unary operand")?;
    let value = match operator {
        UnaryOperator::Negate if operand_type.is_float() => builder.ins().fneg(operand),
        UnaryOperator::Negate => {
            let zero = emit_integer_constant(builder, operand_type, 0)?;
            let (value, overflow) = builder.ins().ssub_overflow(zero, operand);
            emit_runtime_failure_if(builder, module, overflow, Failure::Overflow)?;
            value
        }
        UnaryOperator::Not if operand_type == Type::Bool => {
            builder.ins().icmp_imm_s(IntCC::Equal, operand, 0)
        }
        UnaryOperator::Not => builder.ins().bnot(operand),
    };
    Ok(Emitted::value(value))
}

fn emit_binary<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    operator: BinaryOperator,
    left: &Expression,
    right: &Expression,
) -> Result<Emitted, String> {
    if matches!(operator, BinaryOperator::And | BinaryOperator::Or) {
        return emit_short_circuit(builder, module, functions, state, operator, left, right);
    }

    let operand_type = left.ty;
    let emitted_left = emit_expression(builder, module, functions, state, left)?;
    if emitted_left.terminated {
        return Ok(emitted_left);
    }
    let emitted_right = emit_expression(builder, module, functions, state, right)?;
    if emitted_right.terminated {
        return Ok(emitted_right);
    }
    let left = require_value(emitted_left, "left binary operand")?;
    let right = require_value(emitted_right, "right binary operand")?;
    if operand_type.is_composite()
        && matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual)
    {
        let equal = emit_value_equality(builder, module, state, operand_type, left, right)?;
        let value = if operator == BinaryOperator::Equal {
            equal
        } else {
            builder.ins().icmp_imm_s(IntCC::Equal, equal, 0)
        };
        return Ok(Emitted::value(value));
    }
    let value = if operand_type.is_float() {
        match operator {
            BinaryOperator::Add => builder.ins().fadd(left, right),
            BinaryOperator::Subtract => builder.ins().fsub(left, right),
            BinaryOperator::Multiply => builder.ins().fmul(left, right),
            BinaryOperator::Divide => builder.ins().fdiv(left, right),
            BinaryOperator::Equal => builder.ins().fcmp(FloatCC::Equal, left, right),
            BinaryOperator::NotEqual => builder.ins().fcmp(FloatCC::NotEqual, left, right),
            BinaryOperator::Less => builder.ins().fcmp(FloatCC::LessThan, left, right),
            BinaryOperator::LessEqual => builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right),
            BinaryOperator::Greater => builder.ins().fcmp(FloatCC::GreaterThan, left, right),
            BinaryOperator::GreaterEqual => {
                builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right)
            }
            _ => return Err("invalid floating-point operator reached code generation".to_owned()),
        }
    } else {
        match operator {
            BinaryOperator::Add | BinaryOperator::Subtract | BinaryOperator::Multiply => {
                emit_checked_arithmetic(builder, module, operator, operand_type, left, right)?
            }
            BinaryOperator::Divide | BinaryOperator::Remainder => {
                emit_checked_division(builder, module, operator, operand_type, left, right)?
            }
            BinaryOperator::Equal => builder.ins().icmp(IntCC::Equal, left, right),
            BinaryOperator::NotEqual => builder.ins().icmp(IntCC::NotEqual, left, right),
            BinaryOperator::Less => builder.ins().icmp(
                integer_comparison(operand_type, IntRelation::Less),
                left,
                right,
            ),
            BinaryOperator::LessEqual => builder.ins().icmp(
                integer_comparison(operand_type, IntRelation::LessEqual),
                left,
                right,
            ),
            BinaryOperator::Greater => builder.ins().icmp(
                integer_comparison(operand_type, IntRelation::Greater),
                left,
                right,
            ),
            BinaryOperator::GreaterEqual => builder.ins().icmp(
                integer_comparison(operand_type, IntRelation::GreaterEqual),
                left,
                right,
            ),
            BinaryOperator::BitAnd => builder.ins().band(left, right),
            BinaryOperator::BitXor => builder.ins().bxor(left, right),
            BinaryOperator::BitOr => builder.ins().bor(left, right),
            BinaryOperator::ShiftLeft | BinaryOperator::ShiftRight => {
                emit_checked_shift(builder, module, operator, operand_type, left, right)?
            }
            BinaryOperator::And | BinaryOperator::Or => {
                return Err("logical operator bypassed short-circuit lowering".to_owned());
            }
        }
    };
    Ok(Emitted::value(value))
}

fn emit_integer_addition<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &Expression,
) -> Result<Emitted, String> {
    let ExpressionKind::IntegerAddition { mode, left, right } = &expression.kind else {
        return Err("integer addition lowering received an incompatible expression".to_owned());
    };
    let emitted_left = emit_expression(builder, module, functions, state, left)?;
    if emitted_left.terminated {
        return Ok(emitted_left);
    }
    let emitted_right = emit_expression(builder, module, functions, state, right)?;
    if emitted_right.terminated {
        return Ok(emitted_right);
    }
    let left_value = require_value(emitted_left, "left integer addition operand")?;
    let right_value = require_value(emitted_right, "right integer addition operand")?;
    let value = match mode {
        IntegerAdditionMode::Wrapping => builder.ins().iadd(left_value, right_value),
        IntegerAdditionMode::Checked => {
            let (sum, overflow) = emit_overflowing_add(builder, left.ty, left_value, right_value);
            let some = build_enum_from_values(builder, state, expression.ty, 0, &[(left.ty, sum)])?;
            let none = build_enum_from_values(builder, state, expression.ty, 1, &[])?;
            builder.ins().select(overflow, none, some)
        }
        IntegerAdditionMode::Saturating => {
            emit_saturating_add(builder, left.ty, left_value, right_value)?
        }
    };
    Ok(Emitted::value(value))
}

fn emit_overflowing_add(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    left: Value,
    right: Value,
) -> (Value, Value) {
    if ty.is_signed_integer() {
        builder.ins().sadd_overflow(left, right)
    } else {
        builder.ins().uadd_overflow(left, right)
    }
}

fn emit_saturating_add(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    let (sum, overflow) = emit_overflowing_add(builder, ty, left, right);
    let bits = ty
        .integer_bits()
        .ok_or_else(|| format!("`{ty}` has no integer width"))?;
    if !ty.is_signed_integer() {
        let maximum = emit_integer_constant(builder, ty, integer_bit_mask(bits))?;
        return Ok(builder.ins().select(overflow, maximum, sum));
    }

    let sign_bit = 1_u128 << (bits - 1);
    let minimum = emit_integer_constant(builder, ty, sign_bit)?;
    let maximum = emit_integer_constant(builder, ty, sign_bit - 1)?;
    let left_is_negative = builder.ins().icmp_imm_s(IntCC::SignedLessThan, left, 0);
    let bound = builder.ins().select(left_is_negative, minimum, maximum);
    Ok(builder.ins().select(overflow, bound, sum))
}

fn emit_value_equality<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    state: &CodegenState<'_>,
    ty: Type,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    if ty == Type::Unit {
        return Ok(builder.ins().iconst(types::I8, 1));
    }
    if ty == Type::Str {
        return emit_string_equality(builder, module, state.layouts, left, right);
    }
    if !ty.is_composite() {
        return Ok(if ty.is_float() {
            builder.ins().fcmp(FloatCC::Equal, left, right)
        } else {
            builder.ins().icmp(IntCC::Equal, left, right)
        });
    }
    if let Some(fields) = state.layouts.product_fields(ty) {
        let fields = fields.to_vec();
        let layout = state.layouts.aggregate(ty)?;
        let AggregateLayoutKind::Product { offsets } = &layout.kind else {
            return Err("product metadata does not match its native layout".to_owned());
        };
        return emit_product_equality(builder, module, state, &fields, offsets, left, right);
    }
    if let Some((element, length, stride)) = state.layouts.array_shape(ty) {
        return emit_array_equality(builder, module, state, element, length, stride, left, right);
    }
    if let Some(variants) = state.layouts.enum_variants(ty) {
        let variants = variants.to_vec();
        let layout = state.layouts.aggregate(ty)?;
        let AggregateLayoutKind::Enum { variants: offsets } = &layout.kind else {
            return Err("enum metadata does not match its native layout".to_owned());
        };
        return emit_enum_equality(builder, module, state, &variants, offsets, left, right);
    }
    Err(format!("type `{ty}` has no structural equality layout"))
}

fn emit_product_equality<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    state: &CodegenState<'_>,
    fields: &[Type],
    offsets: &[u32],
    left: Value,
    right: Value,
) -> Result<Value, String> {
    if fields.len() != offsets.len() {
        return Err("product equality metadata has inconsistent field counts".to_owned());
    }
    let mut equal = builder.ins().iconst(types::I8, 1);
    for (field, offset) in fields.iter().zip(offsets) {
        let left_address = address_at_offset(builder, left, *offset);
        let right_address = address_at_offset(builder, right, *offset);
        let left_field = load_at_address(builder, *field, left_address)?;
        let right_field = load_at_address(builder, *field, right_address)?;
        if field.has_runtime_value() {
            let field_equal = emit_value_equality(
                builder,
                module,
                state,
                *field,
                require_value(left_field, "left equality field")?,
                require_value(right_field, "right equality field")?,
            )?;
            equal = builder.ins().band(equal, field_equal);
        }
    }
    Ok(equal)
}

#[allow(
    clippy::too_many_arguments,
    reason = "array equality lowering needs the complete checked layout tuple"
)]
fn emit_array_equality<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    state: &CodegenState<'_>,
    element: Type,
    length: u64,
    stride: u32,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    let header = builder.create_block();
    let body = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(header, pointer_type());
    builder.append_block_param(header, types::I8);
    builder.append_block_param(done, types::I8);
    let zero = builder.ins().iconst(pointer_type(), 0);
    let initially_equal = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(
        header,
        &[BlockArg::from(zero), BlockArg::from(initially_equal)],
    );

    builder.switch_to_block(header);
    let index = builder
        .block_params(header)
        .first()
        .copied()
        .ok_or_else(|| "array equality loop has no index".to_owned())?;
    let equal = builder
        .block_params(header)
        .get(1)
        .copied()
        .ok_or_else(|| "array equality loop has no accumulator".to_owned())?;
    let length = i64::try_from(length)
        .map_err(|_| "array equality length exceeds native addressing".to_owned())?;
    let length = builder.ins().iconst(pointer_type(), length);
    let has_element = builder.ins().icmp(IntCC::UnsignedLessThan, index, length);
    builder
        .ins()
        .brif(has_element, body, &[], done, &[BlockArg::from(equal)]);

    builder.switch_to_block(body);
    let offset = builder.ins().imul_imm_u(index, i64::from(stride));
    let left_address = builder.ins().iadd(left, offset);
    let right_address = builder.ins().iadd(right, offset);
    let left_element = load_at_address(builder, element, left_address)?;
    let right_element = load_at_address(builder, element, right_address)?;
    let element_equal = if element.has_runtime_value() {
        emit_value_equality(
            builder,
            module,
            state,
            element,
            require_value(left_element, "left array element")?,
            require_value(right_element, "right array element")?,
        )?
    } else {
        builder.ins().iconst(types::I8, 1)
    };
    let equal = builder.ins().band(equal, element_equal);
    let next = builder.ins().iadd_imm_u(index, 1);
    builder
        .ins()
        .jump(header, &[BlockArg::from(next), BlockArg::from(equal)]);

    builder.switch_to_block(done);
    builder
        .block_params(done)
        .first()
        .copied()
        .ok_or_else(|| "array equality result is missing".to_owned())
}

fn emit_enum_equality<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    state: &CodegenState<'_>,
    variants: &[Vec<Type>],
    offsets: &[Vec<u32>],
    left: Value,
    right: Value,
) -> Result<Value, String> {
    if variants.len() != offsets.len() {
        return Err("enum equality metadata has inconsistent variant counts".to_owned());
    }
    let dispatch = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, types::I8);
    let left_discriminant = builder.ins().load(types::I32, MemFlagsData::new(), left, 0);
    let right_discriminant = builder
        .ins()
        .load(types::I32, MemFlagsData::new(), right, 0);
    let same_variant = builder
        .ins()
        .icmp(IntCC::Equal, left_discriminant, right_discriminant);
    let not_equal = builder.ins().iconst(types::I8, 0);
    builder.ins().brif(
        same_variant,
        dispatch,
        &[],
        merge,
        &[BlockArg::from(not_equal)],
    );

    builder.switch_to_block(dispatch);
    for (index, (fields, field_offsets)) in variants.iter().zip(offsets).enumerate() {
        let variant_block = builder.create_block();
        let next = builder.create_block();
        let discriminant = i64::try_from(index)
            .map_err(|_| "enum variant index exceeds native discriminant".to_owned())?;
        let selected = builder
            .ins()
            .icmp_imm_s(IntCC::Equal, left_discriminant, discriminant);
        builder.ins().brif(selected, variant_block, &[], next, &[]);
        builder.switch_to_block(variant_block);
        let equal =
            emit_product_equality(builder, module, state, fields, field_offsets, left, right)?;
        builder.ins().jump(merge, &[BlockArg::from(equal)]);
        builder.switch_to_block(next);
    }
    builder.ins().jump(merge, &[BlockArg::from(not_equal)]);
    builder.switch_to_block(merge);
    builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "enum equality result is missing".to_owned())
}

fn emit_string_equality<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    layouts: &Layouts,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    let layout = layouts.aggregate(Type::Str)?;
    let AggregateLayoutKind::Slice {
        data_offset,
        length_offset,
    } = layout.kind
    else {
        return Err("string equality requires a string view layout".to_owned());
    };
    let left_data = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        left,
        i32::try_from(data_offset).map_err(|_| "string data offset exceeds i32".to_owned())?,
    );
    let left_length = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        left,
        i32::try_from(length_offset).map_err(|_| "string length offset exceeds i32".to_owned())?,
    );
    let right_data = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        right,
        i32::try_from(data_offset).map_err(|_| "string data offset exceeds i32".to_owned())?,
    );
    let right_length = builder.ins().load(
        pointer_type(),
        MemFlagsData::new(),
        right,
        i32::try_from(length_offset).map_err(|_| "string length offset exceeds i32".to_owned())?,
    );
    let function = runtime_buffer_equals_reference(builder, module)?;
    let call = builder.ins().call(
        function,
        &[left_data, left_length, right_data, right_length],
    );
    builder
        .inst_results(call)
        .first()
        .copied()
        .ok_or_else(|| "string equality call has no result".to_owned())
}

fn emit_checked_arithmetic<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    operator: BinaryOperator,
    ty: Type,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    let (value, overflow) = match (operator, ty.is_signed_integer()) {
        (BinaryOperator::Add, true) => builder.ins().sadd_overflow(left, right),
        (BinaryOperator::Subtract, true) => builder.ins().ssub_overflow(left, right),
        (BinaryOperator::Multiply, true) => builder.ins().smul_overflow(left, right),
        (BinaryOperator::Add, false) => builder.ins().uadd_overflow(left, right),
        (BinaryOperator::Subtract, false) => builder.ins().usub_overflow(left, right),
        (BinaryOperator::Multiply, false) => builder.ins().umul_overflow(left, right),
        _ => return Ok(left),
    };
    emit_runtime_failure_if(builder, module, overflow, Failure::Overflow)?;
    Ok(value)
}

fn emit_checked_division<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    operator: BinaryOperator,
    ty: Type,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    let divisor_is_zero = builder.ins().icmp_imm_s(IntCC::Equal, right, 0);
    emit_runtime_failure_if(builder, module, divisor_is_zero, Failure::DivisionByZero)?;
    if ty.is_signed_integer() {
        let bits = ty
            .integer_bits()
            .ok_or_else(|| format!("`{ty}` has no integer width"))?;
        let minimum = emit_integer_constant(builder, ty, 1_u128 << (bits - 1))?;
        let negative_one = emit_integer_constant(builder, ty, integer_bit_mask(bits))?;
        let left_is_min = builder.ins().icmp(IntCC::Equal, left, minimum);
        let right_is_negative_one = builder.ins().icmp(IntCC::Equal, right, negative_one);
        let overflows = builder.ins().band(left_is_min, right_is_negative_one);
        emit_runtime_failure_if(builder, module, overflows, Failure::Overflow)?;
    }

    Ok(match (operator, ty.is_signed_integer()) {
        (BinaryOperator::Divide, true) => builder.ins().sdiv(left, right),
        (BinaryOperator::Remainder, true) => builder.ins().srem(left, right),
        (BinaryOperator::Divide, false) => builder.ins().udiv(left, right),
        (BinaryOperator::Remainder, false) => builder.ins().urem(left, right),
        _ => left,
    })
}

#[derive(Debug, Clone, Copy)]
enum IntRelation {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

fn integer_comparison(ty: Type, relation: IntRelation) -> IntCC {
    match (ty.is_signed_integer(), relation) {
        (true, IntRelation::Less) => IntCC::SignedLessThan,
        (true, IntRelation::LessEqual) => IntCC::SignedLessThanOrEqual,
        (true, IntRelation::Greater) => IntCC::SignedGreaterThan,
        (true, IntRelation::GreaterEqual) => IntCC::SignedGreaterThanOrEqual,
        (false, IntRelation::Less) => IntCC::UnsignedLessThan,
        (false, IntRelation::LessEqual) => IntCC::UnsignedLessThanOrEqual,
        (false, IntRelation::Greater) => IntCC::UnsignedGreaterThan,
        (false, IntRelation::GreaterEqual) => IntCC::UnsignedGreaterThanOrEqual,
    }
}

fn integer_bit_mask(bits: u32) -> u128 {
    if bits == 128 {
        u128::MAX
    } else {
        (1_u128 << bits) - 1
    }
}

fn emit_checked_shift<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    operator: BinaryOperator,
    ty: Type,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    let bits = ty
        .integer_bits()
        .ok_or_else(|| format!("`{ty}` has no integer width"))?;
    let width = emit_integer_constant(builder, ty, u128::from(bits))?;
    let invalid = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, right, width);
    emit_runtime_failure_if(builder, module, invalid, Failure::InvalidShift)?;

    Ok(match operator {
        BinaryOperator::ShiftLeft => builder.ins().ishl(left, right),
        BinaryOperator::ShiftRight if ty.is_signed_integer() => builder.ins().sshr(left, right),
        BinaryOperator::ShiftRight => builder.ins().ushr(left, right),
        _ => left,
    })
}

fn emit_short_circuit<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    operator: BinaryOperator,
    left: &Expression,
    right: &Expression,
) -> Result<Emitted, String> {
    let left = emit_expression(builder, module, functions, state, left)?;
    if left.terminated {
        return Ok(left);
    }
    let left = require_value(left, "left logical operand")?;
    let right_block = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, types::I8);
    let direct = builder
        .ins()
        .iconst(types::I8, i64::from(matches!(operator, BinaryOperator::Or)));

    let direct_arguments = [BlockArg::from(direct)];
    if operator == BinaryOperator::And {
        builder
            .ins()
            .brif(left, right_block, &[], merge, &direct_arguments);
    } else {
        builder
            .ins()
            .brif(left, merge, &direct_arguments, right_block, &[]);
    }

    builder.switch_to_block(right_block);
    let right = emit_expression(builder, module, functions, state, right)?;
    if !right.terminated {
        let right_arguments = [BlockArg::from(require_value(
            right,
            "right logical operand",
        )?)];
        builder.ins().jump(merge, &right_arguments);
    }

    builder.switch_to_block(merge);
    let value = builder
        .block_params(merge)
        .first()
        .copied()
        .ok_or_else(|| "logical merge block has no result".to_owned())?;
    Ok(Emitted::value(value))
}

fn emit_call<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    function: FunctionId,
    arguments: &[Expression],
    return_type: Type,
) -> Result<Emitted, String> {
    let function = functions
        .get(&function)
        .copied()
        .ok_or_else(|| format!("called function {} was not declared", function.0))?;
    let mut values = Vec::with_capacity(arguments.len() + usize::from(return_type.is_composite()));
    let return_destination = if return_type.is_composite() {
        let destination = allocate_composite(builder, state.layouts, return_type)?;
        values.push(destination);
        Some(destination)
    } else {
        None
    };
    for argument in arguments {
        let emitted = emit_expression(builder, module, functions, state, argument)?;
        if emitted.terminated {
            return Ok(emitted);
        }
        if argument.ty.has_runtime_value() {
            values.push(require_value(emitted, "call argument")?);
        }
    }
    let function = module.declare_func_in_func(function, builder.func);
    let call = builder.ins().call(function, &values);
    if let Some(destination) = return_destination {
        Ok(Emitted::value(destination))
    } else if return_type.has_runtime_value() {
        let value = builder
            .inst_results(call)
            .first()
            .copied()
            .ok_or_else(|| "value-returning call has no result".to_owned())?;
        Ok(Emitted::value(value))
    } else {
        Ok(Emitted::unit())
    }
}

fn emit_function_address<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    function: FunctionId,
) -> Result<Emitted, String> {
    let function = functions
        .get(&function)
        .copied()
        .ok_or_else(|| format!("referenced function {} was not declared", function.0))?;
    let function = module.declare_func_in_func(function, builder.func);
    Ok(Emitted::value(
        builder.ins().func_addr(pointer_type(), function),
    ))
}

fn emit_indirect_call<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    callee: &Expression,
    arguments: &[Expression],
    return_type: Type,
) -> Result<Emitted, String> {
    let emitted_callee = emit_expression(builder, module, functions, state, callee)?;
    if emitted_callee.terminated {
        return Ok(emitted_callee);
    }
    let callee_value = require_value(emitted_callee, "indirect call target")?;
    let (parameter_types, declared_return) = state
        .layouts
        .function_shape(callee.ty)
        .ok_or_else(|| format!("type `{}` has no function signature", callee.ty))?;
    if declared_return != return_type {
        return Err("indirect call result type differs from its signature".to_owned());
    }
    let signature = typed_function_signature(module, parameter_types.iter().copied(), return_type)?;
    let signature = builder.import_signature(signature);
    let mut values = Vec::with_capacity(arguments.len() + usize::from(return_type.is_composite()));
    let return_destination = if return_type.is_composite() {
        let destination = allocate_composite(builder, state.layouts, return_type)?;
        values.push(destination);
        Some(destination)
    } else {
        None
    };
    for argument in arguments {
        let emitted = emit_expression(builder, module, functions, state, argument)?;
        if emitted.terminated {
            return Ok(emitted);
        }
        if argument.ty.has_runtime_value() {
            values.push(require_value(emitted, "indirect call argument")?);
        }
    }
    let call = builder
        .ins()
        .call_indirect(signature, callee_value, &values);
    if let Some(destination) = return_destination {
        Ok(Emitted::value(destination))
    } else if return_type.has_runtime_value() {
        let value = builder
            .inst_results(call)
            .first()
            .copied()
            .ok_or_else(|| "value-returning indirect call has no result".to_owned())?;
        Ok(Emitted::value(value))
    } else {
        Ok(Emitted::unit())
    }
}

fn emit_loop<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    body: &Block,
    result_type: Type,
) -> Result<Emitted, String> {
    let header = builder.create_block();
    let exit = builder.create_block();
    if result_type.has_runtime_value() {
        builder.append_block_param(exit, runtime_type(result_type)?);
    }
    builder.ins().jump(header, &[]);
    builder.switch_to_block(header);
    state.loops.push(LoopTargets {
        continue_target: header,
        exit,
        result_type,
        defer_depth: state.defer_scopes.len(),
    });
    let emitted = emit_block(builder, module, functions, state, body)?;
    state.loops.pop();
    if !emitted.terminated {
        builder.ins().jump(header, &[]);
    }
    if result_type == Type::Never {
        return Ok(Emitted::terminated());
    }
    builder.switch_to_block(exit);
    if result_type.has_runtime_value() {
        let value = builder
            .block_params(exit)
            .first()
            .copied()
            .ok_or_else(|| "loop exit block has no result".to_owned())?;
        Ok(Emitted::value(value))
    } else {
        Ok(Emitted::unit())
    }
}

fn emit_match<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &MatchExpression,
    result_type: Type,
) -> Result<Emitted, String> {
    let scrutinee = emit_expression(builder, module, functions, state, &expression.scrutinee)?;
    if scrutinee.terminated {
        return Ok(scrutinee);
    }
    let scrutinee = if expression.scrutinee.ty.has_runtime_value() {
        require_value(scrutinee, "match scrutinee")?
    } else {
        builder.ins().iconst(types::I8, 0)
    };
    let merge = builder.create_block();
    if result_type.has_runtime_value() {
        builder.append_block_param(merge, runtime_type(result_type)?);
    }
    let mut reaches_merge = false;
    for arm in &expression.arms {
        let selected = builder.create_block();
        let next = builder.create_block();
        emit_pattern_branch(builder, state, &arm.pattern, scrutinee, selected, next)?;
        builder.switch_to_block(selected);
        bind_pattern(builder, state, &arm.pattern, scrutinee)?;
        let body_block = emit_match_guard(builder, module, functions, state, arm, next)?;
        if let Some(body_block) = body_block {
            builder.switch_to_block(body_block);
            let emitted = emit_expression(builder, module, functions, state, &arm.body)?;
            if !emitted.terminated {
                jump_to_merge(builder, merge, result_type, emitted, "match arm")?;
                reaches_merge = true;
            }
        }
        builder.switch_to_block(next);
    }
    emit_runtime_failure(builder, module, Failure::NonExhaustiveMatch)?;
    if !reaches_merge {
        return Ok(Emitted::terminated());
    }
    builder.switch_to_block(merge);
    if result_type.has_runtime_value() {
        let value = builder
            .block_params(merge)
            .first()
            .copied()
            .ok_or_else(|| "match merge block has no result".to_owned())?;
        Ok(Emitted::value(value))
    } else {
        Ok(Emitted::unit())
    }
}

fn emit_match_guard<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    arm: &reimer_hir::MatchArm,
    failure: ir::Block,
) -> Result<Option<ir::Block>, String> {
    let Some(guard) = &arm.guard else {
        let body = builder.create_block();
        builder.ins().jump(body, &[]);
        return Ok(Some(body));
    };
    let emitted = emit_expression(builder, module, functions, state, guard)?;
    if emitted.terminated {
        return Ok(None);
    }
    let condition = require_value(emitted, "match guard")?;
    let body = builder.create_block();
    builder.ins().brif(condition, body, &[], failure, &[]);
    Ok(Some(body))
}

fn emit_pattern_branch(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    pattern: &Pattern,
    value: Value,
    success: ir::Block,
    failure: ir::Block,
) -> Result<(), String> {
    match &pattern.kind {
        PatternKind::Wildcard | PatternKind::Binding { .. } => {
            builder.ins().jump(success, &[]);
        }
        PatternKind::Integer(expected) => {
            let expected = emit_integer_constant(builder, pattern.ty, *expected)?;
            branch_on_scalar_equality(builder, pattern.ty, value, expected, success, failure);
        }
        PatternKind::Float32(bits) => {
            let expected = builder.ins().f32const(Ieee32::with_bits(*bits));
            branch_on_scalar_equality(builder, pattern.ty, value, expected, success, failure);
        }
        PatternKind::Float64(bits) => {
            let expected = builder.ins().f64const(Ieee64::with_bits(*bits));
            branch_on_scalar_equality(builder, pattern.ty, value, expected, success, failure);
        }
        PatternKind::Character(expected) => {
            let expected = builder
                .ins()
                .iconst(types::I32, i64::from(u32::from(*expected)));
            branch_on_scalar_equality(builder, pattern.ty, value, expected, success, failure);
        }
        PatternKind::Boolean(expected) => {
            let expected = builder.ins().iconst(types::I8, i64::from(*expected));
            branch_on_scalar_equality(builder, pattern.ty, value, expected, success, failure);
        }
        PatternKind::Tuple(fields) => {
            emit_product_pattern_branch(builder, state, pattern, value, fields, success, failure)?;
        }
        PatternKind::Enum { variant, fields } => {
            emit_enum_pattern_branch(
                builder,
                state,
                pattern,
                value,
                *variant,
                fields,
                BranchTargets { success, failure },
            )?;
        }
    }
    Ok(())
}

fn branch_on_scalar_equality(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    value: Value,
    expected: Value,
    success: ir::Block,
    failure: ir::Block,
) {
    let condition = if ty.is_float() {
        builder.ins().fcmp(FloatCC::Equal, value, expected)
    } else {
        builder.ins().icmp(IntCC::Equal, value, expected)
    };
    builder.ins().brif(condition, success, &[], failure, &[]);
}

fn emit_product_pattern_branch(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    pattern: &Pattern,
    value: Value,
    fields: &[Pattern],
    success: ir::Block,
    failure: ir::Block,
) -> Result<(), String> {
    if fields.is_empty() {
        builder.ins().jump(success, &[]);
        return Ok(());
    }
    for (index, field) in fields.iter().enumerate() {
        let next = if index + 1 == fields.len() {
            success
        } else {
            builder.create_block()
        };
        let field_value =
            load_product_pattern_field(builder, state, pattern.ty, value, index, field.ty)?;
        emit_pattern_branch(builder, state, field, field_value, next, failure)?;
        if next != success {
            builder.switch_to_block(next);
        }
    }
    Ok(())
}

fn emit_enum_pattern_branch(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    pattern: &Pattern,
    value: Value,
    variant: u32,
    fields: &[Pattern],
    targets: BranchTargets,
) -> Result<(), String> {
    let discriminant = builder
        .ins()
        .load(types::I32, MemFlagsData::new(), value, 0);
    let expected = builder.ins().iconst(types::I32, i64::from(variant));
    let matches = builder.ins().icmp(IntCC::Equal, discriminant, expected);
    if fields.is_empty() {
        builder
            .ins()
            .brif(matches, targets.success, &[], targets.failure, &[]);
        return Ok(());
    }
    let payload = builder.create_block();
    builder
        .ins()
        .brif(matches, payload, &[], targets.failure, &[]);
    builder.switch_to_block(payload);
    for (index, field) in fields.iter().enumerate() {
        let next = if index + 1 == fields.len() {
            targets.success
        } else {
            builder.create_block()
        };
        let field_value =
            load_enum_pattern_field(builder, state, pattern.ty, value, variant, index, field.ty)?;
        emit_pattern_branch(builder, state, field, field_value, next, targets.failure)?;
        if next != targets.success {
            builder.switch_to_block(next);
        }
    }
    Ok(())
}

fn bind_pattern(
    builder: &mut FunctionBuilder<'_>,
    state: &mut CodegenState<'_>,
    pattern: &Pattern,
    value: Value,
) -> Result<(), String> {
    match &pattern.kind {
        PatternKind::Binding { local, .. } => {
            if pattern.ty.has_runtime_value() {
                define_local(builder, state, *local, pattern.ty, value)?;
            }
        }
        PatternKind::Tuple(fields) => {
            for (index, field) in fields.iter().enumerate() {
                let field_value =
                    load_product_pattern_field(builder, state, pattern.ty, value, index, field.ty)?;
                bind_pattern(builder, state, field, field_value)?;
            }
        }
        PatternKind::Enum { variant, fields } => {
            for (index, field) in fields.iter().enumerate() {
                let field_value = load_enum_pattern_field(
                    builder, state, pattern.ty, value, *variant, index, field.ty,
                )?;
                bind_pattern(builder, state, field, field_value)?;
            }
        }
        PatternKind::Wildcard
        | PatternKind::Integer(_)
        | PatternKind::Float32(_)
        | PatternKind::Float64(_)
        | PatternKind::Character(_)
        | PatternKind::Boolean(_) => {}
    }
    Ok(())
}

fn load_product_pattern_field(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    parent_type: Type,
    base: Value,
    index: usize,
    field_type: Type,
) -> Result<Value, String> {
    let layout = state.layouts.aggregate(parent_type)?;
    let AggregateLayoutKind::Product { offsets } = &layout.kind else {
        return Err("tuple pattern requires a product layout".to_owned());
    };
    let offset = offsets
        .get(index)
        .copied()
        .ok_or_else(|| format!("tuple pattern field {index} has no native offset"))?;
    let address = address_at_offset(builder, base, offset);
    load_pattern_value(builder, field_type, address)
}

fn load_enum_pattern_field(
    builder: &mut FunctionBuilder<'_>,
    state: &CodegenState<'_>,
    enum_type: Type,
    base: Value,
    variant: u32,
    index: usize,
    field_type: Type,
) -> Result<Value, String> {
    let layout = state.layouts.aggregate(enum_type)?;
    let AggregateLayoutKind::Enum { variants } = &layout.kind else {
        return Err("enum pattern requires an enum layout".to_owned());
    };
    let offset = variants
        .get(type_index(TypeId(variant))?)
        .and_then(|offsets| offsets.get(index))
        .copied()
        .ok_or_else(|| format!("enum pattern field {index} has no native offset"))?;
    let address = address_at_offset(builder, base, offset);
    load_pattern_value(builder, field_type, address)
}

fn load_pattern_value(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    address: Value,
) -> Result<Value, String> {
    if ty.is_composite() || !ty.has_runtime_value() {
        Ok(address)
    } else {
        require_value(load_at_address(builder, ty, address)?, "pattern field")
    }
}

fn emit_if<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    expression: &reimer_hir::IfExpression,
    result_type: Type,
) -> Result<Emitted, String> {
    let condition = emit_expression(builder, module, functions, state, &expression.condition)?;
    if condition.terminated {
        return Ok(condition);
    }
    let condition = require_value(condition, "if condition")?;
    let then_block = builder.create_block();
    let else_block = builder.create_block();
    let merge = builder.create_block();
    if result_type.has_runtime_value() {
        builder.append_block_param(merge, runtime_type(result_type)?);
    }
    builder
        .ins()
        .brif(condition, then_block, &[], else_block, &[]);

    let mut reaches_merge = false;
    builder.switch_to_block(then_block);
    let then_value = emit_block(builder, module, functions, state, &expression.then_branch)?;
    if !then_value.terminated {
        jump_to_merge(builder, merge, result_type, then_value, "then branch")?;
        reaches_merge = true;
    }

    builder.switch_to_block(else_block);
    let else_value = if let Some(else_branch) = &expression.else_branch {
        emit_expression(builder, module, functions, state, else_branch)?
    } else {
        Emitted::unit()
    };
    if !else_value.terminated {
        jump_to_merge(builder, merge, result_type, else_value, "else branch")?;
        reaches_merge = true;
    }

    if !reaches_merge {
        return Ok(Emitted::terminated());
    }
    builder.switch_to_block(merge);
    if result_type.has_runtime_value() {
        let value = builder
            .block_params(merge)
            .first()
            .copied()
            .ok_or_else(|| "if merge block has no result".to_owned())?;
        Ok(Emitted::value(value))
    } else {
        Ok(Emitted::unit())
    }
}

fn jump_to_merge(
    builder: &mut FunctionBuilder<'_>,
    merge: ir::Block,
    result_type: Type,
    emitted: Emitted,
    role: &str,
) -> Result<(), String> {
    if result_type.has_runtime_value() {
        let arguments = [BlockArg::from(require_value(emitted, role)?)];
        builder.ins().jump(merge, &arguments);
    } else {
        builder.ins().jump(merge, &[]);
    }
    Ok(())
}

fn emit_assignment<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    target: &Place,
    operator: AssignmentOperator,
    value: &Expression,
) -> Result<Emitted, String> {
    let value_type = value.ty;
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    if value.ty == Type::Unit {
        return Ok(Emitted::unit());
    }
    let right = require_value(emitted, "assignment value")?;
    match &target.kind {
        PlaceKind::Local(local) => {
            let storage =
                state.locals.get(local).copied().ok_or_else(|| {
                    format!("assignment target {} has no native storage", local.0)
                })?;
            match storage {
                LocalStorage::Variable(variable) => {
                    if target.ty.is_composite() {
                        if operator != AssignmentOperator::Assign {
                            return Err(
                                "compound assignment cannot target a composite value".to_owned()
                            );
                        }
                        let destination = builder.use_var(variable);
                        copy_composite(
                            builder,
                            state.layouts,
                            state.target_config,
                            target.ty,
                            destination,
                            right,
                        )?;
                    } else {
                        let value = if operator == AssignmentOperator::Assign {
                            right
                        } else {
                            let left = builder.use_var(variable);
                            emit_compound_assignment(
                                builder, module, operator, value_type, left, right,
                            )?
                        };
                        builder.def_var(variable, value);
                    }
                }
                LocalStorage::Address(address) => {
                    let value = if operator == AssignmentOperator::Assign {
                        right
                    } else {
                        let left = builder.ins().load(
                            runtime_type(target.ty)?,
                            MemFlagsData::new(),
                            address,
                            0,
                        );
                        emit_compound_assignment(
                            builder, module, operator, value_type, left, right,
                        )?
                    };
                    builder.ins().store(MemFlagsData::new(), value, address, 0);
                }
            }
        }
        PlaceKind::Static(_)
        | PlaceKind::Field { .. }
        | PlaceKind::Index { .. }
        | PlaceKind::Dereference { .. } => {
            let address = emit_place_address(builder, module, functions, state, target)?;
            if target.ty.is_composite() {
                copy_composite(
                    builder,
                    state.layouts,
                    state.target_config,
                    target.ty,
                    address,
                    right,
                )?;
            } else {
                let value = if operator == AssignmentOperator::Assign {
                    right
                } else {
                    let left = builder.ins().load(
                        runtime_type(target.ty)?,
                        MemFlagsData::new(),
                        address,
                        0,
                    );
                    emit_compound_assignment(builder, module, operator, target.ty, left, right)?
                };
                builder.ins().store(MemFlagsData::new(), value, address, 0);
            }
        }
    }
    Ok(Emitted::unit())
}

fn emit_place_address<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    place: &Place,
) -> Result<Value, String> {
    match &place.kind {
        PlaceKind::Local(local) => {
            let storage = state
                .locals
                .get(local)
                .copied()
                .ok_or_else(|| format!("local {} has no native storage", local.0))?;
            Ok(match storage {
                LocalStorage::Variable(variable) => builder.use_var(variable),
                LocalStorage::Address(address) => address,
            })
        }
        PlaceKind::Static(value) => static_address(builder, module, state, *value),
        PlaceKind::Field { base, field } => {
            let layout = state.layouts.aggregate(base.ty)?;
            let AggregateLayoutKind::Product { offsets } = &layout.kind else {
                return Err("field place requires a product layout".to_owned());
            };
            let offset = offsets
                .get(type_index(TypeId(*field))?)
                .copied()
                .ok_or_else(|| format!("field {field} has no native offset"))?;
            let base = emit_expression(builder, module, functions, state, base)?;
            let base = require_value(base, "field assignment base")?;
            Ok(address_at_offset(builder, base, offset))
        }
        PlaceKind::Index { base, index } => {
            emit_array_element_address(builder, module, functions, state, base, index)
        }
        PlaceKind::Dereference { pointer } => {
            let emitted = emit_expression(builder, module, functions, state, pointer)?;
            require_value(emitted, "dereference place")
        }
    }
}

fn emit_compound_assignment<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    operator: AssignmentOperator,
    ty: Type,
    left: Value,
    right: Value,
) -> Result<Value, String> {
    Ok(match operator {
        AssignmentOperator::Add if ty.is_float() => builder.ins().fadd(left, right),
        AssignmentOperator::Subtract if ty.is_float() => builder.ins().fsub(left, right),
        AssignmentOperator::Multiply if ty.is_float() => builder.ins().fmul(left, right),
        AssignmentOperator::Divide if ty.is_float() => builder.ins().fdiv(left, right),
        AssignmentOperator::Add => {
            emit_checked_arithmetic(builder, module, BinaryOperator::Add, ty, left, right)?
        }
        AssignmentOperator::Subtract => {
            emit_checked_arithmetic(builder, module, BinaryOperator::Subtract, ty, left, right)?
        }
        AssignmentOperator::Multiply => {
            emit_checked_arithmetic(builder, module, BinaryOperator::Multiply, ty, left, right)?
        }
        AssignmentOperator::Divide => {
            emit_checked_division(builder, module, BinaryOperator::Divide, ty, left, right)?
        }
        AssignmentOperator::Remainder => {
            emit_checked_division(builder, module, BinaryOperator::Remainder, ty, left, right)?
        }
        AssignmentOperator::BitAnd => builder.ins().band(left, right),
        AssignmentOperator::BitXor => builder.ins().bxor(left, right),
        AssignmentOperator::BitOr => builder.ins().bor(left, right),
        AssignmentOperator::ShiftLeft => {
            emit_checked_shift(builder, module, BinaryOperator::ShiftLeft, ty, left, right)?
        }
        AssignmentOperator::ShiftRight => {
            emit_checked_shift(builder, module, BinaryOperator::ShiftRight, ty, left, right)?
        }
        AssignmentOperator::Assign => right,
    })
}

fn emit_cast<M: Module>(
    builder: &mut FunctionBuilder<'_>,
    module: &mut M,
    functions: &HashMap<FunctionId, FuncId>,
    state: &mut CodegenState<'_>,
    value: &Expression,
    target: Type,
) -> Result<Emitted, String> {
    let source = value.ty;
    let emitted = emit_expression(builder, module, functions, state, value)?;
    if emitted.terminated {
        return Ok(emitted);
    }
    let value = require_value(emitted, "cast operand")?;
    if source == target {
        return Ok(Emitted::value(value));
    }

    let converted = if (source.is_thin_pointer() && target.is_thin_pointer())
        || (source.is_thin_pointer() && target == Type::Usize)
        || (source == Type::Usize && matches!(target, Type::RawPointer(_)))
    {
        value
    } else if (source.is_integer() || source == Type::Char) && target.is_integer() {
        let source_bits = if source == Type::Char {
            32
        } else {
            source
                .integer_bits()
                .ok_or_else(|| format!("`{source}` has no integer width"))?
        };
        let target_bits = target
            .integer_bits()
            .ok_or_else(|| format!("`{target}` has no integer width"))?;
        let target_type = runtime_type(target)?;
        if target_bits < source_bits {
            builder.ins().ireduce(target_type, value)
        } else if target_bits > source_bits && source.is_signed_integer() {
            builder.ins().sextend(target_type, value)
        } else if target_bits > source_bits {
            builder.ins().uextend(target_type, value)
        } else {
            value
        }
    } else if source.is_integer() && target.is_float() {
        let target_type = runtime_type(target)?;
        if source.is_signed_integer() {
            builder.ins().fcvt_from_sint(target_type, value)
        } else {
            builder.ins().fcvt_from_uint(target_type, value)
        }
    } else if source.is_float() && target.is_float() {
        if source == Type::F32 {
            builder.ins().fpromote(runtime_type(target)?, value)
        } else {
            builder.ins().fdemote(runtime_type(target)?, value)
        }
    } else {
        return Err(format!(
            "invalid typed cast from `{source}` to `{target}` reached code generation"
        ));
    };
    Ok(Emitted::value(converted))
}

fn runtime_type(ty: Type) -> Result<ir::Type, String> {
    match ty {
        Type::I8 | Type::U8 | Type::Bool => Ok(types::I8),
        Type::I16 | Type::U16 => Ok(types::I16),
        Type::I32 | Type::U32 | Type::Char => Ok(types::I32),
        Type::I64 | Type::U64 => Ok(types::I64),
        Type::I128 | Type::U128 => Ok(types::I128),
        Type::Isize | Type::Usize => {
            if usize::BITS == 64 {
                Ok(types::I64)
            } else {
                Ok(types::I32)
            }
        }
        Type::F32 => Ok(types::F32),
        Type::F64 => Ok(types::F64),
        Type::Reference(_)
        | Type::RawPointer(_)
        | Type::Function(_)
        | Type::CStr
        | Type::Struct(_)
        | Type::Enum(_)
        | Type::Tuple(_)
        | Type::Array(_)
        | Type::Slice(_)
        | Type::Str => Ok(pointer_type()),
        Type::Unit | Type::Never => Err(format!("type `{ty}` has no runtime value")),
    }
}

fn pointer_type() -> ir::Type {
    if usize::BITS == 64 {
        types::I64
    } else {
        types::I32
    }
}

fn require_value(emitted: Emitted, role: &str) -> Result<Value, String> {
    emitted
        .value
        .ok_or_else(|| format!("{role} did not produce a runtime value"))
}

#[expect(
    unsafe_code,
    reason = "Cranelift exposes finalized JIT functions as raw instruction pointers"
)]
fn call_jit_entry(pointer: *const u8) -> Result<i32, Vec<Diagnostic>> {
    if pointer.is_null() {
        return Err(backend_error("Cranelift returned a null entry pointer"));
    }

    // SAFETY: The live `JITModule` finalized this pointer for the validated
    // zero-argument, i32-returning `main` signature.
    let function = unsafe { std::mem::transmute::<*const u8, extern "C" fn() -> i32>(pointer) };
    Ok(function())
}

#[expect(
    unsafe_code,
    reason = "Cranelift exposes finalized JIT functions as raw instruction pointers"
)]
fn call_jit_unit(pointer: *const u8) -> Result<(), Vec<Diagnostic>> {
    if pointer.is_null() {
        return Err(backend_error("Cranelift returned a null unit-test pointer"));
    }

    // SAFETY: The resolver only records zero-argument, unit-returning functions
    // as tests, and the live JIT module finalized this pointer for that ABI.
    let function = unsafe { std::mem::transmute::<*const u8, extern "C" fn()>(pointer) };
    function();
    Ok(())
}

fn backend_error(message: impl Into<String>) -> Vec<Diagnostic> {
    vec![
        Diagnostic::error(
            "E9001",
            format!("native backend failed: {}", message.into()),
            Span::empty(0),
        )
        .with_help("this is a compiler backend error, not a source-language error"),
    ]
}

#[cfg(test)]
mod tests {
    use cranelift_object::object::{File, Object, ObjectSection, ObjectSymbol};
    use reimer_lexer::lex;
    use reimer_parser::parse;
    use reimer_resolver::resolve;

    use super::{
        OptimizationLevel, emit_object, emit_object_with_options, execute, execute_test,
        execute_with_options,
    };

    fn compile_fixture(source: &str) -> reimer_hir::Program {
        let tokens = lex(source).expect("fixture should lex");
        let ast = parse(&tokens).expect("fixture should parse");
        resolve(&ast).expect("fixture should resolve")
    }

    #[test]
    fn emit_object_should_produce_symbols_for_multiple_functions() {
        let program =
            compile_fixture("pub fn answer() -> i32 { 42 } fn main() -> i32 { answer() }");

        let bytes = emit_object(&program).expect("fixture should compile");
        let object = File::parse(bytes.as_slice()).expect("artifact should be an object");

        assert!(object.symbols().any(|symbol| {
            symbol
                .name()
                .is_ok_and(|name| name.contains("function_answer"))
        }));
    }

    #[test]
    fn emit_object_should_define_static_data_symbols() {
        let program = compile_fixture("pub static ANSWER: i32 = 42; fn main() -> i32 { ANSWER }");

        let bytes = emit_object(&program).expect("fixture should compile");
        let object = File::parse(bytes.as_slice()).expect("artifact should be an object");

        assert!(object.symbols().any(|symbol| {
            symbol
                .name()
                .is_ok_and(|name| name.contains("static_ANSWER"))
        }));
    }

    #[test]
    fn emit_object_with_options_should_accept_speed_optimization() {
        let program = compile_fixture("fn main() -> i32 { 42 }");

        let bytes = emit_object_with_options(&program, OptimizationLevel::Speed)
            .expect("optimized fixture should compile");

        assert!(!bytes.is_empty());
    }

    #[test]
    fn execute_with_options_should_accept_size_aware_optimization() {
        let program = compile_fixture("fn main() -> i32 { 42 }");

        let result = execute_with_options(&program, OptimizationLevel::SpeedAndSize)
            .expect("optimized fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_check_assertions_in_every_profile() {
        let program = compile_fixture(
            "fn mark(value: &mut i32) -> bool { *value = 42; true }
             fn main() -> i32 {
                 let mut value = 0;
                 assert(mark(&mut value), \"mark should succeed\");
                 value
             }",
        );

        let result = execute_with_options(&program, OptimizationLevel::Speed)
            .expect("optimized assertion should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_omit_debug_assertion_operands_in_optimized_profiles() {
        let program = compile_fixture(
            "fn mark(value: &mut i32) -> bool { *value = 42; true }
             fn main() -> i32 {
                 let mut value = 0;
                 debug_assert(mark(&mut value), \"mark should succeed\");
                 value
             }",
        );

        let debug = execute_with_options(&program, OptimizationLevel::None)
            .expect("debug assertion should execute");
        let optimized = execute_with_options(&program, OptimizationLevel::Speed)
            .expect("optimized fixture should execute");

        assert_eq!(debug, 42);
        assert_eq!(optimized, 0);
    }

    #[test]
    fn execute_test_should_run_a_discovered_unit_test() {
        let program = compile_fixture(
            "@test
             fn arithmetic_should_work() {
                 if 20 + 22 != 42 { panic(\"unexpected arithmetic result\"); }
             }
             fn main() -> i32 { 0 }",
        );

        execute_test(&program, 0).expect("annotated test should execute");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn emit_object_should_preserve_link_libraries_for_the_native_linker() {
        let program = compile_fixture(
            "@link(\"kernel32\") extern \"C\" { fn GetCurrentProcessId() -> u32; }
             fn main() -> i32 { 42 }",
        );

        let bytes = emit_object(&program).expect("fixture should compile");
        let object = File::parse(bytes.as_slice()).expect("artifact should be an object");
        let directives = object
            .sections()
            .find(|section| section.name().is_ok_and(|name| name == ".drectve"))
            .and_then(|section| section.data().ok())
            .expect("COFF object should contain linker directives");

        assert!(String::from_utf8_lossy(directives).contains("/DEFAULTLIB:\"kernel32.lib\""));
    }

    #[test]
    fn execute_should_run_calls_bindings_and_checked_arithmetic() {
        let program = compile_fixture(
            "fn add(left: i32, right: i32) -> i32 { left + right }
             fn main() -> i32 { let value = add(20, 22); value }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_read_and_mutate_static_aggregate_storage() {
        let program = compile_fixture(
            "struct Pair { left: i32, right: i32 }
             static PAIR: Pair = Pair { left: 20, right: 22 };
             static mut VALUES: [i32; 2] = [0, 0];
             fn main() -> i32 {
                 unsafe {
                     VALUES[1] = PAIR.left + PAIR.right;
                     VALUES[1]
                 }
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    fn execute_should_resolve_symbols_from_a_linked_native_library() {
        #[cfg(target_os = "windows")]
        let source = "@link(\"kernel32\") extern \"C\" {
                fn GetCurrentProcessId() -> u32;
            }
            fn main() -> i32 { unsafe { GetCurrentProcessId() as i32 } }";
        #[cfg(target_os = "linux")]
        let source = "@link(\"libc.so.6\") extern \"C\" {
                fn getpid() -> i32;
            }
            fn main() -> i32 { unsafe { getpid() } }";
        #[cfg(target_os = "macos")]
        let source = "@link(\"/usr/lib/libSystem.B.dylib\") extern \"C\" {
                fn getpid() -> i32;
            }
            fn main() -> i32 { unsafe { getpid() } }";
        let program = compile_fixture(source);

        let result = execute(&program).expect("linked native function should execute");

        assert!(result > 0);
    }

    #[test]
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    fn execute_should_pass_nul_terminated_c_string_literals() {
        #[cfg(target_os = "windows")]
        let library = "msvcrt";
        #[cfg(target_os = "linux")]
        let library = "libc.so.6";
        #[cfg(target_os = "macos")]
        let library = "/usr/lib/libSystem.B.dylib";
        let source = format!(
            r#"@link("{library}") extern "C" {{
                fn strlen(value: cstr) -> usize;
            }}
            fn main() -> i32 {{
                unsafe {{
                    strlen(c"123456789012345678901234567890123456789012") as i32
                }}
            }}"#
        );
        let program = compile_fixture(&source);

        let result = execute(&program).expect("C string call should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_run_while_assignment_and_if_expression() {
        let program = compile_fixture(
            "fn main() -> i32 {
                let mut value = 0;
                while value < 6 { value += 1; }
                if value == 6 { 42 } else { 0 }
            }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_short_circuit_logical_operators() {
        let program = compile_fixture(
            "fn main() -> i32 {
                if false && (1 / 0 == 0) || true { 42 } else { 0 }
            }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_break_continue_and_early_return() {
        let program = compile_fixture(
            "fn choose(limit: i32) -> i32 {
                let mut value = 0;
                while true {
                    value += 1;
                    if value < limit { continue; }
                    break;
                }
                if value == limit { return 42; }
                0
            }
            fn main() -> i32 { choose(3) }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_erase_unit_parameters_from_the_native_abi() {
        let program =
            compile_fixture("fn visit(marker: ()) -> i32 { 42 } fn main() -> i32 { visit(()) }");

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_scalars_casts_and_bitwise_operations() {
        let program = compile_fixture(
            "fn widen(value: u8) -> u64 { value as u64 }
             fn main() -> i32 {
                let byte: u8 = 21;
                let wide: u64 = widen(byte);
                let ratio: f32 = 1.5;
                let scalar: char = 'A';
                if (wide << 1) == 42 && ratio > 1.0 && scalar as u32 == 65 {
                    42
                } else {
                    0
                }
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_all_integer_widths_through_the_native_abi() {
        let program = compile_fixture(
            "fn combine(
                a: i8, b: i16, c: i32, d: i64, e: i128, f: isize,
                g: u8, h: u16, i: u32, j: u64, k: u128, l: usize
             ) -> i32 {
                a as i32 + b as i32 + c + d as i32 + e as i32 + f as i32
                    + g as i32 + h as i32 + i as i32 + j as i32
                    + k as i32 + l as i32
             }
             fn main() -> i32 {
                combine(1, 2, 3, 4, 5, 6, 1, 2, 3, 4, 5, 6)
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_struct_tuple_and_array_values() {
        let program = compile_fixture(
            "struct Pair { left: i32, right: i32 }
             fn make_pair(left: i32, right: i32) -> Pair {
                 Pair { left: left, right: right }
             }
             fn sum(pair: Pair) -> i32 {
                 pair.left + pair.right
             }
             fn main() -> i32 {
                 let pairs: [Pair; 2] = [make_pair(20, 22), make_pair(0, 0)];
                 let selected = pairs[0];
                 let result: (i32, bool) = (sum(selected), true);
                 if result.1 { result.0 } else { 0 }
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_compare_derived_aggregate_values_structurally() {
        let program = compile_fixture(
            "@derive(Copy, Eq)
             struct Pair { left: i32, right: i32 }
             @derive(Copy, Eq)
             enum Value {
                 Empty,
                 Pair(Pair),
                 Numbers([i32; 2]),
             }
             fn main() -> i32 {
                 let left = Value::Pair(Pair { left: 20, right: 22 });
                 let same = Value::Pair(Pair { left: 20, right: 22 });
                 let different = Value::Numbers([20, 22]);
                 if left == same && left != different && [20, 22] == [20, 22] {
                     42
                 } else {
                     0
                 }
             }",
        );

        let result = execute(&program).expect("derived equality fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_monomorphized_generics_and_static_traits() {
        let program = compile_fixture(include_str!("../../../examples/m6_generics.reim"));

        let result = execute(&program).expect("generic fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_all_enum_constructor_forms() {
        let program = compile_fixture(
            "enum Value {
                 Empty,
                 Pair(i32, i32),
                 Named { value: i32 },
             }
             fn make_pair() -> Value { Value::Pair(20, 22) }
             fn main() -> i32 {
                 let empty = Value::Empty;
                 let pair = make_pair();
                 let named = Value::Named { value: 42 };
                 42
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_mutate_struct_fields_and_array_elements() {
        let program = compile_fixture(
            "struct Pair { left: i32, right: i32 }
             fn main() -> i32 {
                 let mut pair = Pair { left: 18, right: 0 };
                 let mut values: [i32; 2] = [11, 0];
                 pair.left += 2;
                 values[0] *= 2;
                 pair.right = values[0];
                 pair.left + pair.right
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_copy_composite_values_before_mutation() {
        let program = compile_fixture(
            "fn main() -> i32 {
                 let original: [i32; 2] = [1, 2];
                 let mut copied = original;
                 copied[1] = 41;
                 original[0] + copied[1]
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_match_loop_for_guards_and_patterns() {
        let program = compile_fixture(
            "enum Value { Empty, Pair(i32, i32), Named { value: i32 } }
             fn main() -> i32 {
                 let values: [i32; 4] = [19, 1, 21, 1];
                 let mut sum = 0;
                 for mut value in values {
                     value += 1;
                     if value == 2 { continue; }
                     sum += value;
                 }
                 let selected = Value::Pair(sum, 0);
                 let result = match selected {
                     Value::Empty => 0,
                     Value::Pair(left, right) if right != 0 => left + right,
                     Value::Pair(left, _) => left,
                     Value::Named { value } => value,
                 };
                 let named = Value::Named { value: result };
                 loop {
                     break match named {
                         Value::Empty => 0,
                         Value::Pair(left, right) => left + right,
                         Value::Named { value } => value,
                     };
                 }
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_references_slices_str_and_raw_pointers() {
        let program = compile_fixture(
            "fn adjust(value: &mut i32) { *value += 2; }
             fn sum(values: &[i32]) -> i32 {
                 let mut total = 0;
                 for value in values { total += value; }
                 total
             }
             fn title_code(title: str) -> i32 { 0 }
             fn main() -> i32 {
                 let mut value = 18;
                 adjust(&mut value);
                 let values: [i32; 2] = [value, 22];
                 let view: &[i32] = &values;
                 let raw: *mut i32 = &mut value as *mut i32;
                 unsafe { *raw -= 2; }
                 sum(view) + title_code(\"Reimer\")
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_option_result_and_try_propagation() {
        let program = compile_fixture(
            "fn maybe(flag: bool) -> Option<i32> {
                 if flag { Some(42) } else { None }
             }
             fn forward(flag: bool) -> Option<i32> {
                 let value = maybe(flag)?;
                 Some(value)
             }
             fn fallible(flag: bool) -> Result<i32, i32> {
                 if flag { Ok(42) } else { Err(7) }
             }
             fn relay(flag: bool) -> Result<i32, i32> {
                 let value = fallible(flag)?;
                 Ok(value)
             }
             fn main() -> i32 {
                 let success = match forward(true) {
                     Some(value) => value,
                     None => 0,
                 };
                 let optional_failure = match forward(false) {
                     Some(_) => 0,
                     None => 1,
                 };
                 let result_failure = match relay(false) {
                     Ok(_) => 0,
                     Err(code) => code,
                 };
                 if success == 42 && optional_failure == 1 && result_failure == 7 {
                     42
                 } else {
                     0
                 }
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_run_defer_on_all_recoverable_scope_exits() {
        let program = compile_fixture(
            "fn record(value: &mut i32, digit: i32) {
                 *value = *value * 10 + digit;
             }
             fn on_return(value: &mut i32) {
                 defer record(value, 1);
                 defer record(value, 2);
                 return;
             }
             fn on_error(value: &mut i32) -> Option<i32> {
                 defer record(value, 5);
                 None?;
                 Some(0)
             }
             fn main() -> i32 {
                 let mut log = 0;
                 on_return(&mut log);
                 {
                     defer record(&mut log, 3);
                     defer { record(&mut log, 4); }
                 }
                 let ignored = on_error(&mut log);
                 loop {
                     defer record(&mut log, 6);
                     break;
                 }
                 let mut once = true;
                 while once {
                     once = false;
                     defer record(&mut log, 7);
                     continue;
                 }
                 if log == 2143567 { 42 } else { 0 }
             }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_lower_non_reached_panic_as_never() {
        let program = compile_fixture(
            "fn choose(flag: bool) -> i32 {
                 if flag { 42 } else { panic(\"invalid state\") }
             }
             fn main() -> i32 { choose(true) }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_call_through_typed_function_values() {
        let program = compile_fixture(
            "fn add(left: i32, right: i32) -> i32 { left + right }
             fn apply(callback: fn(i32, i32) -> i32, left: i32, right: i32) -> i32 {
                 callback(left, right)
             }
             fn main() -> i32 { apply(add, 20, 22) }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }

    #[test]
    fn execute_should_monomorphize_generic_function_callbacks() {
        let program = compile_fixture(
            "fn increment(value: i32) -> i32 { value + 1 }
             fn apply<T: Copy, R: Copy>(callback: fn(T) -> R, value: T) -> R {
                 callback(value)
             }
             fn main() -> i32 { apply(increment, 41) }",
        );

        let result = execute(&program).expect("fixture should execute");

        assert_eq!(result, 42);
    }
}
