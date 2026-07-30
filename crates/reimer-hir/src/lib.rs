//! Typed, name-resolved high-level IR consumed by Reimer backends.

use reimer_diagnostics::Span;
use reimer_types::{Type, TypeId};

/// Index of a function in a [`Program`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

/// Unique local binding within one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// A fully analyzed Reimer compilation unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Composite type definitions indexed by [`TypeId`].
    pub types: Vec<TypeDefinition>,
    /// Functions in declaration order.
    pub functions: Vec<Function>,
    /// Native functions imported through an explicit ABI.
    pub extern_functions: Vec<ExternFunction>,
    /// Validated executable entry point, absent for libraries.
    pub entry: Option<FunctionId>,
    /// Zero-argument functions selected by `@test`.
    pub tests: Vec<FunctionId>,
}

/// A canonical composite type definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDefinition {
    /// Stable index in the compilation unit.
    pub id: TypeId,
    /// Source name for nominal types.
    pub name: Option<String>,
    /// Markdown documentation associated with a named source declaration.
    ///
    /// Compiler-only structural types leave this empty. Editor hosts may
    /// populate it after package resolution, when the original source text is
    /// available.
    pub documentation: Option<String>,
    /// Composite shape.
    pub kind: TypeDefinitionKind,
    /// Externally observable layout policy.
    pub representation: TypeRepresentation,
    /// Requested minimum alignment in bytes.
    pub alignment: Option<u32>,
    /// Compiler-generated structural trait implementations.
    pub derives: Vec<DerivedTrait>,
    /// Whether discarding a value of this type should produce a diagnostic.
    pub must_use: bool,
    /// Source range for named definitions.
    pub span: Span,
}

/// A closed compiler-supported derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivedTrait {
    /// Implicit, bitwise duplication.
    Copy,
    /// Explicit allocation-free duplication.
    Clone,
    /// Developer-facing structural formatting metadata.
    Debug,
    /// Structural equality.
    Eq,
    /// Structural hashing eligibility.
    Hash,
    /// Structural zero/default construction.
    Default,
}

impl DerivedTrait {
    /// Returns the source-level trait spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Clone => "Clone",
            Self::Debug => "Debug",
            Self::Eq => "Eq",
            Self::Hash => "Hash",
            Self::Default => "Default",
        }
    }
}

/// Layout policy attached to a composite type definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRepresentation {
    /// Reimer's native, compiler-controlled representation.
    Native,
    /// Stable field order and alignment compatible with the target C ABI.
    C,
}

/// Shape and members of a composite type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDefinitionKind {
    /// Named product type.
    Struct {
        /// Fields in declaration and layout order.
        fields: Vec<TypeField>,
    },
    /// Named tagged union.
    Enum {
        /// Variants in discriminant order.
        variants: Vec<EnumVariant>,
    },
    /// Structural positional product type.
    Tuple {
        /// Element types in positional order.
        elements: Vec<Type>,
    },
    /// Structural fixed-size sequence type.
    Array {
        /// Element type.
        element: Type,
        /// Number of elements.
        length: u64,
    },
    /// A scoped thin reference.
    Reference {
        /// Referenced value type.
        target: Type,
        /// Whether writes are permitted.
        mutable: bool,
    },
    /// An unchecked thin pointer.
    RawPointer {
        /// Pointee type.
        target: Type,
        /// Whether writes are permitted.
        mutable: bool,
    },
    /// A borrowed fat slice view.
    Slice {
        /// Element type.
        element: Type,
        /// Whether element mutation is permitted.
        mutable: bool,
    },
    /// A thin function pointer signature.
    Function {
        /// Parameter types in source order.
        parameters: Vec<Type>,
        /// Result type.
        return_type: Type,
    },
}

/// A resolved named aggregate field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeField {
    /// Source-level field name.
    pub name: String,
    /// Whether the field is exported from its module.
    pub is_public: bool,
    /// Resolved field type.
    pub ty: Type,
    /// Source range.
    pub span: Span,
}

