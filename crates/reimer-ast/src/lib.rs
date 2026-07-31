//! Syntax-oriented data structures produced by the Reimer parser.

use reimer_diagnostics::Span;

/// A complete Reimer source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Top-level declarations in source order.
    pub items: Vec<Item>,
}

/// A compiler-recognized annotation attached to a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// Attribute name without the leading `@`.
    pub name: Identifier,
    /// Arguments written between parentheses.
    pub arguments: Vec<AttributeArgument>,
    /// Full attribute span.
    pub span: Span,
}

/// One syntactic attribute argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeArgument {
    /// An identifier such as `C` or `Copy`.
    Identifier(Identifier),
    /// A non-negative integer literal.
    Integer(IntegerLiteral),
    /// A decoded UTF-8 string literal.
    String(StringLiteral),
}

impl AttributeArgument {
    /// Returns the complete source range for this argument.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Identifier(identifier) => identifier.span,
            Self::Integer(literal) => literal.span,
            Self::String(literal) => literal.span,
        }
    }
}

/// A top-level declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A module or selective import declaration.
    Import(ImportDeclaration),
    /// A free function declaration.
    Function(Function),
    /// A function imported through a native ABI.
    ExternFunction(ExternFunction),
    /// A named product type.
    Struct(StructDeclaration),
    /// A tagged union type.
    Enum(EnumDeclaration),
    /// A transparent name for another type.
    TypeAlias(TypeAliasDeclaration),
    /// A trait declaration.
    Trait(TraitDeclaration),
    /// An inherent or trait implementation block.
    Impl(ImplDeclaration),
    /// A named compile-time constant.
    Constant(ConstantDeclaration),
    /// A named value with a stable runtime address.
    Static(StaticDeclaration),
    /// A block executed by the compiler after type definitions are available.
    Comptime(ComptimeBlock),
}

/// A top-level import or re-export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDeclaration {
    /// Whether the imported names become part of the module's public API.
    pub is_public: bool,
    /// Import form and imported names.
    pub kind: ImportKind,
    /// Full declaration span.
    pub span: Span,
}

/// The two supported import forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    /// `import x::y [as alias];`
    Module {
        /// Imported module path.
        path: Path,
        /// Optional local name. Without one, the final path segment is bound.
        alias: Option<Identifier>,
    },
    /// `from x::y import z [as alias], ...;`
    Symbols {
        /// Module containing the imported symbols.
        module: Path,
        /// Names introduced into the current module.
        names: Vec<ImportedName>,
    },
}

/// A `::`-separated module, type, or symbol path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    /// Path segments in source order.
    pub segments: Vec<Identifier>,
    /// Full path span.
    pub span: Span,
}

impl Path {
    /// Formats the path with Reimer's canonical separator.
    #[must_use]
    pub fn display(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }
}

/// One name in a selective import list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedName {
    /// Name exported by the source module.
    pub name: Identifier,
    /// Optional local alias.
    pub alias: Option<Identifier>,
}

/// A free function declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Compiler-recognized declaration attributes.
    pub attributes: Vec<Attribute>,
    /// Whether the function is evaluated only by the compiler.
    pub is_comptime: bool,
    /// Whether the function is exported from the module.
    pub is_public: bool,
    /// Function name.
    pub name: Identifier,
    /// Generic type and constant parameters.
    pub generic_parameters: Vec<GenericParameter>,
    /// Parameters in source order.
    pub parameters: Vec<Parameter>,
    /// Explicit return type. Its absence means `()`.
    pub return_type: Option<TypeName>,
    /// Additional constraints on generic parameters.
    pub where_predicates: Vec<WherePredicate>,
    /// Function body.
    pub body: Block,
    /// Full declaration span.
    pub span: Span,
}

/// A native function declaration without a Reimer body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternFunction {
    /// Whether the native symbol is re-exported from the module.
    pub is_public: bool,
    /// ABI name. v0.1 accepts only `C`.
    pub abi: String,
    /// Native symbol name.
    pub name: Identifier,
    /// Linker symbol before module canonicalization.
    pub symbol: String,
    /// Native library requested by the enclosing `@link` attribute.
    pub link: Option<String>,
    /// Parameters in source order.
    pub parameters: Vec<Parameter>,
    /// Explicit return type. Its absence means `()`.
    pub return_type: Option<TypeName>,
    /// Full declaration span.
    pub span: Span,
}