/// One resolved enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// Source-level variant name.
    pub name: String,
    /// Payload shape.
    pub fields: EnumVariantFields,
    /// Source range.
    pub span: Span,
}

/// Resolved enum variant payload fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumVariantFields {
    /// No payload.
    Unit,
    /// Positional payload.
    Tuple(Vec<Type>),
    /// Named payload.
    Struct(Vec<TypeField>),
}

/// A typed function body and signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Stable identifier in this compilation unit.
    pub id: FunctionId,
    /// Source-level name.
    pub name: String,
    /// Whether the function is exported from its module.
    pub is_public: bool,
    /// Compiler-recognized behavior attributes.
    pub attributes: FunctionAttributes,
    /// Typed parameters.
    pub parameters: Vec<Parameter>,
    /// Declared return type.
    pub return_type: Type,
    /// Resolved function body.
    pub body: Block,
    /// Full source range.
    pub span: Span,
}

/// Validated function attributes used by diagnostics and backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunctionAttributes {
    /// Requests aggressive inlining where the backend can honor it.
    pub inline: bool,
    /// Diagnoses discarded call results.
    pub must_use: bool,
    /// Registers this zero-argument unit function as a test.
    pub test: bool,
}

/// A validated native function declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternFunction {
    /// Stable identifier shared with Reimer functions.
    pub id: FunctionId,
    /// Native symbol name.
    pub name: String,
    /// Linker symbol requested from the native library.
    pub symbol: String,
    /// Native library requested by `@link`, if any.
    pub link: Option<String>,
    /// Whether the declaration is re-exported from its module.
    pub is_public: bool,
    /// Native ABI name.
    pub abi: String,
    /// Parameters in source order.
    pub parameters: Vec<Parameter>,
    /// Native return type.
    pub return_type: Type,
    /// Source range.
    pub span: Span,
}

/// A resolved function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Unique local used by references in the body.
    pub local: LocalId,
    /// Source-level name.
    pub name: String,
    /// Parameter type.
    pub ty: Type,
    /// Source range.
    pub span: Span,
}

/// A lexical block after semantic analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Statements in source order.
    pub statements: Vec<Statement>,
    /// Optional final expression without a semicolon.
    pub tail: Option<Box<Expression>>,
    /// Value type produced by the block.
    pub ty: Type,
    /// Full source range.
    pub span: Span,
}

/// A typed statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// Introduces a local binding.
    Let {
        /// Unique binding identifier.
        local: LocalId,
        /// Source-level name.
        name: String,
        /// Whether later assignment is permitted.
        mutable: bool,
        /// Inferred or declared type.
        ty: Type,
        /// Initial value.
        initializer: Expression,
        /// Full statement range.
        span: Span,
    },
    /// Evaluates and discards an expression.
    Expression(Expression),
    /// Registers an action for lexical scope exit.
    Defer {
        /// Typed action evaluated when the scope exits.
        action: Expression,
        /// Full statement range.
        span: Span,
    },
    /// Returns from the enclosing function.
    Return {
        /// Returned value, absent for unit.
        value: Option<Expression>,
        /// Full statement range.
        span: Span,
    },
    /// Repeats a block while its condition is true.
    While {
        /// Boolean loop condition.
        condition: Expression,
        /// Loop body.
        body: Block,
        /// Full statement range.
        span: Span,
    },
    /// Iterates over every element of a fixed-size array.
    For {
        /// Irrefutable pattern bound for each element.
        pattern: Pattern,
        /// Type produced by each iteration.
        element_type: Type,
        /// Array value evaluated once before iteration.
        iterable: Expression,
        /// Repeated loop body.
        body: Block,
        /// Full statement range.
        span: Span,
    },
    /// Exits the innermost loop.
    Break {
        /// Optional result for an unconditional loop expression.
        value: Option<Expression>,
        /// Full statement range.
        span: Span,
    },
    /// Starts the next iteration of the innermost loop.
    Continue(Span),
}

/// An expression annotated with its semantic type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    /// Resolved expression form.
    pub kind: ExpressionKind,
    /// Static type.
    pub ty: Type,
    /// Source range.
    pub span: Span,
}