/// A named product type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDeclaration {
    /// Compiler-recognized declaration attributes.
    pub attributes: Vec<Attribute>,
    /// Whether the type is exported from its module.
    pub is_public: bool,
    /// Type name.
    pub name: Identifier,
    /// Generic type and constant parameters.
    pub generic_parameters: Vec<GenericParameter>,
    /// Fields in declaration order.
    pub fields: Vec<StructField>,
    /// Additional constraints on generic parameters.
    pub where_predicates: Vec<WherePredicate>,
    /// Full declaration span.
    pub span: Span,
}

/// One named field in a struct or struct-like enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructField {
    /// Whether the field is visible outside its module.
    pub is_public: bool,
    /// Field name.
    pub name: Identifier,
    /// Field type.
    pub ty: TypeName,
    /// Full field span.
    pub span: Span,
}

/// Methods associated with a nominal type, optionally implementing a trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplDeclaration {
    /// Generic type and constant parameters scoped to the implementation.
    pub generic_parameters: Vec<GenericParameter>,
    /// Implemented trait. Its absence denotes an inherent implementation.
    pub trait_type: Option<TypeName>,
    /// Type receiving the methods.
    pub target: TypeName,
    /// Additional constraints on generic parameters.
    pub where_predicates: Vec<WherePredicate>,
    /// Methods in source order.
    pub methods: Vec<Function>,
    /// Full declaration span.
    pub span: Span,
}

/// A tagged union declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDeclaration {
    /// Compiler-recognized declaration attributes.
    pub attributes: Vec<Attribute>,
    /// Whether the type is exported from its module.
    pub is_public: bool,
    /// Type name.
    pub name: Identifier,
    /// Generic type and constant parameters.
    pub generic_parameters: Vec<GenericParameter>,
    /// Variants in discriminant order.
    pub variants: Vec<EnumVariant>,
    /// Additional constraints on generic parameters.
    pub where_predicates: Vec<WherePredicate>,
    /// Full declaration span.
    pub span: Span,
}

/// A transparent, non-generic type alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAliasDeclaration {
    /// Whether the alias is exported from its module.
    pub is_public: bool,
    /// Alias name.
    pub name: Identifier,
    /// Existing type named by the alias.
    pub target: TypeName,
    /// Full declaration span.
    pub span: Span,
}

/// A named value evaluated by the compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantDeclaration {
    /// Whether the constant is exported from the module.
    pub is_public: bool,
    /// Constant name.
    pub name: Identifier,
    /// Declared value type.
    pub ty: TypeName,
    /// Compile-time initializer.
    pub value: Expression,
    /// Full declaration span.
    pub span: Span,
}

/// A named value stored at a stable runtime address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDeclaration {
    /// Whether the static is exported from the module.
    pub is_public: bool,
    /// Whether accesses may mutate the stored value.
    pub mutable: bool,
    /// Static name.
    pub name: Identifier,
    /// Declared value type.
    pub ty: TypeName,
    /// Compile-time initializer.
    pub value: Expression,
    /// Full declaration span.
    pub span: Span,
}

/// An unnamed block executed during compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComptimeBlock {
    /// Statements evaluated by the compiler.
    pub body: Block,
    /// Full declaration span.
    pub span: Span,
}

/// A trait declaration used as a generic bound and static dispatch contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitDeclaration {
    /// Whether the trait is exported from its module.
    pub is_public: bool,
    /// Trait name.
    pub name: Identifier,
    /// Generic type and constant parameters.
    pub generic_parameters: Vec<GenericParameter>,
    /// Supertraits required by every implementation.
    pub supertraits: Vec<Path>,
    /// Additional constraints on generic parameters.
    pub where_predicates: Vec<WherePredicate>,
    /// Required methods in source order.
    pub methods: Vec<TraitMethod>,
    /// Full declaration span.
    pub span: Span,
}

/// One required method in a trait declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitMethod {
    /// Method name.
    pub name: Identifier,
    /// Method-local generic parameters.
    pub generic_parameters: Vec<GenericParameter>,
    /// Parameters in source order.
    pub parameters: Vec<Parameter>,
    /// Explicit return type. Its absence means `()`.
    pub return_type: Option<TypeName>,
    /// Additional constraints on generic parameters.
    pub where_predicates: Vec<WherePredicate>,
    /// Full declaration span.
    pub span: Span,
}