/// Runtime synchronization storage selected by a checked intrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynchronizationKind {
    /// Exclusive mutual-exclusion storage.
    Mutex,
    /// Shared-reader, exclusive-writer storage.
    RwLock,
    /// Per-native-thread storage.
    ThreadLocal,
}

/// Overflow behavior selected by an explicit integer addition method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerAdditionMode {
    /// Discards the carry and returns the low bits.
    Wrapping,
    /// Returns `None` when the mathematical result is out of range.
    Checked,
    /// Clamps an out-of-range result to the nearest integer bound.
    Saturating,
}

/// A resolved expression form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionKind {
    /// Integer constant represented as raw bits.
    Integer(u128),
    /// IEEE-754 binary32 constant bits.
    Float32(u32),
    /// IEEE-754 binary64 constant bits.
    Float64(u64),
    /// Unicode scalar constant.
    Character(char),
    /// UTF-8 string literal bytes.
    String(String),
    /// UTF-8 C string literal bytes without their implicit final NUL.
    CString(String),
    /// Boolean constant.
    Boolean(bool),
    /// The unit value.
    Unit,
    /// Positional aggregate construction.
    Tuple(Vec<Expression>),
    /// Fixed-size array construction.
    Array(Vec<Expression>),
    /// Named struct construction in declaration field order.
    Struct(Vec<Expression>),
    /// Enum variant construction.
    Enum {
        /// Zero-based discriminant.
        variant: u32,
        /// Payload values in declaration field order.
        fields: Vec<Expression>,
    },
    /// Read from a resolved local.
    Local(LocalId),
    /// Prefix operation.
    Unary {
        /// Operation to apply.
        operator: UnaryOperator,
        /// Typed operand.
        operand: Box<Expression>,
    },
    /// Infix operation.
    Binary {
        /// Operation to apply.
        operator: BinaryOperator,
        /// Left operand.
        left: Box<Expression>,
        /// Right operand.
        right: Box<Expression>,
    },
    /// Integer addition with source-selected overflow behavior.
    IntegerAddition {
        /// Whether the result wraps, becomes optional, or saturates.
        mode: IntegerAdditionMode,
        /// Left integer operand.
        left: Box<Expression>,
        /// Right integer operand.
        right: Box<Expression>,
    },
    /// Direct call to a resolved function.
    Call {
        /// Target function.
        function: FunctionId,
        /// Arguments in source order.
        arguments: Vec<Expression>,
    },
    /// Address of a resolved function.
    Function(FunctionId),
    /// Call through a statically typed function pointer.
    IndirectCall {
        /// Runtime function address.
        callee: Box<Expression>,
        /// Arguments in source order.
        arguments: Vec<Expression>,
    },
    /// Conditional expression.
    If(Box<IfExpression>),
    /// Pattern-based branch selection.
    Match(Box<MatchExpression>),
    /// Unconditional repetition with an optional break result.
    Loop(Box<LoopExpression>),
    /// Nested lexical block.
    Block(Box<Block>),
    /// Assignment to a resolved local.
    Assign {
        /// Resolved assignable place.
        target: Place,
        /// Assignment form.
        operator: AssignmentOperator,
        /// New or combined value.
        value: Box<Expression>,
    },
    /// Explicit scalar conversion.
    Cast {
        /// Value being converted.
        value: Box<Expression>,
        /// Destination scalar type.
        target: Type,
    },
    /// Creates a scoped reference or slice view.
    Borrow {
        /// Addressable source value.
        place: Place,
        /// Whether mutation through the borrow is permitted.
        mutable: bool,
        /// Fixed source length when coercing an array to a slice.
        slice_length: Option<u64>,
    },
    /// Reads through a reference or raw pointer.
    Dereference(Box<Expression>),
    /// Aborts execution with a non-recoverable failure.
    Panic {
        /// UTF-8 diagnostic message.
        message: Box<Expression>,
    },
    /// Extracts the data pointer from a bounded UTF-8 string view.
    StringData(Box<Expression>),
    /// Extracts the byte length from a bounded UTF-8 string view.
    StringLength(Box<Expression>),
    /// Extracts the element count from a bounded slice view.
    SliceLength(Box<Expression>),
    /// Builds a bounded UTF-8 string view from validated raw parts.
    StringFromParts {
        /// First byte of the live UTF-8 region.
        data: Box<Expression>,
        /// Number of readable bytes in the region.
        length: Box<Expression>,
    },
    /// Builds a bounded slice view from validated raw parts.
    SliceFromParts {
        /// Pointer to the first live element.
        data: Box<Expression>,
        /// Number of live elements.
        length: Box<Expression>,
    },
    /// Returns the native byte stride of a raw pointer's target type.
    TypeStride {
        /// Concrete pointee type after generic monomorphization.
        target: Type,
    },
    /// Allocates an owned byte region through the runtime allocator ABI.
    AllocateBytes {
        /// Opaque allocator handle.
        allocator: Box<Expression>,
        /// Requested byte length.
        length: Box<Expression>,
        /// Struct stored in the success variant.
        allocation_type: Type,
        /// Enum stored in the error variant.
        error_type: Type,
        /// `OutOfMemory` discriminant in the error enum.
        error_variant: u32,
    },
    /// Releases an owned byte region through the runtime allocator ABI.
    DeallocateBytes {
        /// Opaque allocator handle.
        allocator: Box<Expression>,
        /// Allocation base address.
        data: Box<Expression>,
        /// Allocation byte length.
        length: Box<Expression>,
    },
    /// Starts one native thread with a moved argument.
    ThreadSpawn {
        /// Typed function pointer executed by the worker.
        callback: Box<Expression>,
        /// Value transferred into the worker.
        argument: Box<Expression>,
        /// Callback result type retained until join.
        output_type: Type,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `SpawnFailed` discriminant in the error enum.
        spawn_failed_variant: u32,
    },
    /// Joins one native thread and transfers its result to the caller.
    ThreadJoin {
        /// Opaque runtime thread handle.
        handle: Box<Expression>,
        /// Value returned by the worker.
        output_type: Type,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `InvalidHandle` discriminant.
        invalid_handle_variant: u32,
        /// `WorkerPanicked` discriminant.
        worker_panicked_variant: u32,
        /// `ResultMismatch` discriminant.
        result_mismatch_variant: u32,
    },
    /// Runs a native thread and guarantees its join before returning.
    ThreadScope {
        /// Typed function pointer executed by the scoped worker.
        callback: Box<Expression>,
        /// Locally borrowed or owned argument.
        argument: Box<Expression>,
        /// Callback result type.
        output_type: Type,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `SpawnFailed` discriminant.
        spawn_failed_variant: u32,
        /// `InvalidHandle` discriminant.
        invalid_handle_variant: u32,
        /// `WorkerPanicked` discriminant.
        worker_panicked_variant: u32,
        /// `ResultMismatch` discriminant.
        result_mismatch_variant: u32,
    },
    /// Moves one value into an opaque synchronization resource.
    SynchronizationCreate {
        /// Initial resource value.
        value: Box<Expression>,
        /// Runtime resource implementation.
        synchronization: SynchronizationKind,
    },
    /// Copies one protected value into the current function.
    SynchronizationLoad {
        /// Opaque runtime resource handle.
        handle: Box<Expression>,
        /// Concrete value type copied from the resource.
        value_type: Type,
        /// Runtime resource implementation.
        synchronization: SynchronizationKind,
    },
    /// Replaces a mutex- or reader-writer-lock-protected value.
    SynchronizationReplace {
        /// Opaque runtime resource handle.
        handle: Box<Expression>,
        /// Replacement value moved into the resource.
        value: Box<Expression>,
        /// Runtime resource implementation.
        synchronization: SynchronizationKind,
    },
    /// Replaces the current native thread's local value.
    ThreadLocalStore {
        /// Opaque thread-local storage handle.
        handle: Box<Expression>,
        /// Replacement value for the current thread.
        value: Box<Expression>,
    },
    /// Creates a bounded, typed, multi-producer, multi-consumer channel.
    ChannelCreate {
        /// Compile-time type probe; its pointer value is never dereferenced.
        probe: Box<Expression>,
        /// Maximum number of queued values.
        capacity: Box<Expression>,
        /// Concrete channel element type.
        element_type: Type,
    },
    /// Moves one value through a bounded channel.
    ChannelSend {
        /// Opaque channel endpoint handle.
        handle: Box<Expression>,
        /// Value transferred into the channel.
        value: Box<Expression>,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `Closed` discriminant in the error enum.
        closed_variant: u32,
    },
    /// Receives one value from a bounded channel.
    ChannelReceive {
        /// Opaque channel endpoint handle.
        handle: Box<Expression>,
        /// Concrete received value type.
        value_type: Type,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `Closed` discriminant in the error enum.
        closed_variant: u32,
    },
    /// Submits one callback to a fixed-worker job pool.
    JobSubmit {
        /// Opaque runtime pool handle.
        pool: Box<Expression>,
        /// Typed function pointer executed by a worker.
        callback: Box<Expression>,
        /// Value transferred into the job.
        argument: Box<Expression>,
        /// Callback result type retained until wait.
        output_type: Type,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `SubmitFailed` discriminant in the error enum.
        submit_failed_variant: u32,
    },
    /// Waits for one job and transfers its result to the caller.
    JobWait {
        /// Opaque runtime job handle.
        handle: Box<Expression>,
        /// Value returned by the job.
        output_type: Type,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `InvalidHandle` discriminant.
        invalid_handle_variant: u32,
        /// `WorkerPanicked` discriminant.
        worker_panicked_variant: u32,
        /// `ResultMismatch` discriminant.
        result_mismatch_variant: u32,
    },
    /// Applies a callback to nonoverlapping chunks of one mutable slice.
    ParallelFor {
        /// Opaque fixed-worker pool handle.
        pool: Box<Expression>,
        /// Exclusively borrowed mutable slice.
        slice: Box<Expression>,
        /// Mutable slice type passed to each callback.
        chunk_type: Type,
        /// Fixed input length when the source is a borrowed array.
        array_length: Option<u64>,
        /// Callback accepting one mutable slice chunk.
        callback: Box<Expression>,
        /// Smallest preferred number of elements per job.
        minimum_chunk: Box<Expression>,
        /// Error enum stored in the failure result variant.
        error_type: Type,
        /// `SubmitFailed` discriminant.
        submit_failed_variant: u32,
        /// `WorkerPanicked` discriminant.
        worker_panicked_variant: u32,
        /// `ResultMismatch` discriminant.
        result_mismatch_variant: u32,
    },
    /// Extracts a success payload or returns the original failure.
    Try {
        /// Intrinsic enum value.
        value: Box<Expression>,
        /// Success variant discriminant.
        success_variant: u32,
        /// Payload field type.
        output_type: Type,
        /// Failure variant discriminant.
        failure_variant: u32,
        /// Error payload type, absent for payload-free failures such as `None`.
        failure_type: Option<Type>,
        /// Enclosing function result type receiving a propagated failure.
        return_type: Type,
    },
    /// Read one canonical aggregate field.
    Field {
        /// Aggregate value.
        base: Box<Expression>,
        /// Zero-based field position.
        field: u32,
    },
    /// Bounds-checked array access.
    Index {
        /// Array value.
        base: Box<Expression>,
        /// Element index.
        index: Box<Expression>,
    },
}