/// A generic parameter declared by a type, function, trait, or implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericParameter {
    /// A type parameter with optional trait bounds and default.
    Type {
        /// Parameter name.
        name: Identifier,
        /// Required traits.
        bounds: Vec<Path>,
        /// Default type argument.
        default: Option<TypeName>,
        /// Full parameter span.
        span: Span,
    },
    /// A compile-time integer or boolean parameter.
    Const {
        /// Parameter name.
        name: Identifier,
        /// Declared scalar type.
        ty: TypeName,
        /// Default constant argument.
        default: Option<Expression>,
        /// Full parameter span.
        span: Span,
    },
}

impl GenericParameter {
    /// Returns the declared parameter name.
    #[must_use]
    pub fn name(&self) -> &Identifier {
        match self {
            Self::Type { name, .. } | Self::Const { name, .. } => name,
        }
    }

    /// Returns the full source span.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Type { span, .. } | Self::Const { span, .. } => *span,
        }
    }
}

/// A `where Type: Trait + OtherTrait` constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WherePredicate {
    /// Constrained type.
    pub ty: TypeName,
    /// Required traits.
    pub bounds: Vec<Path>,
    /// Full predicate span.
    pub span: Span,
}

/// One enum variant declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// Variant name.
    pub name: Identifier,
    /// Optional payload form.
    pub payload: EnumVariantPayload,
    /// Full variant span.
    pub span: Span,
}

/// Payload shape of an enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumVariantPayload {
    /// A variant with no payload.
    Unit,
    /// Positional payload fields.
    Tuple(Vec<TypeName>),
    /// Named payload fields.
    Struct(Vec<StructField>),
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Parameter name.
    pub name: Identifier,
    /// Declared parameter type.
    pub ty: TypeName,
    /// Full parameter span.
    pub span: Span,
}

/// An identifier and its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    /// Identifier spelling.
    pub name: String,
    /// Source location.
    pub span: Span,
}

/// A type path and its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeName {
    /// Written type form.
    pub kind: TypeNameKind,
    /// Source location.
    pub span: Span,
}

/// A syntactic type form supported by the current parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeNameKind {
    /// A thin pointer to a function with a statically known signature.
    Function {
        /// Parameter types in source order.
        parameters: Vec<TypeName>,
        /// Result type.
        return_type: Box<TypeName>,
    },
    /// A primitive or user-defined type path.
    Path(Path),
    /// A type path applied to type arguments.
    Generic {
        /// Generic type constructor path.
        path: Path,
        /// Type and constant arguments in source order.
        arguments: Vec<GenericArgument>,
    },
    /// The unit type `()`.
    Unit,
    /// A tuple type `(A, B, ...)`.
    Tuple(Vec<TypeName>),
    /// A fixed-size array type `[T; N]`.
    Array {
        /// Element type.
        element: Box<TypeName>,
        /// Compile-time length expression.
        length: Box<Expression>,
    },
    /// A slice type `[T]`.
    Slice(Box<TypeName>),
    /// A scoped reference type.
    Reference {
        /// Whether the reference permits mutation.
        mutable: bool,
        /// Referenced type.
        target: Box<TypeName>,
    },
    /// A raw pointer type.
    RawPointer {
        /// Whether writes through the pointer are permitted.
        mutable: bool,
        /// Pointee type.
        target: Box<TypeName>,
    },
}

/// One argument supplied to a generic type constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericArgument {
    /// A type argument.
    Type(TypeName),
    /// A compile-time value argument.
    Const(Expression),
}

impl GenericArgument {
    /// Returns the full source span.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Type(ty) => ty.span,
            Self::Const(expression) => expression.span(),
        }
    }
}

/// A braced statement block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Statements in source order.
    pub statements: Vec<Statement>,
    /// Optional value expression without a trailing semicolon.
    pub tail: Option<Box<Expression>>,
    /// Full block span, including braces.
    pub span: Span,
}

/// A statement in a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// A local binding declaration.
    Let(LetStatement),
    /// An expression followed by a semicolon.
    Expression(ExpressionStatement),
    /// Registers an action to run when the current scope exits.
    Defer(DeferStatement),
    /// An explicit return statement.
    Return(ReturnStatement),
    /// A conditional loop.
    While(WhileStatement),
    /// Iterates over a value using a pattern binding.
    For(ForStatement),
    /// Exit from the innermost loop.
    Break(BreakStatement),
    /// Continue the innermost loop.
    Continue(Span),
}