/// A typed assignable location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// Location form.
    pub kind: PlaceKind,
    /// Stored value type.
    pub ty: Type,
    /// Source range.
    pub span: Span,
}

/// A resolved assignable location form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceKind {
    /// A local binding.
    Local(LocalId),
    /// A field within an aggregate value.
    Field {
        /// Aggregate base.
        base: Box<Expression>,
        /// Zero-based field position.
        field: u32,
    },
    /// An array element.
    Index {
        /// Array base.
        base: Box<Expression>,
        /// Checked index.
        index: Box<Expression>,
    },
    /// A location reached through a pointer-like value.
    Dereference {
        /// Reference or raw pointer expression.
        pointer: Box<Expression>,
    },
}

/// A typed conditional expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpression {
    /// Boolean condition.
    pub condition: Expression,
    /// Branch used when the condition is true.
    pub then_branch: Block,
    /// Optional branch used when the condition is false.
    pub else_branch: Option<Box<Expression>>,
}

/// A typed pattern-based branch expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchExpression {
    /// Value inspected exactly once.
    pub scrutinee: Expression,
    /// Resolved arms in source order.
    pub arms: Vec<MatchArm>,
}

/// One resolved match arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    /// Typed pattern and local bindings.
    pub pattern: Pattern,
    /// Optional boolean guard.
    pub guard: Option<Expression>,
    /// Result expression.
    pub body: Expression,
    /// Full arm range.
    pub span: Span,
}