/// A local binding declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetStatement {
    /// Whether later assignment is permitted.
    pub mutable: bool,
    /// Binding name.
    pub name: Identifier,
    /// Optional declared type.
    pub ty: Option<TypeName>,
    /// Initial value.
    pub initializer: Expression,
    /// Full statement span.
    pub span: Span,
}

/// An expression whose value is discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionStatement {
    /// Evaluated expression.
    pub expression: Expression,
    /// Full statement span.
    pub span: Span,
}

/// An action registered for lexical scope exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferStatement {
    /// Expression or block evaluated at scope exit.
    pub action: Expression,
    /// Full statement span.
    pub span: Span,
}

/// An explicit function return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnStatement {
    /// Returned expression. Its absence returns `()`.
    pub value: Option<Expression>,
    /// Full statement span.
    pub span: Span,
}

/// A `while` loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhileStatement {
    /// Loop condition.
    pub condition: Expression,
    /// Repeated body.
    pub body: Block,
    /// Full statement span.
    pub span: Span,
}

/// A `for pattern in iterable` loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForStatement {
    /// Pattern bound for each element.
    pub pattern: Pattern,
    /// Value being iterated.
    pub iterable: Expression,
    /// Repeated body.
    pub body: Block,
    /// Full statement span.
    pub span: Span,
}

/// An exit from the innermost loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakStatement {
    /// Optional value produced by a `loop` expression.
    pub value: Option<Expression>,
    /// Full statement span.
    pub span: Span,
}

/// A Reimer expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expression {
    /// A base-10 integer literal.
    Integer(IntegerLiteral),
    /// A decimal floating-point literal.
    Float(FloatLiteral),
    /// A Unicode scalar literal.
    Character(CharacterLiteral),
    /// An immutable UTF-8 string literal.
    String(StringLiteral),
    /// An interpolated UTF-8 string literal.
    FormattedString(FormattedStringExpression),
    /// An immutable NUL-terminated UTF-8 C string literal.
    CString(StringLiteral),
    /// A boolean literal.
    Boolean(BooleanLiteral),
    /// The unit literal `()`.
    Unit(Span),
    /// A tuple literal.
    Tuple(TupleExpression),
    /// An array literal.
    Array(ArrayExpression),
    /// A named-field aggregate literal.
    Struct(StructExpression),
    /// A local, function, module, or associated path.
    Path(Path),
    /// A prefix operation.
    Unary(Box<UnaryExpression>),
    /// An infix operation.
    Binary(Box<BinaryExpression>),
    /// A function call.
    Call(Box<CallExpression>),
    /// A conditional expression.
    If(Box<IfExpression>),
    /// Pattern-based branch selection.
    Match(Box<MatchExpression>),
    /// An unconditional loop expression.
    Loop(Box<LoopExpression>),
    /// A block permitting explicit unsafe operations.
    Unsafe(Box<Block>),
    /// A nested block expression.
    Block(Box<Block>),
    /// An assignment expression.
    Assignment(Box<AssignmentExpression>),
    /// An explicit scalar conversion.
    Cast(Box<CastExpression>),
    /// Field or tuple-element access.
    Field(Box<FieldExpression>),
    /// Checked indexing.
    Index(Box<IndexExpression>),
    /// Propagates an `Option`/`Result` failure.
    Try {
        /// Value being unwrapped.
        value: Box<Expression>,
        /// Full postfix expression span.
        span: Span,
    },
}

impl Expression {
    /// Returns the complete source range for this expression.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Integer(literal) => literal.span,
            Self::Float(literal) => literal.span,
            Self::Character(literal) => literal.span,
            Self::String(literal) | Self::CString(literal) => literal.span,
            Self::FormattedString(expression) => expression.span,
            Self::Boolean(literal) => literal.span,
            Self::Unit(span) | Self::Try { span, .. } => *span,
            Self::Tuple(expression) => expression.span,
            Self::Array(expression) => expression.span,
            Self::Struct(expression) => expression.span,
            Self::Path(path) => path.span,
            Self::Unary(expression) => expression.span,
            Self::Binary(expression) => expression.span,
            Self::Call(expression) => expression.span,
            Self::If(expression) => expression.span,
            Self::Match(expression) => expression.span,
            Self::Loop(expression) => expression.span,
            Self::Unsafe(block) | Self::Block(block) => block.span,
            Self::Assignment(expression) => expression.span,
            Self::Cast(expression) => expression.span,
            Self::Field(expression) => expression.span,
            Self::Index(expression) => expression.span,
        }
    }
}