/// An unconditional loop expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopExpression {
    /// Repeated body.
    pub body: Block,
}

/// A typed and resolved pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// Resolved pattern form.
    pub kind: PatternKind,
    /// Type inspected by this pattern.
    pub ty: Type,
    /// Full source range.
    pub span: Span,
}

/// A resolved pattern form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternKind {
    /// Matches every value without a local.
    Wildcard,
    /// Matches every value and introduces a local.
    Binding {
        /// Unique local identifier.
        local: LocalId,
        /// Source-level binding name.
        name: String,
        /// Whether assignment is permitted.
        mutable: bool,
    },
    /// Integer bit pattern.
    Integer(u128),
    /// IEEE-754 binary32 bits.
    Float32(u32),
    /// IEEE-754 binary64 bits.
    Float64(u64),
    /// Unicode scalar.
    Character(char),
    /// Boolean value.
    Boolean(bool),
    /// Positional product fields.
    Tuple(Vec<Pattern>),
    /// Enum discriminant and positional payload fields.
    Enum {
        /// Zero-based discriminant.
        variant: u32,
        /// Payload patterns in declaration order.
        fields: Vec<Pattern>,
    },
}

/// A supported prefix operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    /// Arithmetic negation.
    Negate,
    /// Logical negation.
    Not,
}