/// A sequence of literal text and typed interpolation expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedStringExpression {
    /// Fragments in source order.
    pub fragments: Vec<FormattedStringFragment>,
    /// Complete `f"..."` source range.
    pub span: Span,
}

/// One fragment of an interpolated string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormattedStringFragment {
    /// Decoded literal UTF-8.
    Text(StringLiteral),
    /// An expression written between braces.
    Expression(Expression),
}

/// A pattern-based branch expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchExpression {
    /// Value inspected by the patterns.
    pub scrutinee: Expression,
    /// Arms in source order.
    pub arms: Vec<MatchArm>,
    /// Full expression span.
    pub span: Span,
}

/// One pattern, optional guard, and result expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    /// Pattern tested by this arm.
    pub pattern: Pattern,
    /// Additional boolean condition.
    pub guard: Option<Expression>,
    /// Value produced when the arm is selected.
    pub body: Expression,
    /// Full arm span.
    pub span: Span,
}

/// An unconditional `loop` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopExpression {
    /// Repeated body.
    pub body: Block,
    /// Full expression span.
    pub span: Span,
}

/// A syntactic pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Matches any value without binding it.
    Wildcard(Span),
    /// A potentially mutable local binding or an unqualified unit variant.
    Identifier {
        /// Whether the resulting binding is mutable.
        mutable: bool,
        /// Written identifier.
        name: Identifier,
        /// Full pattern span.
        span: Span,
    },
    /// A non-negative or negative integer literal.
    Integer {
        /// Literal magnitude.
        value: u128,
        /// Whether a leading minus was written.
        negative: bool,
        /// Full pattern span.
        span: Span,
    },
    /// A floating-point literal.
    Float {
        /// IEEE-754 binary64 source representation.
        bits: u64,
        /// Whether a leading minus was written.
        negative: bool,
        /// Full pattern span.
        span: Span,
    },
    /// A Unicode scalar literal.
    Character(CharacterLiteral),
    /// A boolean literal.
    Boolean(BooleanLiteral),
    /// A tuple pattern.
    Tuple {
        /// Element patterns.
        elements: Vec<Pattern>,
        /// Full pattern span.
        span: Span,
    },
    /// A qualified unit enum variant.
    Path(Path),
    /// A positional enum variant pattern.
    EnumTuple {
        /// Qualified or unqualified variant path.
        path: Path,
        /// Payload patterns.
        fields: Vec<Pattern>,
        /// Full pattern span.
        span: Span,
    },
    /// A named-field enum variant pattern.
    EnumStruct {
        /// Qualified or unqualified variant path.
        path: Path,
        /// Payload field patterns.
        fields: Vec<PatternField>,
        /// Full pattern span.
        span: Span,
    },
}

/// One named field in a struct-like enum pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternField {
    /// Declared field name.
    pub name: Identifier,
    /// Pattern applied to the field value.
    pub pattern: Pattern,
    /// Full field pattern span.
    pub span: Span,
}

impl Pattern {
    /// Returns the complete source range for this pattern.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Wildcard(span)
            | Self::Identifier { span, .. }
            | Self::Integer { span, .. }
            | Self::Float { span, .. }
            | Self::Tuple { span, .. }
            | Self::EnumTuple { span, .. }
            | Self::EnumStruct { span, .. } => *span,
            Self::Character(literal) => literal.span,
            Self::Boolean(literal) => literal.span,
            Self::Path(path) => path.span,
        }
    }
}

/// A parsed integer literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerLiteral {
    /// Numeric value.
    pub value: u128,
    /// Source location.
    pub span: Span,
}

/// A parsed floating-point literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatLiteral {
    /// IEEE-754 binary64 representation of the parsed value.
    pub bits: u64,
    /// Source location.
    pub span: Span,
}

impl FloatLiteral {
    /// Returns the literal as an `f64`.
    #[must_use]
    pub const fn value(self) -> f64 {
        f64::from_bits(self.bits)
    }
}

/// A parsed character literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterLiteral {
    /// Decoded Unicode scalar.
    pub value: char,
    /// Source location.
    pub span: Span,
}

/// A parsed UTF-8 string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringLiteral {
    /// Decoded Unicode contents.
    pub value: String,
    /// Source location.
    pub span: Span,
}

/// A parsed boolean literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BooleanLiteral {
    /// Logical value.
    pub value: bool,
    /// Source location.
    pub span: Span,
}

/// A tuple literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TupleExpression {
    /// Elements in positional order.
    pub elements: Vec<Expression>,
    /// Full source location.
    pub span: Span,
}

/// An array literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayExpression {
    /// Elements in index order.
    pub elements: Vec<Expression>,
    /// Full source location.
    pub span: Span,
}

/// A struct or struct-like enum variant literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructExpression {
    /// Constructed type or variant path.
    pub path: Path,
    /// Explicit field initializers.
    pub fields: Vec<FieldInitializer>,
    /// Full source location.
    pub span: Span,
}

/// One named aggregate field initializer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInitializer {
    /// Field name.
    pub name: Identifier,
    /// Field value.
    pub value: Expression,
    /// Full source location.
    pub span: Span,
}

/// A prefix operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaryExpression {
    /// Operation to apply.
    pub operator: UnaryOperator,
    /// Operand.
    pub operand: Expression,
    /// Full expression span.
    pub span: Span,
}

/// A supported prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    /// Arithmetic negation.
    Negate,
    /// Logical negation.
    Not,
    /// Creates an immutable scoped reference.
    Borrow,
    /// Creates a mutable scoped reference.
    BorrowMut,
    /// Reads through a reference or raw pointer.
    Dereference,
}

/// An infix operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryExpression {
    /// Operation to apply.
    pub operator: BinaryOperator,
    /// Left operand.
    pub left: Expression,
    /// Right operand.
    pub right: Expression,
    /// Full expression span.
    pub span: Span,
}

/// A supported infix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    /// Checked integer addition.
    Add,
    /// Checked integer subtraction.
    Subtract,
    /// Checked integer multiplication.
    Multiply,
    /// Checked integer division.
    Divide,
    /// Checked integer remainder.
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
    /// Less-than comparison.
    Less,
    /// Less-than-or-equal comparison.
    LessEqual,
    /// Greater-than comparison.
    Greater,
    /// Greater-than-or-equal comparison.
    GreaterEqual,
    /// Short-circuit logical conjunction.
    And,
    /// Short-circuit logical disjunction.
    Or,
}

/// A direct or computed function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallExpression {
    /// Expression producing the callee.
    pub callee: Expression,
    /// Explicit type or constant arguments written before the call parentheses.
    pub generic_arguments: Vec<GenericArgument>,
    /// Arguments in source order.
    pub arguments: Vec<Expression>,
    /// Full expression span.
    pub span: Span,
}

/// A conditional expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpression {
    /// Boolean condition.
    pub condition: Expression,
    /// Branch used when the condition is true.
    pub then_branch: Block,
    /// Optional `else if` or block expression.
    pub else_branch: Option<Expression>,
    /// Full expression span.
    pub span: Span,
}

/// An assignment expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentExpression {
    /// Target expression.
    pub target: Expression,
    /// Assignment form.
    pub operator: AssignmentOperator,
    /// New or combined value.
    pub value: Expression,
    /// Full expression span.
    pub span: Span,
}

/// An explicit `value as Type` conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastExpression {
    /// Value being converted.
    pub value: Expression,
    /// Destination type.
    pub target: TypeName,
    /// Full expression span.
    pub span: Span,
}

/// A postfix field access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldExpression {
    /// Aggregate value.
    pub base: Expression,
    /// Named field or tuple position.
    pub field: FieldName,
    /// Full expression span.
    pub span: Span,
}

/// A field selector after `.`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldName {
    /// Named struct field.
    Named(Identifier),
    /// Tuple element number and source location.
    TupleIndex { index: u32, span: Span },
}

/// A postfix checked index operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexExpression {
    /// Indexed aggregate value.
    pub base: Expression,
    /// One or more indices.
    pub indices: Vec<Expression>,
    /// Full expression span.
    pub span: Span,
}

/// A supported assignment operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentOperator {
    /// `=`
    Assign,
    /// `+=`
    Add,
    /// `-=`
    Subtract,
    /// `*=`
    Multiply,
    /// `/=`
    Divide,
    /// `%=`
    Remainder,
    /// `&=`
    BitAnd,
    /// `^=`
    BitXor,
    /// `|=`
    BitOr,
    /// `<<=`
    ShiftLeft,
    /// `>>=`
    ShiftRight,
}