/// A supported infix operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    /// Checked integer addition.
    Add,
    /// Checked integer subtraction.
    Subtract,
    /// Checked integer multiplication.
    Multiply,
    /// Checked signed integer division.
    Divide,
    /// Checked signed integer remainder.
    Remainder,
    /// Bitwise conjunction.
    BitAnd,
    /// Bitwise exclusive disjunction.
    BitXor,
    /// Bitwise disjunction.
    BitOr,
    /// Checked left shift.
    ShiftLeft,
    /// Checked right shift.
    ShiftRight,
    /// Equality comparison.
    Equal,
    /// Inequality comparison.
    NotEqual,
    /// Signed less-than comparison.
    Less,
    /// Signed less-than-or-equal comparison.
    LessEqual,
    /// Signed greater-than comparison.
    Greater,
    /// Signed greater-than-or-equal comparison.
    GreaterEqual,
    /// Short-circuit logical conjunction.
    And,
    /// Short-circuit logical disjunction.
    Or,
}

/// A supported assignment operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentOperator {
    /// Replace the current value.
    Assign,
    /// Checked addition and replacement.
    Add,
    /// Checked subtraction and replacement.
    Subtract,
    /// Checked multiplication and replacement.
    Multiply,
    /// Checked division and replacement.
    Divide,
    /// Checked remainder and replacement.
    Remainder,
    /// Bitwise conjunction and replacement.
    BitAnd,
    /// Bitwise exclusive disjunction and replacement.
    BitXor,
    /// Bitwise disjunction and replacement.
    BitOr,
    /// Checked left shift and replacement.
    ShiftLeft,
    /// Checked right shift and replacement.
    ShiftRight,
}
