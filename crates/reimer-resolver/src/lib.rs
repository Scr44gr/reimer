//! Name resolution and type checking from Reimer AST to typed HIR.

mod comptime;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::mem::size_of;

use reimer_ast::{
    self as ast, AssignmentOperator as AstAssignmentOperator, BinaryOperator as AstBinaryOperator,
    Expression as AstExpression, Item, Statement as AstStatement, TypeNameKind,
    UnaryOperator as AstUnaryOperator,
};
use reimer_diagnostics::{Diagnostic, Span};
use reimer_hir::{
    self as hir, AssertionMode, AssignmentOperator, BinaryOperator, Expression, ExpressionKind,
    FunctionId, IntegerAdditionMode, LocalId, Place, PlaceKind, StaticId, UnaryOperator,
};
use reimer_layout::Layouts;
use reimer_types::{Type, TypeId};

const STANDARD_STRING_TYPE: &str = "__module_3_std_6_string$String";
const STANDARD_VEC_TYPE: &str = "__module_3_std_11_collections$Vec";
const STANDARD_HASH_MAP_TYPE: &str = "__module_3_std_11_collections$HashMap";
const STANDARD_HASH_SET_TYPE: &str = "__module_3_std_11_collections$HashSet";
const STANDARD_RING_BUFFER_TYPE: &str = "__module_3_std_11_collections$RingBuffer";
const STANDARD_DISPLAY_TRAIT: &str = "__module_3_std_6_string$Display";
const STANDARD_APPEND_DISPLAY: &str = "__module_3_std_6_string$append_display";
const STANDARD_DEBUG_TRAIT: &str = "__module_3_std_6_string$Debug";
const STANDARD_APPEND_DEBUG: &str = "__module_3_std_6_string$append_debug";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormattingMode {
    Display,
    Debug,
}

/// Resolves names and checks the types of one parsed compilation unit.
///
/// # Errors
///
/// Returns accumulated semantic diagnostics for invalid declarations,
/// unresolved names, type mismatches, mutability violations, and invalid
/// control flow.
pub fn resolve(program: &ast::Program) -> Result<hir::Program, Vec<Diagnostic>> {
    Resolver::new().resolve(program, true)
}

/// Resolves names and checks types without requiring an executable entry point.
///
/// # Errors
///
/// Returns accumulated semantic diagnostics for invalid declarations,
/// unresolved names, type mismatches, mutability violations, and invalid
/// control flow.
pub fn resolve_library(program: &ast::Program) -> Result<hir::Program, Vec<Diagnostic>> {
    Resolver::new().resolve(program, false)
}

#[derive(Debug, Clone)]
struct Signature {
    id: FunctionId,
    parameter_types: Vec<Type>,
    return_type: Type,
    requires_unsafe: bool,
    is_public: bool,
}

#[derive(Debug, Clone)]
struct StaticSymbol {
    id: StaticId,
    mutable: bool,
    ty: Type,
}

struct SignatureSource<'ast> {
    resolved_name: &'ast str,
    source_name: &'ast ast::Identifier,
    parameters: &'ast [ast::Parameter],
    return_type: Option<&'ast ast::TypeName>,
    span: Span,
    requires_unsafe: bool,
    is_public: bool,
}

#[derive(Debug, Clone, Copy)]
struct ThreadErrorVariants {
    spawn_failed: u32,
    invalid_handle: u32,
    worker_panicked: u32,
    result_mismatch: u32,
}

#[derive(Debug, Clone, Copy)]
struct JobErrorVariants {
    submit_failed: u32,
    invalid_handle: u32,
    worker_panicked: u32,
    result_mismatch: u32,
}

#[derive(Debug, Clone, Copy)]
enum ParallelInputKind {
    Slice,
    Array,
}

enum Declaration<'ast> {
    Source {
        function: &'ast ast::Function,
        resolved_name: String,
        signature: Signature,
    },
    Extern {
        function: &'ast ast::ExternFunction,
        signature: Signature,
    },
}

#[derive(Debug, Clone)]
struct GenericFunctionTemplate {
    function: ast::Function,
    parameters: Vec<ast::GenericParameter>,
    where_predicates: Vec<ast::WherePredicate>,
    resolved_name: String,
    module_identity: Option<String>,
    explicit_parameter_start: usize,
    is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GenericFunctionKey {
    resolved_name: String,
    arguments: Vec<GenericValue>,
    pack_lengths: Vec<usize>,
}

#[derive(Debug, Clone)]
struct PendingFunction {
    function: ast::Function,
    resolved_name: String,
    signature: Signature,
    environment: GenericEnvironment,
    module_identity: Option<String>,
}

#[derive(Default)]
struct GenericFunctionRegistry {
    templates: HashMap<String, GenericFunctionTemplate>,
    instances: HashMap<GenericFunctionKey, Signature>,
    pending: Vec<PendingFunction>,
    next_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum StructuralType {
    Tuple(Vec<Type>),
    Array {
        element: Type,
        length: u64,
    },
    Reference {
        target: Type,
        mutable: bool,
    },
    RawPointer {
        target: Type,
        mutable: bool,
    },
    Slice {
        element: Type,
        mutable: bool,
    },
    Function {
        parameters: Vec<Type>,
        return_type: Type,
    },
    Option(Type),
    Result {
        success: Type,
        error: Type,
    },
}

#[derive(Debug, Clone, Copy)]
enum IntrinsicType {
    Option { value: Type },
    Result { success: Type, error: Type },
}

#[derive(Debug, Clone, Copy)]
enum StringViewIntrinsic {
    Data,
    Length,
}

#[derive(Debug, Clone, Copy)]
enum ThreadSafety {
    Send,
    Sync,
}

#[derive(Debug, Clone)]
enum GenericTypeTemplate {
    Struct(ast::StructDeclaration),
    Enum(ast::EnumDeclaration),
}

impl GenericTypeTemplate {
    fn parameters(&self) -> &[ast::GenericParameter] {
        match self {
            Self::Struct(declaration) => &declaration.generic_parameters,
            Self::Enum(declaration) => &declaration.generic_parameters,
        }
    }

    fn span(&self) -> Span {
        match self {
            Self::Struct(declaration) => declaration.span,
            Self::Enum(declaration) => declaration.span,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GenericValue {
    Type(Type),
    Const(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GenericTypeKey {
    name: String,
    arguments: Vec<GenericValue>,
}

#[derive(Debug, Clone)]
struct GenericTypeInstance {
    base_name: String,
    arguments: Vec<GenericValue>,
}

#[derive(Debug, Clone)]
struct TraitDefinition {
    declaration: ast::TraitDeclaration,
}

#[derive(Debug, Clone)]
struct TraitImplementation {
    trait_name: String,
    target: ast::TypeName,
    parameters: Vec<ast::GenericParameter>,
    where_predicates: Vec<ast::WherePredicate>,
}

#[derive(Debug, Clone, Default)]
struct GenericEnvironment {
    types: HashMap<String, Type>,
    type_packs: HashMap<String, Vec<Type>>,
    constants: HashMap<String, u64>,
}

#[derive(Default)]
struct TypeRegistry {
    names: HashMap<String, Type>,
    structural: HashMap<StructuralType, Type>,
    definitions: Vec<hir::TypeDefinition>,
    chars: Option<Type>,
    intrinsics: HashMap<Type, IntrinsicType>,
    generic_templates: HashMap<String, GenericTypeTemplate>,
    generic_instances: HashMap<GenericTypeKey, Type>,
    generic_instance_data: HashMap<Type, GenericTypeInstance>,
    traits: HashMap<String, TraitDefinition>,
    trait_implementations: Vec<TraitImplementation>,
    constants: HashMap<String, hir::Expression>,
    constant_integers: HashMap<String, u64>,
}

impl TypeRegistry {
    fn base_environment(&self) -> GenericEnvironment {
        GenericEnvironment {
            types: HashMap::new(),
            type_packs: HashMap::new(),
            constants: self.constant_integers.clone(),
        }
    }

    fn remember_preliminary_constants(
        &mut self,
        constants: &HashMap<String, comptime::EvaluatedConstant>,
    ) {
        self.constant_integers = constants
            .iter()
            .filter_map(|(name, constant)| {
                constant
                    .value
                    .as_non_negative_u128()
                    .and_then(|value| u64::try_from(value).ok())
                    .map(|value| (name.clone(), value))
            })
            .collect();
    }

    fn lower_compiletime_value(
        &self,
        value: &comptime::Value,
        ty: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Expression> {
        let kind = match (value, ty) {
            (comptime::Value::Integer(_), ty) if ty.is_integer() => {
                return Self::lower_compiletime_integer(value, ty, span, diagnostics);
            }
            (comptime::Value::Float(bits), Type::F64) => ExpressionKind::Float64(*bits),
            (comptime::Value::Float(bits), Type::F32) => {
                let narrowed = narrow_f64_to_f32(f64::from_bits(*bits));
                if !narrowed.is_finite() {
                    report_compiletime_type_mismatch(
                        ty,
                        "a floating-point value outside the `f32` range",
                        span,
                        diagnostics,
                    );
                    return None;
                }
                ExpressionKind::Float32(narrowed.to_bits())
            }
            (comptime::Value::Boolean(value), Type::Bool) => ExpressionKind::Boolean(*value),
            (comptime::Value::Character(value), Type::Char) => ExpressionKind::Character(*value),
            (comptime::Value::String(value), Type::Str) => ExpressionKind::String(value.clone()),
            (comptime::Value::String(value), Type::CStr) if !value.contains('\0') => {
                ExpressionKind::CString(value.clone())
            }
            (comptime::Value::Unit, Type::Unit) => ExpressionKind::Unit,
            (comptime::Value::Tuple(values), Type::Tuple(_)) => {
                self.lower_compiletime_tuple(values, ty, span, diagnostics)?
            }
            (comptime::Value::Array(values), Type::Array(_)) => {
                self.lower_compiletime_array(values, ty, span, diagnostics)?
            }
            (comptime::Value::Record(values), Type::Struct(_)) => {
                self.lower_compiletime_record(values, ty, span, diagnostics)?
            }
            _ => {
                report_compiletime_type_mismatch(
                    ty,
                    compiletime_value_kind(value),
                    span,
                    diagnostics,
                );
                return None;
            }
        };
        Some(Expression { kind, ty, span })
    }

    fn lower_compiletime_tuple(
        &self,
        values: &[comptime::Value],
        ty: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<ExpressionKind> {
        let hir::TypeDefinitionKind::Tuple { elements } = self.definition(ty)?.kind.clone() else {
            return None;
        };
        if values.len() != elements.len() {
            report_compiletime_type_mismatch(
                ty,
                "a tuple with a different arity",
                span,
                diagnostics,
            );
            return None;
        }
        values
            .iter()
            .zip(elements)
            .map(|(value, field_type)| {
                self.lower_compiletime_value(value, field_type, span, diagnostics)
            })
            .collect::<Option<Vec<_>>>()
            .map(ExpressionKind::Tuple)
    }

    fn lower_compiletime_array(
        &self,
        values: &[comptime::Value],
        ty: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<ExpressionKind> {
        let hir::TypeDefinitionKind::Array { element, length } = self.definition(ty)?.kind else {
            return None;
        };
        if u64::try_from(values.len()).ok() != Some(length) {
            report_compiletime_type_mismatch(
                ty,
                "an array with a different length",
                span,
                diagnostics,
            );
            return None;
        }
        values
            .iter()
            .map(|value| self.lower_compiletime_value(value, element, span, diagnostics))
            .collect::<Option<Vec<_>>>()
            .map(ExpressionKind::Array)
    }

    fn lower_compiletime_record(
        &self,
        values: &BTreeMap<String, comptime::Value>,
        ty: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<ExpressionKind> {
        let hir::TypeDefinitionKind::Struct { fields } = self.definition(ty)?.kind.clone() else {
            return None;
        };
        if values.len() != fields.len()
            || fields.iter().any(|field| !values.contains_key(&field.name))
        {
            report_compiletime_type_mismatch(
                ty,
                "a record with different fields",
                span,
                diagnostics,
            );
            return None;
        }
        fields
            .iter()
            .map(|field| {
                self.lower_compiletime_value(values.get(&field.name)?, field.ty, span, diagnostics)
            })
            .collect::<Option<Vec<_>>>()
            .map(ExpressionKind::Struct)
    }

    fn lower_compiletime_integer(
        value: &comptime::Value,
        ty: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Expression> {
        let (negative, magnitude) = value.as_integer()?;
        let maximum = if negative {
            if !ty.is_signed_integer() {
                report_compiletime_type_mismatch(ty, "a negative integer", span, diagnostics);
                return None;
            }
            integer_minimum_magnitude(ty)
        } else {
            integer_positive_maximum(ty)
        };
        if magnitude > maximum {
            report_compiletime_type_mismatch(ty, "an out-of-range integer", span, diagnostics);
            return None;
        }
        let literal = Expression {
            kind: ExpressionKind::Integer(magnitude),
            ty,
            span,
        };
        if !negative || magnitude == integer_minimum_magnitude(ty) {
            Some(literal)
        } else {
            Some(Expression {
                kind: ExpressionKind::Unary {
                    operator: UnaryOperator::Negate,
                    operand: Box::new(literal),
                },
                ty,
                span,
            })
        }
    }

    fn push_definition(
        &mut self,
        name: Option<String>,
        kind: hir::TypeDefinitionKind,
        span: Span,
    ) -> Option<TypeId> {
        let id = TypeId(u32::try_from(self.definitions.len()).ok()?);
        self.definitions.push(hir::TypeDefinition {
            id,
            name,
            documentation: None,
            kind,
            representation: hir::TypeRepresentation::Native,
            alignment: None,
            derives: Vec::new(),
            marker_traits: Vec::new(),
            must_use: false,
            span,
        });
        Some(id)
    }

    fn intern_tuple(
        &mut self,
        elements: Vec<Type>,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let key = StructuralType::Tuple(elements.clone());
        if let Some(ty) = self.structural.get(&key) {
            return Some(*ty);
        }
        let Some(id) =
            self.push_definition(None, hir::TypeDefinitionKind::Tuple { elements }, span)
        else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = Type::Tuple(id);
        self.structural.insert(key, ty);
        Some(ty)
    }

    fn intern_array(
        &mut self,
        element: Type,
        length: u64,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let key = StructuralType::Array { element, length };
        if let Some(ty) = self.structural.get(&key) {
            return Some(*ty);
        }
        let Some(id) = self.push_definition(
            None,
            hir::TypeDefinitionKind::Array { element, length },
            span,
        ) else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = Type::Array(id);
        self.structural.insert(key, ty);
        Some(ty)
    }

    fn intern_indirect_type(
        &mut self,
        key: StructuralType,
        kind: hir::TypeDefinitionKind,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
        constructor: fn(TypeId) -> Type,
    ) -> Option<Type> {
        if let Some(ty) = self.structural.get(&key) {
            return Some(*ty);
        }
        let Some(id) = self.push_definition(None, kind, span) else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = constructor(id);
        self.structural.insert(key, ty);
        Some(ty)
    }

    fn intern_reference(
        &mut self,
        target: Type,
        mutable: bool,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        self.intern_indirect_type(
            StructuralType::Reference { target, mutable },
            hir::TypeDefinitionKind::Reference { target, mutable },
            span,
            diagnostics,
            Type::Reference,
        )
    }

    fn intern_raw_pointer(
        &mut self,
        target: Type,
        mutable: bool,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        self.intern_indirect_type(
            StructuralType::RawPointer { target, mutable },
            hir::TypeDefinitionKind::RawPointer { target, mutable },
            span,
            diagnostics,
            Type::RawPointer,
        )
    }

    fn intern_slice(
        &mut self,
        element: Type,
        mutable: bool,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        self.intern_indirect_type(
            StructuralType::Slice { element, mutable },
            hir::TypeDefinitionKind::Slice { element, mutable },
            span,
            diagnostics,
            Type::Slice,
        )
    }

    fn intern_function(
        &mut self,
        parameters: Vec<Type>,
        return_type: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        self.intern_indirect_type(
            StructuralType::Function {
                parameters: parameters.clone(),
                return_type,
            },
            hir::TypeDefinitionKind::Function {
                parameters,
                return_type,
            },
            span,
            diagnostics,
            Type::Function,
        )
    }

    fn intern_option(
        &mut self,
        value: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let key = StructuralType::Option(value);
        if let Some(ty) = self.structural.get(&key) {
            return Some(*ty);
        }
        let kind = hir::TypeDefinitionKind::Enum {
            variants: vec![
                hir::EnumVariant {
                    name: "Some".to_owned(),
                    fields: hir::EnumVariantFields::Tuple(vec![value]),
                    span,
                },
                hir::EnumVariant {
                    name: "None".to_owned(),
                    fields: hir::EnumVariantFields::Unit,
                    span,
                },
            ],
        };
        let Some(id) = self.push_definition(Some("Option".to_owned()), kind, span) else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = Type::Enum(id);
        self.structural.insert(key, ty);
        self.intrinsics.insert(ty, IntrinsicType::Option { value });
        Some(ty)
    }

    fn intern_chars(&mut self, span: Span, diagnostics: &mut Vec<Diagnostic>) -> Option<Type> {
        if let Some(ty) = self.chars {
            return Some(ty);
        }
        let kind = hir::TypeDefinitionKind::Struct {
            fields: vec![
                hir::TypeField {
                    name: "source".to_owned(),
                    is_public: false,
                    ty: Type::Str,
                    span,
                },
                hir::TypeField {
                    name: "offset".to_owned(),
                    is_public: false,
                    ty: Type::Usize,
                    span,
                },
            ],
        };
        let Some(id) = self.push_definition(Some("Chars".to_owned()), kind, Span::empty(0)) else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = Type::Struct(id);
        if let Ok(index) = usize::try_from(id.0)
            && let Some(definition) = self.definitions.get_mut(index)
        {
            definition.must_use = true;
        }
        self.chars = Some(ty);
        Some(ty)
    }

    fn is_chars(&self, ty: Type) -> bool {
        self.chars == Some(ty)
    }

    fn intern_result(
        &mut self,
        success: Type,
        error: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let key = StructuralType::Result { success, error };
        if let Some(ty) = self.structural.get(&key) {
            return Some(*ty);
        }
        let kind = hir::TypeDefinitionKind::Enum {
            variants: vec![
                hir::EnumVariant {
                    name: "Ok".to_owned(),
                    fields: hir::EnumVariantFields::Tuple(vec![success]),
                    span,
                },
                hir::EnumVariant {
                    name: "Err".to_owned(),
                    fields: hir::EnumVariantFields::Tuple(vec![error]),
                    span,
                },
            ],
        };
        let Some(id) = self.push_definition(Some("Result".to_owned()), kind, span) else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = Type::Enum(id);
        self.structural.insert(key, ty);
        self.intrinsics
            .insert(ty, IntrinsicType::Result { success, error });
        Some(ty)
    }

    fn resolve_type_name(
        &mut self,
        type_name: &ast::TypeName,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let environment = self.base_environment();
        self.resolve_type_name_in(type_name, &environment, diagnostics)
    }

    fn resolve_type_name_in(
        &mut self,
        type_name: &ast::TypeName,
        environment: &GenericEnvironment,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        match &type_name.kind {
            TypeNameKind::Function {
                parameters: parameter_names,
                return_type,
            } => {
                let parameters =
                    self.resolve_type_sequence(parameter_names, environment, diagnostics)?;
                let return_type =
                    self.resolve_type_name_in(return_type, environment, diagnostics)?;
                self.intern_function(parameters, return_type, type_name.span, diagnostics)
            }
            TypeNameKind::Unit => Some(Type::Unit),
            TypeNameKind::Generic { path, arguments } => self.resolve_generic_type_name(
                path,
                arguments,
                environment,
                type_name.span,
                diagnostics,
            ),
            TypeNameKind::Tuple(element_names) => {
                let elements =
                    self.resolve_type_sequence(element_names, environment, diagnostics)?;
                self.intern_tuple(elements, type_name.span, diagnostics)
            }
            TypeNameKind::PackExpansion { .. } => {
                diagnostics.push(
                    Diagnostic::error(
                        "E6020",
                        "a type pack can only expand inside a tuple, function type, or generic argument list",
                        type_name.span,
                    )
                    .with_help("wrap the expansion in a type list such as `(...Types)`"),
                );
                None
            }
            TypeNameKind::Array { element, length } => {
                let element = self.resolve_type_name_in(element, environment, diagnostics)?;
                let length = evaluate_array_length_in(length, environment, diagnostics)?;
                self.intern_array(element, length, type_name.span, diagnostics)
            }
            TypeNameKind::Reference { mutable, target } => {
                if let TypeNameKind::Slice(element) = &target.kind {
                    let element = self.resolve_type_name_in(element, environment, diagnostics)?;
                    self.intern_slice(element, *mutable, type_name.span, diagnostics)
                } else {
                    let target = self.resolve_type_name_in(target, environment, diagnostics)?;
                    self.intern_reference(target, *mutable, type_name.span, diagnostics)
                }
            }
            TypeNameKind::RawPointer { mutable, target } => {
                let target = self.resolve_type_name_in(target, environment, diagnostics)?;
                self.intern_raw_pointer(target, *mutable, type_name.span, diagnostics)
            }
            TypeNameKind::Slice(_) => {
                diagnostics.push(
                    Diagnostic::error(
                        "E3006",
                        "slice types require a scoped reference in Reimer v0.1",
                        type_name.span,
                    )
                    .with_help("write `&[T]` or `&mut [T]` once references are enabled"),
                );
                None
            }
            TypeNameKind::Path(path) => {
                let ty = single_path_name(path)
                    .and_then(|name| environment.types.get(name).copied())
                    .or_else(|| single_path_name(path).and_then(primitive_type))
                    .or_else(|| {
                        single_path_name(path).and_then(|name| self.names.get(name).copied())
                    });
                if ty.is_none() {
                    let message = if single_path_name(path)
                        .is_some_and(|name| self.generic_templates.contains_key(name))
                    {
                        format!(
                            "generic type `{}` requires type or const arguments",
                            path.display()
                        )
                    } else {
                        format!("unknown type `{}`", path.display())
                    };
                    diagnostics.push(
                        Diagnostic::error("E3005", message, type_name.span)
                            .with_help("declare the type or supply its generic arguments"),
                    );
                }
                ty
            }
        }
    }

    fn resolve_type_sequence(
        &mut self,
        type_names: &[ast::TypeName],
        environment: &GenericEnvironment,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Vec<Type>> {
        let mut resolved = Vec::new();
        for type_name in type_names {
            match &type_name.kind {
                TypeNameKind::PackExpansion { pack, template } => {
                    let Some(types) = environment.type_packs.get(&pack.name).cloned() else {
                        diagnostics.push(
                            Diagnostic::error(
                                "E6020",
                                format!("unknown type pack `{}`", pack.name),
                                pack.span,
                            )
                            .with_help("declare the pack with `<...Types>`"),
                        );
                        return None;
                    };
                    for ty in types {
                        if let Some(template) = template {
                            let mut element_environment = environment.clone();
                            element_environment.types.insert(pack.name.clone(), ty);
                            resolved.push(self.resolve_type_name_in(
                                template,
                                &element_environment,
                                diagnostics,
                            )?);
                        } else {
                            resolved.push(ty);
                        }
                    }
                }
                _ => {
                    resolved.push(self.resolve_type_name_in(
                        type_name,
                        environment,
                        diagnostics,
                    )?);
                }
            }
        }
        Some(resolved)
    }

    fn resolve_generic_type_name(
        &mut self,
        path: &ast::Path,
        arguments: &[ast::GenericArgument],
        environment: &GenericEnvironment,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let name = single_path_name(path);
        match (name, arguments) {
            (Some("Option"), [ast::GenericArgument::Type(value)]) => {
                let value = self.resolve_type_name_in(value, environment, diagnostics)?;
                self.intern_option(value, span, diagnostics)
            }
            (
                Some("Result"),
                [
                    ast::GenericArgument::Type(success),
                    ast::GenericArgument::Type(error),
                ],
            ) => {
                let success = self.resolve_type_name_in(success, environment, diagnostics)?;
                let error = self.resolve_type_name_in(error, environment, diagnostics)?;
                self.intern_result(success, error, span, diagnostics)
            }
            (Some("Option" | "Result"), _) => {
                diagnostics.push(Diagnostic::error(
                    "E3007",
                    format!("invalid type argument count for `{}`", path.display()),
                    span,
                ));
                None
            }
            (Some(name), _) => {
                self.instantiate_generic_type(name, arguments, environment, span, diagnostics)
            }
            (None, _) => {
                diagnostics.push(Diagnostic::error(
                    "E6003",
                    format!(
                        "generic type path `{}` must resolve to one declared type",
                        path.display()
                    ),
                    span,
                ));
                None
            }
        }
    }

    fn instantiate_generic_type(
        &mut self,
        name: &str,
        arguments: &[ast::GenericArgument],
        outer_environment: &GenericEnvironment,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        let Some(template) = self.generic_templates.get(name).cloned() else {
            diagnostics.push(
                Diagnostic::error("E6003", format!("unknown generic type `{name}`"), span)
                    .with_help("declare the generic struct or enum before using it"),
            );
            return None;
        };
        let (values, environment) = self.bind_generic_arguments(
            template.parameters(),
            arguments,
            outer_environment,
            span,
            diagnostics,
        )?;
        let where_predicates = match &template {
            GenericTypeTemplate::Struct(declaration) => &declaration.where_predicates,
            GenericTypeTemplate::Enum(declaration) => &declaration.where_predicates,
        };
        if !self.validate_bounds(
            template.parameters(),
            where_predicates,
            &environment,
            span,
            diagnostics,
        ) {
            return None;
        }
        let key = GenericTypeKey {
            name: name.to_owned(),
            arguments: values.clone(),
        };
        if let Some(ty) = self.generic_instances.get(&key) {
            if self.reject_hidden_scoped_storage(&values, *ty, span, diagnostics) {
                return None;
            }
            return Some(*ty);
        }

        let (ty, id, representation) =
            self.reserve_generic_type(name, &template, &values, key, span, diagnostics)?;
        let attributes = match &template {
            GenericTypeTemplate::Struct(declaration) => &declaration.attributes,
            GenericTypeTemplate::Enum(declaration) => &declaration.attributes,
        };
        let alignment = requested_alignment(attributes);
        let derives = derived_traits(attributes);
        let marker_traits = derived_marker_traits(attributes);
        let must_use = has_marker_attribute(attributes, "must_use");
        let kind = match template {
            GenericTypeTemplate::Struct(declaration) => hir::TypeDefinitionKind::Struct {
                fields: self.resolve_fields_in(&declaration.fields, &environment, diagnostics),
            },
            GenericTypeTemplate::Enum(declaration) => hir::TypeDefinitionKind::Enum {
                variants: self.resolve_variants_in(
                    &declaration.variants,
                    &environment,
                    diagnostics,
                ),
            },
        };
        if let Some(definition) = self
            .definitions
            .get_mut(usize::try_from(id.0).unwrap_or(usize::MAX))
        {
            definition.representation = representation;
            definition.alignment = alignment;
            definition.derives = derives;
            definition.marker_traits = marker_traits;
            definition.must_use = must_use;
            definition.kind = kind;
        }
        self.validate_type_derives(ty, diagnostics);
        if self.reject_hidden_scoped_storage(&values, ty, span, diagnostics) {
            return None;
        }
        Some(ty)
    }

    fn reserve_generic_type(
        &mut self,
        name: &str,
        template: &GenericTypeTemplate,
        values: &[GenericValue],
        key: GenericTypeKey,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<(Type, TypeId, hir::TypeRepresentation)> {
        let display_name = self.generic_type_display_name(name, values);
        let (placeholder, constructor, representation) = match template {
            GenericTypeTemplate::Struct(declaration) => (
                hir::TypeDefinitionKind::Struct { fields: Vec::new() },
                Type::Struct as fn(TypeId) -> Type,
                if has_identifier_attribute(&declaration.attributes, "repr", "C") {
                    hir::TypeRepresentation::C
                } else {
                    hir::TypeRepresentation::Native
                },
            ),
            GenericTypeTemplate::Enum(_) => (
                hir::TypeDefinitionKind::Enum {
                    variants: Vec::new(),
                },
                Type::Enum as fn(TypeId) -> Type,
                hir::TypeRepresentation::Native,
            ),
        };
        let Some(id) = self.push_definition(Some(display_name), placeholder, template.span())
        else {
            diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many types",
                span,
            ));
            return None;
        };
        let ty = constructor(id);
        self.generic_instances.insert(key, ty);
        self.generic_instance_data.insert(
            ty,
            GenericTypeInstance {
                base_name: name.to_owned(),
                arguments: values.to_vec(),
            },
        );
        Some((ty, id, representation))
    }

    fn reject_hidden_scoped_storage(
        &self,
        values: &[GenericValue],
        result: Type,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> bool {
        let hides_scoped_value = values
            .iter()
            .any(|value| matches!(value, GenericValue::Type(ty) if self.is_scoped(*ty)))
            && !self.is_scoped(result);
        if hides_scoped_value {
            diagnostics.push(scoped_storage_diagnostic(span));
        }
        hides_scoped_value
    }

    fn bind_generic_arguments(
        &mut self,
        parameters: &[ast::GenericParameter],
        arguments: &[ast::GenericArgument],
        outer_environment: &GenericEnvironment,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<(Vec<GenericValue>, GenericEnvironment)> {
        let pack_position = parameters
            .iter()
            .position(|parameter| matches!(parameter, ast::GenericParameter::TypePack { .. }));
        let required = parameters
            .iter()
            .filter(|parameter| match parameter {
                ast::GenericParameter::Type { default, .. } => default.is_none(),
                ast::GenericParameter::Const { default, .. } => default.is_none(),
                ast::GenericParameter::TypePack { .. } => false,
            })
            .count();
        let maximum = pack_position.is_none().then_some(parameters.len());
        if arguments.len() < required || maximum.is_some_and(|maximum| arguments.len() > maximum) {
            let accepted = maximum.map_or_else(
                || format!("at least {required}"),
                |maximum| format!("between {required} and {maximum}"),
            );
            diagnostics.push(Diagnostic::error(
                "E6003",
                format!(
                    "generic type expects {accepted} argument(s), but {} were provided",
                    arguments.len()
                ),
                span,
            ));
            return None;
        }
        let mut environment = outer_environment.clone();
        let mut values = Vec::with_capacity(parameters.len());
        let supplied =
            self.resolve_generic_argument_values(arguments, outer_environment, diagnostics)?;
        let mut argument_index = 0;
        for parameter in parameters {
            let argument = supplied.get(argument_index).copied();
            match parameter {
                ast::GenericParameter::Type { name, default, .. } => {
                    let ty = match argument {
                        Some(GenericValue::Type(ty)) => {
                            argument_index += 1;
                            ty
                        }
                        Some(GenericValue::Const(_)) => {
                            diagnostics.push(Diagnostic::error(
                                "E6003",
                                format!("type parameter `{}` received a const argument", name.name),
                                span,
                            ));
                            return None;
                        }
                        None => {
                            self.resolve_type_name_in(default.as_ref()?, &environment, diagnostics)?
                        }
                    };
                    environment.types.insert(name.name.clone(), ty);
                    values.push(GenericValue::Type(ty));
                }
                ast::GenericParameter::TypePack { name, .. } => {
                    let mut types = Vec::new();
                    for value in &supplied[argument_index..] {
                        let GenericValue::Type(ty) = value else {
                            diagnostics.push(
                                Diagnostic::error(
                                    "E6020",
                                    format!("type pack `{}` received a const argument", name.name),
                                    span,
                                )
                                .with_help("type packs accept only type arguments"),
                            );
                            return None;
                        };
                        types.push(*ty);
                        values.push(GenericValue::Type(*ty));
                    }
                    argument_index = supplied.len();
                    environment.type_packs.insert(name.name.clone(), types);
                }
                ast::GenericParameter::Const { name, default, .. } => {
                    let value = match argument {
                        Some(GenericValue::Const(value)) => {
                            argument_index += 1;
                            value
                        }
                        Some(GenericValue::Type(_)) => {
                            diagnostics.push(Diagnostic::error(
                                "E6003",
                                format!("const parameter `{}` received a type argument", name.name),
                                span,
                            ));
                            return None;
                        }
                        None => {
                            evaluate_array_length_in(default.as_ref()?, &environment, diagnostics)?
                        }
                    };
                    environment.constants.insert(name.name.clone(), value);
                    values.push(GenericValue::Const(value));
                }
            }
        }
        Some((values, environment))
    }

    fn resolve_generic_argument_values(
        &mut self,
        arguments: &[ast::GenericArgument],
        environment: &GenericEnvironment,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Vec<GenericValue>> {
        let mut values = Vec::new();
        for argument in arguments {
            match argument {
                ast::GenericArgument::Type(ty) => {
                    if let TypeNameKind::Path(path) = &ty.kind
                        && let Some(name) = single_path_name(path)
                        && let Some(value) = environment.constants.get(name)
                    {
                        values.push(GenericValue::Const(*value));
                    } else {
                        values.push(GenericValue::Type(self.resolve_type_name_in(
                            ty,
                            environment,
                            diagnostics,
                        )?));
                    }
                }
                ast::GenericArgument::Const(value) => {
                    values.push(GenericValue::Const(evaluate_array_length_in(
                        value,
                        environment,
                        diagnostics,
                    )?));
                }
                ast::GenericArgument::Pack { pack, template, .. } => {
                    let Some(types) = environment.type_packs.get(&pack.name).cloned() else {
                        diagnostics.push(
                            Diagnostic::error(
                                "E6020",
                                format!("unknown type pack `{}`", pack.name),
                                pack.span,
                            )
                            .with_help("declare the pack with `<...Types>`"),
                        );
                        return None;
                    };
                    for ty in types {
                        if let Some(template) = template {
                            let mut element_environment = environment.clone();
                            element_environment.types.insert(pack.name.clone(), ty);
                            values.push(GenericValue::Type(self.resolve_type_name_in(
                                template,
                                &element_environment,
                                diagnostics,
                            )?));
                        } else {
                            values.push(GenericValue::Type(ty));
                        }
                    }
                }
            }
        }
        Some(values)
    }

    fn resolve_fields_in(
        &mut self,
        fields: &[ast::StructField],
        environment: &GenericEnvironment,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Vec<hir::TypeField> {
        let mut names = HashMap::new();
        let mut resolved = Vec::with_capacity(fields.len());
        for field in fields {
            if names.insert(&field.name.name, field.name.span).is_some() {
                diagnostics.push(Diagnostic::error(
                    "E3010",
                    format!("field `{}` is declared more than once", field.name.name),
                    field.name.span,
                ));
            }
            if let Some(ty) = self.resolve_type_name_in(&field.ty, environment, diagnostics) {
                resolved.push(hir::TypeField {
                    name: field.name.name.clone(),
                    is_public: field.is_public,
                    ty,
                    span: field.span,
                });
            }
        }
        resolved
    }

    fn resolve_variants_in(
        &mut self,
        variants: &[ast::EnumVariant],
        environment: &GenericEnvironment,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Vec<hir::EnumVariant> {
        let mut names = HashMap::new();
        let mut resolved = Vec::with_capacity(variants.len());
        for variant in variants {
            if names
                .insert(&variant.name.name, variant.name.span)
                .is_some()
            {
                diagnostics.push(Diagnostic::error(
                    "E3009",
                    format!(
                        "enum variant `{}` is declared more than once",
                        variant.name.name
                    ),
                    variant.name.span,
                ));
            }
            let fields = match &variant.payload {
                ast::EnumVariantPayload::Unit => hir::EnumVariantFields::Unit,
                ast::EnumVariantPayload::Tuple(types) => {
                    let mut fields = Vec::with_capacity(types.len());
                    for type_name in types {
                        if let Some(ty) =
                            self.resolve_type_name_in(type_name, environment, diagnostics)
                        {
                            fields.push(ty);
                        }
                    }
                    hir::EnumVariantFields::Tuple(fields)
                }
                ast::EnumVariantPayload::Struct(fields) => hir::EnumVariantFields::Struct(
                    self.resolve_fields_in(fields, environment, diagnostics),
                ),
            };
            resolved.push(hir::EnumVariant {
                name: variant.name.name.clone(),
                fields,
                span: variant.span,
            });
        }
        resolved
    }

    fn generic_type_display_name(&self, name: &str, values: &[GenericValue]) -> String {
        let arguments = values
            .iter()
            .map(|value| match value {
                GenericValue::Type(ty) => self
                    .definition(*ty)
                    .and_then(|definition| definition.name.clone())
                    .unwrap_or_else(|| ty.to_string()),
                GenericValue::Const(value) => value.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{name}<{arguments}>")
    }

    fn generic_instance(&self, ty: Type) -> Option<&GenericTypeInstance> {
        self.generic_instance_data.get(&ty)
    }

    fn infer_type_pattern(
        &self,
        pattern: &ast::TypeName,
        actual: Type,
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) -> bool {
        match &pattern.kind {
            TypeNameKind::Function {
                parameters: parameter_patterns,
                return_type,
            } => {
                let Some((actual_parameters, actual_return)) = self.function_shape(actual) else {
                    return false;
                };
                self.infer_type_sequence(
                    parameter_patterns,
                    actual_parameters,
                    parameters,
                    environment,
                ) && self.infer_type_pattern(return_type, actual_return, parameters, environment)
            }
            TypeNameKind::Unit => actual == Type::Unit,
            TypeNameKind::Path(path) => {
                let Some(name) = single_path_name(path) else {
                    return false;
                };
                if generic_type_parameter(parameters, name) {
                    return bind_type_argument(environment, name, actual);
                }
                environment
                    .types
                    .get(name)
                    .copied()
                    .or_else(|| primitive_type(name))
                    .or_else(|| self.names.get(name).copied())
                    .is_some_and(|expected| expected == actual)
            }
            TypeNameKind::Reference { mutable, target } => {
                self.infer_borrowed_type_pattern(*mutable, target, actual, parameters, environment)
            }
            TypeNameKind::RawPointer { mutable, target } => {
                self.pointer_shape(actual)
                    .is_some_and(|(actual_target, actual_mutable, raw)| {
                        raw && *mutable == actual_mutable
                            && self.infer_type_pattern(
                                target,
                                actual_target,
                                parameters,
                                environment,
                            )
                    })
            }
            TypeNameKind::Slice(element) => {
                self.slice_shape(actual).is_some_and(|(actual_element, _)| {
                    self.infer_type_pattern(element, actual_element, parameters, environment)
                })
            }
            TypeNameKind::Tuple(elements) => {
                let Some(definition) = self.definition(actual) else {
                    return false;
                };
                let hir::TypeDefinitionKind::Tuple {
                    elements: actual_elements,
                } = &definition.kind
                else {
                    return false;
                };
                self.infer_type_sequence(elements, actual_elements, parameters, environment)
            }
            TypeNameKind::Array { element, length } => {
                let Some(definition) = self.definition(actual) else {
                    return false;
                };
                let hir::TypeDefinitionKind::Array {
                    element: actual_element,
                    length: actual_length,
                } = definition.kind
                else {
                    return false;
                };
                self.infer_type_pattern(element, actual_element, parameters, environment)
                    && infer_const_pattern(length, actual_length, parameters, environment)
            }
            TypeNameKind::Generic { path, arguments } => {
                self.infer_generic_type_pattern(path, arguments, actual, parameters, environment)
            }
            TypeNameKind::PackExpansion { .. } => false,
        }
    }

    fn infer_type_sequence(
        &self,
        patterns: &[ast::TypeName],
        actual: &[Type],
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) -> bool {
        let expansions = patterns
            .iter()
            .enumerate()
            .filter_map(|(index, pattern)| {
                matches!(pattern.kind, TypeNameKind::PackExpansion { .. }).then_some(index)
            })
            .collect::<Vec<_>>();
        let [] = expansions.as_slice() else {
            let [expansion_index] = expansions.as_slice() else {
                return false;
            };
            if actual.len() < patterns.len().saturating_sub(1) {
                return false;
            }
            let suffix_len = patterns.len() - expansion_index - 1;
            let pack_end = actual.len() - suffix_len;
            if !patterns[..*expansion_index]
                .iter()
                .zip(&actual[..*expansion_index])
                .all(|(pattern, actual)| {
                    self.infer_type_pattern(pattern, *actual, parameters, environment)
                })
            {
                return false;
            }
            let TypeNameKind::PackExpansion { pack, template } = &patterns[*expansion_index].kind
            else {
                return false;
            };
            let mut inferred = Vec::with_capacity(pack_end - expansion_index);
            for actual_type in &actual[*expansion_index..pack_end] {
                if let Some(template) = template {
                    let mut element_environment = environment.clone();
                    element_environment.types.remove(&pack.name);
                    if !self.infer_type_pattern(
                        template,
                        *actual_type,
                        parameters,
                        &mut element_environment,
                    ) {
                        return false;
                    }
                    let Some(inferred_type) = element_environment.types.get(&pack.name).copied()
                    else {
                        return false;
                    };
                    inferred.push(inferred_type);
                } else {
                    inferred.push(*actual_type);
                }
            }
            if !bind_type_pack_argument(environment, &pack.name, inferred) {
                return false;
            }
            return patterns[expansion_index + 1..]
                .iter()
                .zip(&actual[pack_end..])
                .all(|(pattern, actual)| {
                    self.infer_type_pattern(pattern, *actual, parameters, environment)
                });
        };
        patterns.len() == actual.len()
            && patterns.iter().zip(actual).all(|(pattern, actual)| {
                self.infer_type_pattern(pattern, *actual, parameters, environment)
            })
    }

    fn infer_borrowed_type_pattern(
        &self,
        mutable: bool,
        target: &ast::TypeName,
        actual: Type,
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) -> bool {
        if let TypeNameKind::Slice(element) = &target.kind {
            return self
                .slice_shape(actual)
                .is_some_and(|(actual_element, actual_mutable)| {
                    mutable == actual_mutable
                        && self.infer_type_pattern(element, actual_element, parameters, environment)
                });
        }
        self.pointer_shape(actual)
            .is_some_and(|(actual_target, actual_mutable, raw)| {
                !raw && mutable == actual_mutable
                    && self.infer_type_pattern(target, actual_target, parameters, environment)
            })
    }

    fn infer_generic_type_pattern(
        &self,
        path: &ast::Path,
        arguments: &[ast::GenericArgument],
        actual: Type,
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) -> bool {
        let Some(name) = single_path_name(path) else {
            return false;
        };
        if name == "Option" {
            let Some(IntrinsicType::Option { value }) = self.intrinsic(actual) else {
                return false;
            };
            let [ast::GenericArgument::Type(argument)] = arguments else {
                return false;
            };
            return self.infer_type_pattern(argument, value, parameters, environment);
        }
        if name == "Result" {
            let Some(IntrinsicType::Result { success, error }) = self.intrinsic(actual) else {
                return false;
            };
            let [
                ast::GenericArgument::Type(success_pattern),
                ast::GenericArgument::Type(error_pattern),
            ] = arguments
            else {
                return false;
            };
            return self.infer_type_pattern(success_pattern, success, parameters, environment)
                && self.infer_type_pattern(error_pattern, error, parameters, environment);
        }
        let Some(instance) = self.generic_instance(actual) else {
            return false;
        };
        instance.base_name == name
            && self.infer_generic_argument_sequence(
                arguments,
                &instance.arguments,
                parameters,
                environment,
            )
    }

    fn infer_generic_argument_sequence(
        &self,
        patterns: &[ast::GenericArgument],
        actual: &[GenericValue],
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) -> bool {
        let expansions = patterns
            .iter()
            .enumerate()
            .filter_map(|(index, pattern)| {
                matches!(pattern, ast::GenericArgument::Pack { .. }).then_some(index)
            })
            .collect::<Vec<_>>();
        let [] = expansions.as_slice() else {
            let [expansion_index] = expansions.as_slice() else {
                return false;
            };
            if actual.len() < patterns.len().saturating_sub(1) {
                return false;
            }
            let suffix_len = patterns.len() - expansion_index - 1;
            let pack_end = actual.len() - suffix_len;
            if !patterns[..*expansion_index]
                .iter()
                .zip(&actual[..*expansion_index])
                .all(|(pattern, actual)| {
                    self.infer_generic_argument(pattern, *actual, parameters, environment)
                })
            {
                return false;
            }
            let ast::GenericArgument::Pack { pack, template, .. } = &patterns[*expansion_index]
            else {
                return false;
            };
            let mut inferred = Vec::with_capacity(pack_end - expansion_index);
            for actual_value in &actual[*expansion_index..pack_end] {
                let GenericValue::Type(actual_type) = actual_value else {
                    return false;
                };
                if let Some(template) = template {
                    let mut element_environment = environment.clone();
                    element_environment.types.remove(&pack.name);
                    if !self.infer_type_pattern(
                        template,
                        *actual_type,
                        parameters,
                        &mut element_environment,
                    ) {
                        return false;
                    }
                    let Some(inferred_type) = element_environment.types.get(&pack.name).copied()
                    else {
                        return false;
                    };
                    inferred.push(inferred_type);
                } else {
                    inferred.push(*actual_type);
                }
            }
            if !bind_type_pack_argument(environment, &pack.name, inferred) {
                return false;
            }
            return patterns[expansion_index + 1..]
                .iter()
                .zip(&actual[pack_end..])
                .all(|(pattern, actual)| {
                    self.infer_generic_argument(pattern, *actual, parameters, environment)
                });
        };
        patterns.len() == actual.len()
            && patterns.iter().zip(actual).all(|(pattern, actual)| {
                self.infer_generic_argument(pattern, *actual, parameters, environment)
            })
    }

    fn infer_generic_argument(
        &self,
        argument: &ast::GenericArgument,
        actual: GenericValue,
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) -> bool {
        match (argument, actual) {
            (ast::GenericArgument::Type(pattern), GenericValue::Type(actual)) => {
                self.infer_type_pattern(pattern, actual, parameters, environment)
            }
            (ast::GenericArgument::Const(pattern), GenericValue::Const(actual)) => {
                infer_const_pattern(pattern, actual, parameters, environment)
            }
            (
                ast::GenericArgument::Type(ast::TypeName {
                    kind: TypeNameKind::Path(path),
                    ..
                }),
                GenericValue::Const(actual),
            ) => single_path_name(path).is_some_and(|name| {
                generic_const_parameter(parameters, name)
                    && bind_const_argument(environment, name, actual)
            }),
            _ => false,
        }
    }

    fn validate_bounds(
        &mut self,
        parameters: &[ast::GenericParameter],
        where_predicates: &[ast::WherePredicate],
        environment: &GenericEnvironment,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> bool {
        let mut valid = true;
        for parameter in parameters {
            match parameter {
                ast::GenericParameter::Type { name, bounds, .. } => {
                    let Some(ty) = environment.types.get(&name.name).copied() else {
                        continue;
                    };
                    for bound in bounds {
                        valid &= self.validate_bound(ty, bound, span, diagnostics);
                    }
                }
                ast::GenericParameter::TypePack { name, bounds, .. } => {
                    let Some(types) = environment.type_packs.get(&name.name) else {
                        continue;
                    };
                    for ty in types {
                        for bound in bounds {
                            valid &= self.validate_bound(*ty, bound, span, diagnostics);
                        }
                    }
                }
                ast::GenericParameter::Const { .. } => {}
            }
        }
        for predicate in where_predicates {
            if let TypeNameKind::PackExpansion { pack, template } = &predicate.ty.kind {
                let Some(types) = environment.type_packs.get(&pack.name).cloned() else {
                    diagnostics.push(Diagnostic::error(
                        "E6020",
                        format!("unknown type pack `{}`", pack.name),
                        pack.span,
                    ));
                    valid = false;
                    continue;
                };
                for pack_type in types {
                    let ty = if let Some(template) = template {
                        let mut element_environment = environment.clone();
                        element_environment
                            .types
                            .insert(pack.name.clone(), pack_type);
                        let Some(ty) =
                            self.resolve_type_name_in(template, &element_environment, diagnostics)
                        else {
                            valid = false;
                            continue;
                        };
                        ty
                    } else {
                        pack_type
                    };
                    for bound in &predicate.bounds {
                        valid &= self.validate_bound(ty, bound, predicate.span, diagnostics);
                    }
                }
                continue;
            }
            let Some(ty) = self.resolve_type_name_in(&predicate.ty, environment, diagnostics)
            else {
                valid = false;
                continue;
            };
            for bound in &predicate.bounds {
                valid &= self.validate_bound(ty, bound, predicate.span, diagnostics);
            }
        }
        valid
    }

    fn validate_bound(
        &self,
        ty: Type,
        bound: &ast::Path,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> bool {
        let Some(name) = single_path_name(bound) else {
            diagnostics.push(Diagnostic::error(
                "E6014",
                "trait bound must resolve to one trait",
                bound.span,
            ));
            return false;
        };
        if !is_builtin_trait(name) && !self.traits.contains_key(name) {
            diagnostics.push(Diagnostic::error(
                "E6014",
                format!("unknown trait bound `{name}`"),
                bound.span,
            ));
            return false;
        }
        if self.satisfies_trait(ty, name) {
            true
        } else {
            diagnostics.push(
                Diagnostic::error(
                    "E6014",
                    format!("type `{ty}` does not satisfy trait bound `{name}`"),
                    span,
                )
                .with_help("add a matching trait implementation or use a compatible type"),
            );
            false
        }
    }

    fn definition(&self, ty: Type) -> Option<&hir::TypeDefinition> {
        let (Type::Struct(id)
        | Type::Enum(id)
        | Type::Tuple(id)
        | Type::Array(id)
        | Type::Reference(id)
        | Type::RawPointer(id)
        | Type::Slice(id)
        | Type::Function(id)) = ty
        else {
            return None;
        };
        self.definitions.get(usize::try_from(id.0).ok()?)
    }

    fn pointer_shape(&self, ty: Type) -> Option<(Type, bool, bool)> {
        let definition = self.definition(ty)?;
        match definition.kind {
            hir::TypeDefinitionKind::Reference { target, mutable } => {
                Some((target, mutable, false))
            }
            hir::TypeDefinitionKind::RawPointer { target, mutable } => {
                Some((target, mutable, true))
            }
            _ => None,
        }
    }

    fn slice_shape(&self, ty: Type) -> Option<(Type, bool)> {
        let definition = self.definition(ty)?;
        let hir::TypeDefinitionKind::Slice { element, mutable } = definition.kind else {
            return None;
        };
        Some((element, mutable))
    }

    fn function_shape(&self, ty: Type) -> Option<(&[Type], Type)> {
        let definition = self.definition(ty)?;
        let hir::TypeDefinitionKind::Function {
            parameters,
            return_type,
        } = &definition.kind
        else {
            return None;
        };
        Some((parameters, *return_type))
    }

    fn intrinsic(&self, ty: Type) -> Option<IntrinsicType> {
        self.intrinsics.get(&ty).copied()
    }

    fn is_copy(&self, ty: Type) -> bool {
        self.is_intrinsically_copy(ty)
            || self.has_derived_trait(ty, hir::DerivedTrait::Copy)
            || self.has_trait_implementation(ty, "Copy", 0)
    }

    fn is_intrinsically_copy(&self, ty: Type) -> bool {
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::I128
            | Type::Isize
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::U128
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::Char
            | Type::RawPointer(_)
            | Type::Function(_)
            | Type::Str
            | Type::CStr
            | Type::Unit
            | Type::Never => true,
            Type::Reference(_) => self.definition(ty).is_some_and(|definition| {
                matches!(
                    definition.kind,
                    hir::TypeDefinitionKind::Reference { mutable: false, .. }
                )
            }),
            Type::Slice(_) => self.definition(ty).is_some_and(|definition| {
                matches!(
                    definition.kind,
                    hir::TypeDefinitionKind::Slice { mutable: false, .. }
                )
            }),
            Type::Tuple(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Tuple { elements } = &definition.kind else {
                    return false;
                };
                elements.iter().all(|element| self.is_copy(*element))
            }),
            Type::Array(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Array { element, .. } = definition.kind else {
                    return false;
                };
                self.is_copy(element)
            }),
            Type::Enum(_) if self.intrinsic(ty).is_some() => self.intrinsic_fields_are_copy(ty),
            Type::Struct(_) | Type::Enum(_) => false,
        }
    }

    fn satisfies_trait(&self, ty: Type, trait_name: &str) -> bool {
        self.satisfies_trait_at_depth(ty, trait_name, 0)
    }

    fn satisfies_trait_at_depth(&self, ty: Type, trait_name: &str, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        let builtin_satisfied = match trait_name {
            "Copy" => Some(
                self.is_intrinsically_copy(ty)
                    || self.has_derived_trait(ty, hir::DerivedTrait::Copy),
            ),
            "Clone" => Some(
                self.is_intrinsically_copy(ty)
                    || self.has_derived_trait(ty, hir::DerivedTrait::Clone),
            ),
            "Debug" => Some(self.is_debug_capable(ty)),
            "Eq" => Some(self.is_equality_capable(ty)),
            "Ord" | "Ordered" => Some(is_ordered_type(ty)),
            "Hash" => Some(self.is_hash_capable(ty)),
            "Default" => Some(self.is_default_capable(ty)),
            "Pod" => return self.is_pod_capable(ty),
            "Send" => {
                return self.satisfies_thread_safety_at_depth(ty, ThreadSafety::Send, depth);
            }
            "Sync" => {
                return self.satisfies_thread_safety_at_depth(ty, ThreadSafety::Sync, depth);
            }
            _ => None,
        };
        if builtin_satisfied == Some(true) {
            return true;
        }
        self.has_derived_marker_trait(ty, trait_name, depth)
            || self.has_trait_implementation(ty, trait_name, depth)
    }

    fn has_derived_marker_trait(&self, ty: Type, trait_name: &str, depth: usize) -> bool {
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        if !definition
            .marker_traits
            .iter()
            .any(|derived| derived == trait_name)
        {
            return false;
        }
        self.traits.get(trait_name).is_some_and(|definition| {
            definition.declaration.generic_parameters.is_empty()
                && definition.declaration.methods.is_empty()
                && definition.declaration.supertraits.iter().all(|supertrait| {
                    single_path_name(supertrait).is_some_and(|supertrait| {
                        self.satisfies_trait_at_depth(ty, supertrait, depth + 1)
                    })
                })
        })
    }

    fn satisfies_thread_safety_at_depth(
        &self,
        ty: Type,
        safety: ThreadSafety,
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        if let Some(safe) = self.standard_library_thread_safety(ty, safety, depth) {
            return safe;
        }
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::I128
            | Type::Isize
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::U128
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::Char
            | Type::Function(_)
            | Type::Str
            | Type::CStr
            | Type::Unit
            | Type::Never => true,
            Type::RawPointer(_) => false,
            Type::Reference(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Reference { target, mutable } = definition.kind else {
                    return false;
                };
                let required = if mutable && matches!(safety, ThreadSafety::Send) {
                    ThreadSafety::Send
                } else {
                    ThreadSafety::Sync
                };
                self.satisfies_thread_safety_at_depth(target, required, depth + 1)
            }),
            Type::Slice(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Slice { element, mutable } = definition.kind else {
                    return false;
                };
                let required = if mutable && matches!(safety, ThreadSafety::Send) {
                    ThreadSafety::Send
                } else {
                    ThreadSafety::Sync
                };
                self.satisfies_thread_safety_at_depth(element, required, depth + 1)
            }),
            Type::Struct(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Struct { fields } = &definition.kind else {
                    return false;
                };
                fields
                    .iter()
                    .all(|field| self.satisfies_thread_safety_at_depth(field.ty, safety, depth + 1))
            }),
            Type::Tuple(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Tuple { elements } = &definition.kind else {
                    return false;
                };
                elements.iter().all(|element| {
                    self.satisfies_thread_safety_at_depth(*element, safety, depth + 1)
                })
            }),
            Type::Array(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Array { element, .. } = definition.kind else {
                    return false;
                };
                self.satisfies_thread_safety_at_depth(element, safety, depth + 1)
            }),
            Type::Enum(_) => self.definition(ty).is_some_and(|definition| {
                let hir::TypeDefinitionKind::Enum { variants } = &definition.kind else {
                    return false;
                };
                variants.iter().all(|variant| match &variant.fields {
                    hir::EnumVariantFields::Unit => true,
                    hir::EnumVariantFields::Tuple(fields) => fields.iter().all(|field| {
                        self.satisfies_thread_safety_at_depth(*field, safety, depth + 1)
                    }),
                    hir::EnumVariantFields::Struct(fields) => fields.iter().all(|field| {
                        self.satisfies_thread_safety_at_depth(field.ty, safety, depth + 1)
                    }),
                })
            }),
        }
    }

    fn standard_library_thread_safety(
        &self,
        ty: Type,
        safety: ThreadSafety,
        depth: usize,
    ) -> Option<bool> {
        if self.nominal_name(ty) == Some(STANDARD_STRING_TYPE) {
            return Some(true);
        }
        let instance = self.generic_instance(ty)?;
        if !matches!(
            instance.base_name.as_str(),
            STANDARD_VEC_TYPE
                | STANDARD_HASH_MAP_TYPE
                | STANDARD_HASH_SET_TYPE
                | STANDARD_RING_BUFFER_TYPE
        ) {
            return None;
        }
        Some(instance.arguments.iter().all(|argument| match argument {
            GenericValue::Type(argument) => {
                self.satisfies_thread_safety_at_depth(*argument, safety, depth + 1)
            }
            GenericValue::Const(_) => true,
        }))
    }

    fn has_trait_implementation(&self, ty: Type, trait_name: &str, depth: usize) -> bool {
        self.trait_implementations.iter().any(|implementation| {
            if implementation.trait_name != trait_name {
                return false;
            }
            let mut environment = self.base_environment();
            if !self.infer_type_pattern(
                &implementation.target,
                ty,
                &implementation.parameters,
                &mut environment,
            ) {
                return false;
            }
            let parameter_bounds_hold = implementation.parameters.iter().all(|parameter| {
                let ast::GenericParameter::Type { name, bounds, .. } = parameter else {
                    return true;
                };
                let Some(actual) = environment.types.get(&name.name).copied() else {
                    return false;
                };
                bounds.iter().all(|bound| {
                    single_path_name(bound).is_some_and(|bound| {
                        self.satisfies_trait_at_depth(actual, bound, depth + 1)
                    })
                })
            });
            let supertraits_hold = self.traits.get(trait_name).is_none_or(|definition| {
                definition.declaration.supertraits.iter().all(|supertrait| {
                    single_path_name(supertrait).is_some_and(|supertrait| {
                        self.satisfies_trait_at_depth(ty, supertrait, depth + 1)
                    })
                })
            });
            parameter_bounds_hold
                && supertraits_hold
                && implementation.where_predicates.iter().all(|predicate| {
                    let Some(subject) = self.resolve_existing_type(&predicate.ty, &environment)
                    else {
                        return false;
                    };
                    predicate.bounds.iter().all(|bound| {
                        single_path_name(bound).is_some_and(|bound| {
                            self.satisfies_trait_at_depth(subject, bound, depth + 1)
                        })
                    })
                })
        })
    }

    fn resolve_existing_type(
        &self,
        type_name: &ast::TypeName,
        environment: &GenericEnvironment,
    ) -> Option<Type> {
        match &type_name.kind {
            TypeNameKind::Function {
                parameters,
                return_type,
            } => {
                let parameters = parameters
                    .iter()
                    .map(|parameter| self.resolve_existing_type(parameter, environment))
                    .collect::<Option<Vec<_>>>()?;
                let return_type = self.resolve_existing_type(return_type, environment)?;
                self.structural
                    .get(&StructuralType::Function {
                        parameters,
                        return_type,
                    })
                    .copied()
            }
            TypeNameKind::Path(path) => {
                let name = single_path_name(path)?;
                environment
                    .types
                    .get(name)
                    .copied()
                    .or_else(|| primitive_type(name))
                    .or_else(|| self.names.get(name).copied())
            }
            TypeNameKind::Generic { path, arguments } => {
                let name = single_path_name(path)?;
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    match argument {
                        ast::GenericArgument::Type(ty) => {
                            if let TypeNameKind::Path(path) = &ty.kind
                                && let Some(name) = single_path_name(path)
                                && let Some(value) = environment.constants.get(name)
                            {
                                values.push(GenericValue::Const(*value));
                            } else {
                                values.push(GenericValue::Type(
                                    self.resolve_existing_type(ty, environment)?,
                                ));
                            }
                        }
                        ast::GenericArgument::Const(value) => {
                            let mut diagnostics = Vec::new();
                            values.push(GenericValue::Const(evaluate_array_length_in(
                                value,
                                environment,
                                &mut diagnostics,
                            )?));
                        }
                        ast::GenericArgument::Pack { pack, template, .. } => {
                            for ty in environment.type_packs.get(&pack.name)? {
                                if let Some(template) = template {
                                    let mut element_environment = environment.clone();
                                    element_environment.types.insert(pack.name.clone(), *ty);
                                    values.push(GenericValue::Type(
                                        self.resolve_existing_type(template, &element_environment)?,
                                    ));
                                } else {
                                    values.push(GenericValue::Type(*ty));
                                }
                            }
                        }
                    }
                }
                self.generic_instances
                    .get(&GenericTypeKey {
                        name: name.to_owned(),
                        arguments: values,
                    })
                    .copied()
            }
            TypeNameKind::Unit => Some(Type::Unit),
            TypeNameKind::PackExpansion { .. }
            | TypeNameKind::Tuple(_)
            | TypeNameKind::Array { .. }
            | TypeNameKind::Slice(_)
            | TypeNameKind::Reference { .. }
            | TypeNameKind::RawPointer { .. } => None,
        }
    }

    fn is_equality_capable(&self, ty: Type) -> bool {
        if is_equality_type(ty) {
            return true;
        }
        if self.has_derived_trait(ty, hir::DerivedTrait::Eq) {
            return true;
        }
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        match &definition.kind {
            hir::TypeDefinitionKind::Tuple { elements } => elements
                .iter()
                .all(|element| self.is_equality_capable(*element)),
            hir::TypeDefinitionKind::Array { element, .. } => self.is_equality_capable(*element),
            hir::TypeDefinitionKind::Reference { .. }
            | hir::TypeDefinitionKind::RawPointer { .. }
            | hir::TypeDefinitionKind::Slice { .. }
            | hir::TypeDefinitionKind::Function { .. }
            | hir::TypeDefinitionKind::Struct { .. }
            | hir::TypeDefinitionKind::Enum { .. } => false,
        }
    }

    fn is_hash_capable(&self, ty: Type) -> bool {
        self.has_derived_trait(ty, hir::DerivedTrait::Hash)
            || (is_equality_type(ty) && !ty.is_float())
    }

    fn is_debug_capable(&self, ty: Type) -> bool {
        matches!(
            ty,
            Type::I8
                | Type::I16
                | Type::I32
                | Type::I64
                | Type::I128
                | Type::Isize
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
                | Type::U128
                | Type::Usize
                | Type::F32
                | Type::F64
                | Type::Bool
                | Type::Char
                | Type::Str
                | Type::CStr
                | Type::Unit
                | Type::RawPointer(_)
                | Type::Reference(_)
                | Type::Function(_)
        ) || self.has_derived_trait(ty, hir::DerivedTrait::Debug)
            || self
                .definition(ty)
                .is_some_and(|definition| match &definition.kind {
                    hir::TypeDefinitionKind::Tuple { elements } => elements
                        .iter()
                        .all(|element| self.is_debug_capable(*element)),
                    hir::TypeDefinitionKind::Array { element, .. }
                    | hir::TypeDefinitionKind::Slice { element, .. } => {
                        self.is_debug_capable(*element)
                    }
                    hir::TypeDefinitionKind::Struct { .. }
                    | hir::TypeDefinitionKind::Enum { .. }
                    | hir::TypeDefinitionKind::Reference { .. }
                    | hir::TypeDefinitionKind::RawPointer { .. }
                    | hir::TypeDefinitionKind::Function { .. } => false,
                })
    }

    fn is_default_capable(&self, ty: Type) -> bool {
        matches!(
            ty,
            Type::I8
                | Type::I16
                | Type::I32
                | Type::I64
                | Type::I128
                | Type::Isize
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
                | Type::U128
                | Type::Usize
                | Type::F32
                | Type::F64
                | Type::Bool
                | Type::Char
                | Type::Str
                | Type::CStr
                | Type::Unit
        ) || self.has_derived_trait(ty, hir::DerivedTrait::Default)
            || self
                .definition(ty)
                .is_some_and(|definition| match &definition.kind {
                    hir::TypeDefinitionKind::Tuple { elements } => elements
                        .iter()
                        .all(|element| self.is_default_capable(*element)),
                    hir::TypeDefinitionKind::Array { element, .. } => {
                        self.is_default_capable(*element)
                    }
                    hir::TypeDefinitionKind::Struct { .. }
                    | hir::TypeDefinitionKind::Enum { .. }
                    | hir::TypeDefinitionKind::Reference { .. }
                    | hir::TypeDefinitionKind::RawPointer { .. }
                    | hir::TypeDefinitionKind::Slice { .. }
                    | hir::TypeDefinitionKind::Function { .. } => false,
                })
    }

    fn has_derived_trait(&self, ty: Type, derived: hir::DerivedTrait) -> bool {
        self.definition(ty)
            .is_some_and(|definition| definition.derives.contains(&derived))
    }

    fn default_expression(&self, ty: Type, span: Span) -> Option<Expression> {
        self.default_expression_at_depth(ty, span, 0)
    }

    fn default_expression_at_depth(
        &self,
        ty: Type,
        span: Span,
        depth: usize,
    ) -> Option<Expression> {
        if depth > 64 {
            return None;
        }
        let kind = match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::I128
            | Type::Isize
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::U128
            | Type::Usize => ExpressionKind::Integer(0),
            Type::F32 => ExpressionKind::Float32(0_f32.to_bits()),
            Type::F64 => ExpressionKind::Float64(0_f64.to_bits()),
            Type::Bool => ExpressionKind::Boolean(false),
            Type::Char => ExpressionKind::Character('\0'),
            Type::Str => ExpressionKind::String(String::new()),
            Type::CStr => ExpressionKind::CString(String::new()),
            Type::Unit => ExpressionKind::Unit,
            Type::Never
            | Type::Reference(_)
            | Type::RawPointer(_)
            | Type::Slice(_)
            | Type::Function(_) => return None,
            Type::Struct(_) => {
                let definition = self.definition(ty)?;
                let hir::TypeDefinitionKind::Struct { fields } = &definition.kind else {
                    return None;
                };
                ExpressionKind::Struct(
                    fields
                        .iter()
                        .map(|field| self.default_expression_at_depth(field.ty, span, depth + 1))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            Type::Tuple(_) => {
                let definition = self.definition(ty)?;
                let hir::TypeDefinitionKind::Tuple { elements } = &definition.kind else {
                    return None;
                };
                ExpressionKind::Tuple(
                    elements
                        .iter()
                        .map(|element| self.default_expression_at_depth(*element, span, depth + 1))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            Type::Array(_) => {
                let definition = self.definition(ty)?;
                let hir::TypeDefinitionKind::Array { element, length } = definition.kind else {
                    return None;
                };
                let length = usize::try_from(length).ok()?;
                let value = self.default_expression_at_depth(element, span, depth + 1)?;
                ExpressionKind::Array(vec![value; length])
            }
            Type::Enum(_) => {
                let definition = self.definition(ty)?;
                let hir::TypeDefinitionKind::Enum { variants } = &definition.kind else {
                    return None;
                };
                let variant = variants.first()?;
                let field_types = match &variant.fields {
                    hir::EnumVariantFields::Unit => Vec::new(),
                    hir::EnumVariantFields::Tuple(fields) => fields.clone(),
                    hir::EnumVariantFields::Struct(fields) => {
                        fields.iter().map(|field| field.ty).collect()
                    }
                };
                let fields = field_types
                    .into_iter()
                    .map(|field| self.default_expression_at_depth(field, span, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                ExpressionKind::Enum { variant: 0, fields }
            }
        };
        Some(Expression { kind, ty, span })
    }

    fn validate_type_derives(&self, ty: Type, diagnostics: &mut Vec<Diagnostic>) {
        let Some(definition) = self.definition(ty) else {
            return;
        };
        for derived in &definition.derives {
            if self.derived_requirements_hold(ty, *derived) {
                continue;
            }
            let requirement = match derived {
                hir::DerivedTrait::Copy => "every stored field must satisfy `Copy`",
                hir::DerivedTrait::Clone => {
                    "every stored field must be allocation-free and satisfy `Copy`"
                }
                hir::DerivedTrait::Debug => "every stored field must satisfy `Debug`",
                hir::DerivedTrait::Eq => "every stored field must satisfy `Eq`",
                hir::DerivedTrait::Hash => "every stored field must satisfy `Hash`",
                hir::DerivedTrait::Default => {
                    "every struct field, or every field of the first enum variant, must satisfy `Default`"
                }
                hir::DerivedTrait::Pod => {
                    "use `@repr(C)` on a padding-free struct whose fields all satisfy `Pod`"
                }
            };
            diagnostics.push(
                Diagnostic::error(
                    "E7006",
                    format!(
                        "cannot derive `{}` for `{}`",
                        derived.name(),
                        definition.name.as_deref().unwrap_or("this type")
                    ),
                    definition.span,
                )
                .with_help(requirement),
            );
        }
        for marker in &definition.marker_traits {
            let Some(marker_definition) = self.traits.get(marker) else {
                continue;
            };
            if !marker_definition.declaration.generic_parameters.is_empty()
                || !marker_definition.declaration.methods.is_empty()
            {
                continue;
            }
            let missing = marker_definition
                .declaration
                .supertraits
                .iter()
                .filter_map(single_path_name)
                .filter(|supertrait| !self.satisfies_trait(ty, supertrait))
                .collect::<Vec<_>>();
            if missing.is_empty() {
                continue;
            }
            diagnostics.push(
                Diagnostic::error(
                    "E7006",
                    format!(
                        "cannot derive `{marker}` for `{}`",
                        definition.name.as_deref().unwrap_or("this type")
                    ),
                    definition.span,
                )
                .with_help(format!(
                    "satisfy the marker trait requirements: {}",
                    missing
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            );
        }
    }

    fn derived_requirements_hold(&self, ty: Type, derived: hir::DerivedTrait) -> bool {
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        if derived == hir::DerivedTrait::Default {
            return match &definition.kind {
                hir::TypeDefinitionKind::Struct { fields } => {
                    fields.iter().all(|field| self.is_default_capable(field.ty))
                }
                hir::TypeDefinitionKind::Enum { variants } => {
                    variants.first().is_some_and(|variant| {
                        Self::variant_fields_satisfy(variant, |field| {
                            self.is_default_capable(field)
                        })
                    })
                }
                _ => false,
            };
        }
        if derived == hir::DerivedTrait::Pod {
            return self.pod_requirements_hold(ty);
        }
        Self::stored_fields_satisfy(definition, |field| match derived {
            hir::DerivedTrait::Copy | hir::DerivedTrait::Clone => self.is_copy(field),
            hir::DerivedTrait::Debug => self.is_debug_capable(field),
            hir::DerivedTrait::Eq => self.is_equality_capable(field),
            hir::DerivedTrait::Hash => self.is_hash_capable(field),
            hir::DerivedTrait::Default => false,
            hir::DerivedTrait::Pod => self.is_pod_capable(field),
        })
    }

    fn is_pod_capable(&self, ty: Type) -> bool {
        if matches!(
            ty,
            Type::I8
                | Type::I16
                | Type::I32
                | Type::I64
                | Type::I128
                | Type::Isize
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
                | Type::U128
                | Type::Usize
                | Type::F32
                | Type::F64
        ) {
            return true;
        }
        match ty {
            Type::Array(_) => {
                let Some(definition) = self.definition(ty) else {
                    return false;
                };
                let hir::TypeDefinitionKind::Array { element, .. } = definition.kind else {
                    return false;
                };
                self.is_pod_capable(element)
            }
            Type::Struct(_) => self.has_derived_trait(ty, hir::DerivedTrait::Pod),
            _ => false,
        }
    }

    fn pod_requirements_hold(&self, ty: Type) -> bool {
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        if definition.representation != hir::TypeRepresentation::C
            || !self.has_derived_trait(ty, hir::DerivedTrait::Copy)
        {
            return false;
        }
        let hir::TypeDefinitionKind::Struct { fields } = &definition.kind else {
            return false;
        };
        if !fields.iter().all(|field| self.is_pod_capable(field.ty)) {
            return false;
        }
        let Ok(layouts) = Layouts::build(&self.definitions) else {
            return false;
        };
        let Ok(aggregate) = layouts.aggregate(ty) else {
            return false;
        };
        let reimer_layout::AggregateLayoutKind::Product { offsets } = &aggregate.kind else {
            return false;
        };
        let mut cursor = 0_u32;
        for (field, offset) in fields.iter().zip(offsets) {
            if *offset != cursor {
                return false;
            }
            let Ok(field_layout) = layouts.value_layout(field.ty) else {
                return false;
            };
            let Some(next) = cursor.checked_add(field_layout.size) else {
                return false;
            };
            cursor = next;
        }
        cursor == aggregate.value.size
    }

    fn stored_fields_satisfy(
        definition: &hir::TypeDefinition,
        mut predicate: impl FnMut(Type) -> bool,
    ) -> bool {
        match &definition.kind {
            hir::TypeDefinitionKind::Struct { fields } => {
                fields.iter().all(|field| predicate(field.ty))
            }
            hir::TypeDefinitionKind::Enum { variants } => variants
                .iter()
                .all(|variant| Self::variant_fields_satisfy(variant, &mut predicate)),
            _ => false,
        }
    }

    fn variant_fields_satisfy(
        variant: &hir::EnumVariant,
        mut predicate: impl FnMut(Type) -> bool,
    ) -> bool {
        match &variant.fields {
            hir::EnumVariantFields::Unit => true,
            hir::EnumVariantFields::Tuple(fields) => fields.iter().all(|field| predicate(*field)),
            hir::EnumVariantFields::Struct(fields) => {
                fields.iter().all(|field| predicate(field.ty))
            }
        }
    }

    fn intrinsic_fields_are_copy(&self, ty: Type) -> bool {
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        let hir::TypeDefinitionKind::Enum { variants } = &definition.kind else {
            return false;
        };
        variants.iter().all(|variant| match &variant.fields {
            hir::EnumVariantFields::Unit => true,
            hir::EnumVariantFields::Tuple(fields) => {
                fields.iter().all(|field| self.is_copy(*field))
            }
            hir::EnumVariantFields::Struct(fields) => {
                fields.iter().all(|field| self.is_copy(field.ty))
            }
        })
    }

    fn is_mutable_view(&self, ty: Type) -> bool {
        self.definition(ty).is_some_and(|definition| {
            matches!(
                definition.kind,
                hir::TypeDefinitionKind::Reference { mutable: true, .. }
                    | hir::TypeDefinitionKind::Slice { mutable: true, .. }
            )
        })
    }

    fn is_scoped(&self, ty: Type) -> bool {
        self.is_scoped_at_depth(ty, 0)
    }

    fn is_scoped_at_depth(&self, ty: Type, depth: usize) -> bool {
        if depth > 64 {
            return true;
        }
        if matches!(
            ty,
            Type::Reference(_) | Type::Slice(_) | Type::Str | Type::CStr
        ) {
            return true;
        }
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        match &definition.kind {
            hir::TypeDefinitionKind::Struct { fields } => fields
                .iter()
                .any(|field| self.is_scoped_at_depth(field.ty, depth + 1)),
            hir::TypeDefinitionKind::Tuple { elements } => elements
                .iter()
                .any(|element| self.is_scoped_at_depth(*element, depth + 1)),
            hir::TypeDefinitionKind::Array { element, .. } => {
                self.is_scoped_at_depth(*element, depth + 1)
            }
            hir::TypeDefinitionKind::Enum { variants } => {
                variants.iter().any(|variant| match &variant.fields {
                    hir::EnumVariantFields::Unit => false,
                    hir::EnumVariantFields::Tuple(fields) => fields
                        .iter()
                        .any(|field| self.is_scoped_at_depth(*field, depth + 1)),
                    hir::EnumVariantFields::Struct(fields) => fields
                        .iter()
                        .any(|field| self.is_scoped_at_depth(field.ty, depth + 1)),
                })
            }
            hir::TypeDefinitionKind::Reference { .. } | hir::TypeDefinitionKind::Slice { .. } => {
                true
            }
            hir::TypeDefinitionKind::RawPointer { .. }
            | hir::TypeDefinitionKind::Function { .. } => false,
        }
    }

    fn supports_static_storage(&self, ty: Type) -> bool {
        self.supports_static_storage_at_depth(ty, 0)
    }

    fn supports_static_storage_at_depth(&self, ty: Type, depth: usize) -> bool {
        if depth > 64 || matches!(ty, Type::Never | Type::Str | Type::CStr) {
            return false;
        }
        let Some(definition) = self.definition(ty) else {
            return true;
        };
        match &definition.kind {
            hir::TypeDefinitionKind::Struct { fields } => fields
                .iter()
                .all(|field| self.supports_static_storage_at_depth(field.ty, depth + 1)),
            hir::TypeDefinitionKind::Tuple { elements } => elements
                .iter()
                .all(|element| self.supports_static_storage_at_depth(*element, depth + 1)),
            hir::TypeDefinitionKind::Array { element, .. } => {
                self.supports_static_storage_at_depth(*element, depth + 1)
            }
            hir::TypeDefinitionKind::Enum { variants } => {
                variants.iter().all(|variant| match &variant.fields {
                    hir::EnumVariantFields::Unit => true,
                    hir::EnumVariantFields::Tuple(fields) => fields
                        .iter()
                        .all(|field| self.supports_static_storage_at_depth(*field, depth + 1)),
                    hir::EnumVariantFields::Struct(fields) => fields
                        .iter()
                        .all(|field| self.supports_static_storage_at_depth(field.ty, depth + 1)),
                })
            }
            hir::TypeDefinitionKind::Reference { .. }
            | hir::TypeDefinitionKind::RawPointer { .. }
            | hir::TypeDefinitionKind::Slice { .. }
            | hir::TypeDefinitionKind::Function { .. } => false,
        }
    }

    fn type_name_may_be_scoped(&self, type_name: &ast::TypeName) -> bool {
        self.type_name_may_be_scoped_at_depth(type_name, 0)
    }

    fn type_name_may_be_scoped_at_depth(&self, type_name: &ast::TypeName, depth: usize) -> bool {
        if depth > 64 {
            return true;
        }
        match &type_name.kind {
            TypeNameKind::Reference { .. } | TypeNameKind::Slice(_) => true,
            TypeNameKind::Path(path) => {
                single_path_name(path).is_some_and(|name| matches!(name, "str" | "cstr"))
            }
            TypeNameKind::Generic { path, arguments } => {
                if arguments.iter().any(|argument| match argument {
                    ast::GenericArgument::Type(ty) => {
                        self.type_name_may_be_scoped_at_depth(ty, depth + 1)
                    }
                    ast::GenericArgument::Const(_) => false,
                    ast::GenericArgument::Pack { template, .. } => {
                        template.as_ref().is_some_and(|template| {
                            self.type_name_may_be_scoped_at_depth(template, depth + 1)
                        })
                    }
                }) {
                    return true;
                }
                let Some(name) = single_path_name(path) else {
                    return false;
                };
                let Some(template) = self.generic_templates.get(name) else {
                    return false;
                };
                match template {
                    GenericTypeTemplate::Struct(declaration) => declaration
                        .fields
                        .iter()
                        .any(|field| self.type_name_may_be_scoped_at_depth(&field.ty, depth + 1)),
                    GenericTypeTemplate::Enum(declaration) => {
                        declaration
                            .variants
                            .iter()
                            .any(|variant| match &variant.payload {
                                ast::EnumVariantPayload::Unit => false,
                                ast::EnumVariantPayload::Tuple(fields) => {
                                    fields.iter().any(|field| {
                                        self.type_name_may_be_scoped_at_depth(field, depth + 1)
                                    })
                                }
                                ast::EnumVariantPayload::Struct(fields) => {
                                    fields.iter().any(|field| {
                                        self.type_name_may_be_scoped_at_depth(&field.ty, depth + 1)
                                    })
                                }
                            })
                    }
                }
            }
            TypeNameKind::Tuple(elements) => elements
                .iter()
                .any(|element| self.type_name_may_be_scoped_at_depth(element, depth + 1)),
            TypeNameKind::PackExpansion { template, .. } => template
                .as_ref()
                .is_some_and(|template| self.type_name_may_be_scoped_at_depth(template, depth + 1)),
            TypeNameKind::Array { element, .. } => {
                self.type_name_may_be_scoped_at_depth(element, depth + 1)
            }
            TypeNameKind::Function { .. }
            | TypeNameKind::Unit
            | TypeNameKind::RawPointer { .. } => false,
        }
    }

    fn is_ffi_safe_type(&self, ty: Type) -> bool {
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::Isize
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::CStr => true,
            Type::RawPointer(_) => self.ffi_pointer_target(ty).is_some_and(|target| {
                target == Type::Unit
                    || self.is_ffi_safe_type(target)
                    || self.is_c_representation_struct(target)
            }),
            Type::Function(_) => {
                self.function_shape(ty)
                    .is_some_and(|(parameters, return_type)| {
                        parameters
                            .iter()
                            .all(|parameter| self.is_ffi_safe_type(*parameter))
                            && self.is_ffi_safe_return_type(return_type)
                    })
            }
            Type::I128
            | Type::U128
            | Type::Char
            | Type::Struct(_)
            | Type::Enum(_)
            | Type::Tuple(_)
            | Type::Array(_)
            | Type::Reference(_)
            | Type::Slice(_)
            | Type::Str
            | Type::Unit
            | Type::Never => false,
        }
    }

    fn is_ffi_safe_extern_value(&self, ty: Type) -> bool {
        self.is_ffi_safe_type(ty) || self.is_ffi_safe_c_struct(ty)
    }

    fn is_ffi_safe_c_struct(&self, ty: Type) -> bool {
        let Some(definition) = self.definition(ty) else {
            return false;
        };
        let hir::TypeDefinitionKind::Struct { fields } = &definition.kind else {
            return false;
        };
        definition.representation == hir::TypeRepresentation::C
            && !fields.is_empty()
            && fields
                .iter()
                .all(|field| self.is_ffi_safe_c_field(field.ty))
    }

    fn is_ffi_safe_c_field(&self, ty: Type) -> bool {
        if self.is_ffi_safe_type(ty) {
            return true;
        }
        match ty {
            Type::Struct(_) => self.is_ffi_safe_c_struct(ty),
            Type::Array(_) => self
                .definition(ty)
                .and_then(|definition| match &definition.kind {
                    hir::TypeDefinitionKind::Array { element, .. } => Some(*element),
                    _ => None,
                })
                .is_some_and(|element| self.is_ffi_safe_c_field(element)),
            Type::I128
            | Type::U128
            | Type::Char
            | Type::Enum(_)
            | Type::Tuple(_)
            | Type::Reference(_)
            | Type::Slice(_)
            | Type::Str
            | Type::Unit
            | Type::Never
            | Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::Isize
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::CStr
            | Type::RawPointer(_)
            | Type::Function(_) => false,
        }
    }

    fn is_ffi_safe_return_type(&self, ty: Type) -> bool {
        ty == Type::Unit || self.is_ffi_safe_extern_value(ty)
    }

    fn ffi_pointer_target(&self, ty: Type) -> Option<Type> {
        let definition = self.definition(ty)?;
        let hir::TypeDefinitionKind::RawPointer { target, .. } = definition.kind else {
            return None;
        };
        Some(target)
    }

    fn is_c_representation_struct(&self, ty: Type) -> bool {
        self.definition(ty).is_some_and(|definition| {
            matches!(ty, Type::Struct(_)) && definition.representation == hir::TypeRepresentation::C
        })
    }

    fn nominal_name(&self, ty: Type) -> Option<&str> {
        if let Some((target, _, false)) = self.pointer_shape(ty) {
            return self.nominal_name(target);
        }
        self.definition(ty)?.name.as_deref()
    }

    fn reflection_type_name(&self, ty: Type) -> String {
        let Some(definition) = self.definition(ty) else {
            return ty.to_string();
        };
        if let Some(name) = &definition.name {
            return name.clone();
        }
        match &definition.kind {
            hir::TypeDefinitionKind::Tuple { elements } => format!(
                "({})",
                elements
                    .iter()
                    .map(|element| self.reflection_type_name(*element))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            hir::TypeDefinitionKind::Array { element, length } => {
                format!("[{}; {length}]", self.reflection_type_name(*element))
            }
            hir::TypeDefinitionKind::Reference { target, mutable } => {
                let modifier = if *mutable { "mut " } else { "" };
                format!("&{modifier}{}", self.reflection_type_name(*target))
            }
            hir::TypeDefinitionKind::RawPointer { target, mutable } => {
                let modifier = if *mutable { "mut" } else { "const" };
                format!("*{modifier} {}", self.reflection_type_name(*target))
            }
            hir::TypeDefinitionKind::Slice { element, mutable } => {
                let modifier = if *mutable { "mut " } else { "" };
                format!("&{modifier}[{}]", self.reflection_type_name(*element))
            }
            hir::TypeDefinitionKind::Function {
                parameters,
                return_type,
            } => format!(
                "fn({}) -> {}",
                parameters
                    .iter()
                    .map(|parameter| self.reflection_type_name(*parameter))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.reflection_type_name(*return_type)
            ),
            hir::TypeDefinitionKind::Struct { .. } => "struct".to_owned(),
            hir::TypeDefinitionKind::Enum { .. } => "enum".to_owned(),
        }
    }
}

struct ReflectionMetadata<'types> {
    types: &'types mut TypeRegistry,
}

impl comptime::Metadata for ReflectionMetadata<'_> {
    fn evaluate(
        &mut self,
        path: &ast::Path,
        arguments: &[ast::GenericArgument],
        type_bindings: &HashMap<String, ast::TypeName>,
    ) -> comptime::IntrinsicResult {
        let name = path.display();
        let intrinsic = match name.as_str() {
            "size_of" | "align_of" | "fields" | "variants" => name.as_str(),
            "meta::name" => "name",
            "meta::fields" => "fields",
            "meta::variants" => "variants",
            "meta::traits" => "traits",
            _ => return comptime::IntrinsicResult::NotFound,
        };
        let [ast::GenericArgument::Type(requested)] = arguments else {
            return comptime::IntrinsicResult::Error {
                message: format!("`{name}` expects exactly one type argument"),
                help: "write the reflection call as `name<Type>()`",
            };
        };
        let mut environment = self.types.base_environment();
        let mut diagnostics = Vec::new();
        for (binding, type_name) in type_bindings {
            if let Some(ty) =
                self.types
                    .resolve_type_name_in(type_name, &environment, &mut diagnostics)
            {
                environment.types.insert(binding.clone(), ty);
            }
        }
        let Some(ty) = self
            .types
            .resolve_type_name_in(requested, &environment, &mut diagnostics)
        else {
            let message = diagnostics.into_iter().next().map_or_else(
                || "reflection type could not be resolved".to_owned(),
                |error| error.message,
            );
            return comptime::IntrinsicResult::Error {
                message,
                help: "use a concrete type known at this compile-time call site",
            };
        };
        match intrinsic {
            "size_of" | "align_of" => {
                let layout = match Layouts::build(&self.types.definitions)
                    .and_then(|layouts| layouts.value_layout(ty))
                {
                    Ok(layout) => layout,
                    Err(message) => {
                        return comptime::IntrinsicResult::Error {
                            message,
                            help: "use a finite type with a valid native layout",
                        };
                    }
                };
                let value = if intrinsic == "size_of" {
                    layout.size
                } else {
                    layout.align
                };
                comptime::IntrinsicResult::Value(comptime::Value::Integer(comptime::Integer::from(
                    u128::from(value),
                )))
            }
            "name" => comptime::IntrinsicResult::Value(comptime::Value::String(
                self.types.reflection_type_name(ty),
            )),
            "fields" => self.reflect_fields(ty),
            "variants" => self.reflect_variants(ty),
            "traits" => {
                let mut candidates = [
                    "Copy", "Clone", "Debug", "Eq", "Ordered", "Hash", "Default", "Send", "Sync",
                    "Pod",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
                candidates.extend(self.types.traits.keys().cloned());
                let traits = candidates
                    .into_iter()
                    .filter(|name| self.types.satisfies_trait(ty, name))
                    .map(comptime::Value::String)
                    .collect();
                comptime::IntrinsicResult::Value(comptime::Value::Array(traits))
            }
            _ => comptime::IntrinsicResult::NotFound,
        }
    }
}

impl ReflectionMetadata<'_> {
    fn reflect_fields(&self, ty: Type) -> comptime::IntrinsicResult {
        let fields = match self.types.definition(ty).map(|definition| &definition.kind) {
            Some(hir::TypeDefinitionKind::Struct { fields }) => fields
                .iter()
                .map(|field| {
                    reflection_entry(&field.name, &self.types.reflection_type_name(field.ty))
                })
                .collect(),
            Some(hir::TypeDefinitionKind::Tuple { elements }) => elements
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    reflection_entry(&index.to_string(), &self.types.reflection_type_name(*ty))
                })
                .collect(),
            _ => {
                return comptime::IntrinsicResult::Error {
                    message: format!(
                        "type `{}` does not have fields",
                        self.types.reflection_type_name(ty)
                    ),
                    help: "use `meta::fields` with a struct or tuple type",
                };
            }
        };
        comptime::IntrinsicResult::Value(comptime::Value::Array(fields))
    }

    fn reflect_variants(&self, ty: Type) -> comptime::IntrinsicResult {
        let Some(hir::TypeDefinitionKind::Enum { variants }) =
            self.types.definition(ty).map(|definition| &definition.kind)
        else {
            return comptime::IntrinsicResult::Error {
                message: format!(
                    "type `{}` does not have enum variants",
                    self.types.reflection_type_name(ty)
                ),
                help: "use `meta::variants` with an enum type",
            };
        };
        let variants = variants
            .iter()
            .map(|variant| comptime::Value::String(variant.name.clone()))
            .collect();
        comptime::IntrinsicResult::Value(comptime::Value::Array(variants))
    }
}

fn reflection_entry(name: &str, ty: &str) -> comptime::Value {
    comptime::Value::Record(
        [
            ("name".to_owned(), comptime::Value::String(name.to_owned())),
            ("type".to_owned(), comptime::Value::String(ty.to_owned())),
        ]
        .into_iter()
        .collect(),
    )
}

struct Resolver {
    signatures: HashMap<String, Signature>,
    statics: HashMap<String, StaticSymbol>,
    generic_functions: GenericFunctionRegistry,
    types: TypeRegistry,
    diagnostics: Vec<Diagnostic>,
    preliminary_constants: HashMap<String, comptime::EvaluatedConstant>,
}

impl Resolver {
    fn new() -> Self {
        Self {
            signatures: HashMap::new(),
            statics: HashMap::new(),
            generic_functions: GenericFunctionRegistry::default(),
            types: TypeRegistry::default(),
            diagnostics: Vec::new(),
            preliminary_constants: HashMap::new(),
        }
    }

    fn resolve(
        mut self,
        program: &ast::Program,
        require_entry: bool,
    ) -> Result<hir::Program, Vec<Diagnostic>> {
        self.validate_attributes(program);
        let mut unavailable_metadata = comptime::UnavailableMetadata;
        let preliminary = comptime::evaluate(
            program,
            &mut unavailable_metadata,
            HashMap::new(),
            false,
            false,
        );
        self.types
            .remember_preliminary_constants(&preliminary.constants);
        self.preliminary_constants = preliminary.constants;
        self.collect_type_headers(program);
        self.collect_type_aliases(program);
        self.collect_trait_headers(program);
        self.validate_derived_marker_traits(program);
        self.resolve_type_definitions(program);
        self.validate_type_cycles();
        self.validate_derived_traits();
        self.collect_trait_implementations(program);
        let compiletime_constants = self.evaluate_compiletime(program);
        let declarations = self.collect_declarations(program);
        let statics = self.collect_statics(program, &compiletime_constants);
        self.generic_functions.next_id = u32::try_from(declarations.len()).unwrap_or(u32::MAX);
        let entry = require_entry.then(|| self.validate_entry()).flatten();
        let (mut functions, mut extern_functions) = self.analyze_declarations(declarations);
        functions.sort_by_key(|function| function.id.0);
        extern_functions.sort_by_key(|function| function.id.0);
        let tests = functions
            .iter()
            .filter(|function| function.attributes.test)
            .map(|function| function.id)
            .collect();
        self.diagnostics.sort_by(|left, right| {
            left.span
                .start
                .cmp(&right.span.start)
                .then_with(|| left.span.end.cmp(&right.span.end))
                .then_with(|| left.code.cmp(right.code))
                .then_with(|| left.message.cmp(&right.message))
        });
        self.diagnostics.dedup_by(|left, right| {
            left.span == right.span && left.code == right.code && left.message == right.message
        });

        if self.diagnostics.is_empty() {
            Ok(hir::Program {
                types: self.types.definitions,
                functions,
                extern_functions,
                statics,
                entry,
                tests,
            })
        } else {
            Err(self.diagnostics)
        }
    }

    fn analyze_declarations(
        &mut self,
        declarations: Vec<Declaration<'_>>,
    ) -> (Vec<hir::Function>, Vec<hir::ExternFunction>) {
        let mut functions = Vec::with_capacity(declarations.len());
        let mut extern_functions = Vec::new();
        for declaration in declarations {
            match declaration {
                Declaration::Source {
                    function,
                    resolved_name,
                    signature,
                } => {
                    let environment = self.types.base_environment();
                    let mut analyzer = FunctionAnalyzer::new(
                        ValueSymbols {
                            functions: &mut self.signatures,
                            statics: &self.statics,
                        },
                        &mut self.generic_functions,
                        &mut self.types,
                        &mut self.diagnostics,
                        signature,
                        environment,
                        symbol_module_identity(&resolved_name).map(str::to_owned),
                    );
                    functions.push(analyzer.analyze(function, resolved_name));
                }
                Declaration::Extern {
                    function,
                    signature,
                } => {
                    let parameters = function
                        .parameters
                        .iter()
                        .zip(&signature.parameter_types)
                        .enumerate()
                        .filter_map(|(index, (parameter, ty))| {
                            Some(hir::Parameter {
                                local: LocalId(u32::try_from(index).ok()?),
                                name: parameter.name.name.clone(),
                                ty: *ty,
                                span: parameter.span,
                            })
                        })
                        .collect();
                    extern_functions.push(hir::ExternFunction {
                        id: signature.id,
                        name: function.name.name.clone(),
                        symbol: function.symbol.clone(),
                        link: function.link.clone(),
                        is_public: function.is_public,
                        abi: function.abi.clone(),
                        parameters,
                        return_type: signature.return_type,
                        span: function.span,
                    });
                }
            }
        }
        let mut pending_index = 0;
        while let Some(pending) = self.generic_functions.pending.get(pending_index).cloned() {
            pending_index += 1;
            let mut analyzer = FunctionAnalyzer::new(
                ValueSymbols {
                    functions: &mut self.signatures,
                    statics: &self.statics,
                },
                &mut self.generic_functions,
                &mut self.types,
                &mut self.diagnostics,
                pending.signature,
                pending.environment,
                pending.module_identity,
            );
            functions.push(analyzer.analyze(&pending.function, pending.resolved_name));
        }
        (functions, extern_functions)
    }

    fn validate_attributes(&mut self, program: &ast::Program) {
        let comptime_functions = program
            .items
            .iter()
            .filter_map(|item| {
                let Item::Function(function) = item else {
                    return None;
                };
                function.is_comptime.then_some(function.name.name.as_str())
            })
            .collect::<BTreeSet<_>>();
        for item in &program.items {
            match item {
                Item::Function(function) => {
                    validate_function_attributes(function, &mut self.diagnostics);
                    if function.is_comptime {
                        validate_comptime_function(
                            function,
                            &comptime_functions,
                            &mut self.diagnostics,
                        );
                    }
                }
                Item::Struct(declaration) => {
                    validate_type_attributes(&declaration.attributes, false, &mut self.diagnostics);
                }
                Item::Enum(declaration) => {
                    validate_type_attributes(&declaration.attributes, true, &mut self.diagnostics);
                }
                Item::Impl(implementation) => {
                    for method in &implementation.methods {
                        validate_function_attributes(method, &mut self.diagnostics);
                    }
                }
                Item::Import(_)
                | Item::ExternFunction(_)
                | Item::TypeAlias(_)
                | Item::Trait(_)
                | Item::Constant(_)
                | Item::Static(_)
                | Item::Comptime(_) => {}
            }
        }
    }

    fn evaluate_compiletime(
        &mut self,
        program: &ast::Program,
    ) -> HashMap<String, comptime::EvaluatedConstant> {
        let seed = std::mem::take(&mut self.preliminary_constants);
        let evaluation = {
            let mut metadata = ReflectionMetadata {
                types: &mut self.types,
            };
            comptime::evaluate(program, &mut metadata, seed, true, true)
        };
        self.diagnostics.extend(evaluation.diagnostics);

        let mut names = HashMap::new();
        for item in &program.items {
            let Item::Constant(declaration) = item else {
                continue;
            };
            if names
                .insert(declaration.name.name.as_str(), declaration.name.span)
                .is_some()
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E7007",
                        format!(
                            "constant `{}` is declared more than once",
                            declaration.name.name
                        ),
                        declaration.name.span,
                    )
                    .with_help("give each compile-time constant a unique name"),
                );
                continue;
            }
            let Some(evaluated) = evaluation.constants.get(&declaration.name.name) else {
                continue;
            };
            let Some(ty) = self
                .types
                .resolve_type_name(&declaration.ty, &mut self.diagnostics)
            else {
                continue;
            };
            if let Some(value) = self.types.lower_compiletime_value(
                &evaluated.value,
                ty,
                evaluated.span,
                &mut self.diagnostics,
            ) {
                self.types
                    .constants
                    .insert(declaration.name.name.clone(), value);
            }
        }
        evaluation.constants
    }

    fn collect_statics(
        &mut self,
        program: &ast::Program,
        constants: &HashMap<String, comptime::EvaluatedConstant>,
    ) -> Vec<hir::Static> {
        let mut resolved = Vec::new();
        for item in &program.items {
            let Item::Static(declaration) = item else {
                continue;
            };
            let name = declaration.name.name.clone();
            if self.statics.contains_key(&name)
                || self.types.constants.contains_key(&name)
                || self.signatures.contains_key(&name)
                || self.generic_functions.templates.contains_key(&name)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3155",
                        format!("static `{name}` conflicts with another value declaration"),
                        declaration.name.span,
                    )
                    .with_help("give the static a unique name"),
                );
                continue;
            }
            let Some(ty) = self
                .types
                .resolve_type_name(&declaration.ty, &mut self.diagnostics)
            else {
                continue;
            };
            if !self.types.supports_static_storage(ty) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3155",
                        format!("type `{ty}` cannot be stored in a static"),
                        declaration.ty.span,
                    )
                    .with_help(
                        "use an owned scalar or aggregate without references, slices, strings, raw pointers, or function values",
                    ),
                );
                continue;
            }
            let evaluation = {
                let mut metadata = ReflectionMetadata {
                    types: &mut self.types,
                };
                comptime::evaluate_initializer(
                    program,
                    &mut metadata,
                    constants.clone(),
                    &declaration.value,
                )
            };
            self.diagnostics.extend(evaluation.diagnostics);
            let Some(evaluated) = evaluation.value else {
                continue;
            };
            let Some(initializer) = self.types.lower_compiletime_value(
                &evaluated.value,
                ty,
                evaluated.span,
                &mut self.diagnostics,
            ) else {
                continue;
            };
            let Ok(index) = u32::try_from(resolved.len()) else {
                self.diagnostics.push(Diagnostic::error(
                    "E3999",
                    "this compilation unit contains too many statics",
                    declaration.span,
                ));
                continue;
            };
            let id = StaticId(index);
            let symbol = StaticSymbol {
                id,
                mutable: declaration.mutable,
                ty,
            };
            self.statics.insert(name.clone(), symbol);
            resolved.push(hir::Static {
                id,
                name,
                is_public: declaration.is_public,
                mutable: declaration.mutable,
                ty,
                initializer,
                documentation: None,
                span: declaration.span,
            });
        }
        resolved
    }

    fn collect_type_headers(&mut self, program: &ast::Program) {
        for item in &program.items {
            let (name, span, is_struct, repr_c, generic_template) = match item {
                Item::Struct(declaration) => (
                    declaration.name.name.as_str(),
                    declaration.span,
                    true,
                    has_identifier_attribute(&declaration.attributes, "repr", "C"),
                    (!declaration.generic_parameters.is_empty())
                        .then(|| GenericTypeTemplate::Struct(declaration.clone())),
                ),
                Item::Enum(declaration) => (
                    declaration.name.name.as_str(),
                    declaration.span,
                    false,
                    false,
                    (!declaration.generic_parameters.is_empty())
                        .then(|| GenericTypeTemplate::Enum(declaration.clone())),
                ),
                _ => continue,
            };
            if primitive_type(name).is_some()
                || self.types.names.contains_key(name)
                || self.types.generic_templates.contains_key(name)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3008",
                        format!("type `{name}` is declared more than once"),
                        span,
                    )
                    .with_help("give each type a unique name"),
                );
                continue;
            }
            if let Some(template) = generic_template {
                validate_generic_parameter_names(template.parameters(), &mut self.diagnostics);
                self.types
                    .generic_templates
                    .insert(name.to_owned(), template);
                continue;
            }
            let kind = if is_struct {
                hir::TypeDefinitionKind::Struct { fields: Vec::new() }
            } else {
                hir::TypeDefinitionKind::Enum {
                    variants: Vec::new(),
                }
            };
            let Some(id) = self
                .types
                .push_definition(Some(name.to_owned()), kind, span)
            else {
                self.diagnostics.push(Diagnostic::error(
                    "E3999",
                    "this compilation unit contains too many types",
                    span,
                ));
                continue;
            };
            if repr_c
                && let Some(definition) = self
                    .types
                    .definitions
                    .get_mut(usize::try_from(id.0).unwrap_or(usize::MAX))
            {
                definition.representation = hir::TypeRepresentation::C;
            }
            if let Some(definition) = self
                .types
                .definitions
                .get_mut(usize::try_from(id.0).unwrap_or(usize::MAX))
            {
                if let Item::Struct(declaration) = item {
                    definition.alignment = requested_alignment(&declaration.attributes);
                    definition.derives = derived_traits(&declaration.attributes);
                    definition.marker_traits = derived_marker_traits(&declaration.attributes);
                    definition.must_use = has_marker_attribute(&declaration.attributes, "must_use");
                } else if let Item::Enum(declaration) = item {
                    definition.alignment = requested_alignment(&declaration.attributes);
                    definition.derives = derived_traits(&declaration.attributes);
                    definition.marker_traits = derived_marker_traits(&declaration.attributes);
                    definition.must_use = has_marker_attribute(&declaration.attributes, "must_use");
                }
            }
            let ty = if is_struct {
                Type::Struct(id)
            } else {
                Type::Enum(id)
            };
            self.types.names.insert(name.to_owned(), ty);
        }
    }

    fn collect_type_aliases(&mut self, program: &ast::Program) {
        let mut pending = Vec::new();
        let mut seen = BTreeSet::new();
        for item in &program.items {
            let Item::TypeAlias(declaration) = item else {
                continue;
            };
            let name = declaration.name.name.as_str();
            if primitive_type(name).is_some()
                || self.types.names.contains_key(name)
                || self.types.generic_templates.contains_key(name)
                || !seen.insert(name.to_owned())
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3008",
                        format!("type `{name}` is declared more than once"),
                        declaration.name.span,
                    )
                    .with_help("give each type alias a unique name"),
                );
                continue;
            }
            pending.push(declaration);
        }

        while !pending.is_empty() {
            let mut unresolved = Vec::new();
            let mut progress = false;
            for declaration in pending {
                let mut attempt_diagnostics = Vec::new();
                if let Some(target) = self
                    .types
                    .resolve_type_name(&declaration.target, &mut attempt_diagnostics)
                {
                    self.types
                        .names
                        .insert(declaration.name.name.clone(), target);
                    progress = true;
                } else {
                    unresolved.push(declaration);
                }
            }
            if !progress {
                for declaration in unresolved {
                    let _ = self
                        .types
                        .resolve_type_name(&declaration.target, &mut self.diagnostics);
                }
                break;
            }
            pending = unresolved;
        }
    }

    fn collect_trait_headers(&mut self, program: &ast::Program) {
        for item in &program.items {
            let Item::Trait(declaration) = item else {
                continue;
            };
            let name = &declaration.name.name;
            if primitive_type(name).is_some()
                || self.types.names.contains_key(name)
                || self.types.generic_templates.contains_key(name)
                || self.types.traits.contains_key(name)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6010",
                        format!("trait `{name}` is declared more than once"),
                        declaration.name.span,
                    )
                    .with_help("give each trait a unique name"),
                );
                continue;
            }
            validate_generic_parameter_names(
                &declaration.generic_parameters,
                &mut self.diagnostics,
            );
            let mut methods = HashMap::new();
            for method in &declaration.methods {
                validate_generic_parameter_names(&method.generic_parameters, &mut self.diagnostics);
                if methods
                    .insert(method.name.name.as_str(), method.name.span)
                    .is_some()
                {
                    self.diagnostics.push(Diagnostic::error(
                        "E6010",
                        format!(
                            "trait method `{}` is declared more than once",
                            method.name.name
                        ),
                        method.name.span,
                    ));
                }
            }
            self.types.traits.insert(
                name.clone(),
                TraitDefinition {
                    declaration: declaration.clone(),
                },
            );
        }

        for definition in self.types.traits.values() {
            for supertrait in &definition.declaration.supertraits {
                let Some(name) = single_path_name(supertrait) else {
                    self.diagnostics.push(Diagnostic::error(
                        "E6010",
                        "supertrait must resolve to one declared trait",
                        supertrait.span,
                    ));
                    continue;
                };
                if !self.types.traits.contains_key(name) && !is_builtin_trait(name) {
                    self.diagnostics.push(Diagnostic::error(
                        "E6010",
                        format!("unknown supertrait `{name}`"),
                        supertrait.span,
                    ));
                }
            }
        }
    }

    fn validate_derived_marker_traits(&mut self, program: &ast::Program) {
        for item in &program.items {
            let attributes = match item {
                Item::Struct(declaration) => &declaration.attributes,
                Item::Enum(declaration) => &declaration.attributes,
                _ => continue,
            };
            for attribute in attributes {
                if attribute.name.name != "derive" {
                    continue;
                }
                for argument in &attribute.arguments {
                    let ast::AttributeArgument::Identifier(name) = argument else {
                        continue;
                    };
                    if derived_trait(&name.name).is_some() {
                        continue;
                    }
                    let Some(definition) = self.types.traits.get(&name.name) else {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E7004",
                                format!("unknown derive `{}`", name.name),
                                name.span,
                            )
                            .with_help(
                                "import a zero-method marker trait or use a built-in structural derive",
                            ),
                        );
                        continue;
                    };
                    if !definition.declaration.generic_parameters.is_empty() {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E7004",
                                format!("generic trait `{}` cannot be derived", name.name),
                                name.span,
                            )
                            .with_help(
                                "marker derives must name a trait without generic parameters",
                            ),
                        );
                    }
                    if !definition.declaration.methods.is_empty() {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E7004",
                                format!("trait `{}` is not a marker trait", name.name),
                                name.span,
                            )
                            .with_help(
                                "implement traits with methods explicitly; marker derives may not generate behavior",
                            ),
                        );
                    }
                }
            }
        }
    }

    fn collect_trait_implementations(&mut self, program: &ast::Program) {
        for item in &program.items {
            let Item::Impl(implementation) = item else {
                continue;
            };
            let Some(trait_type) = &implementation.trait_type else {
                continue;
            };
            let Some(trait_name) = type_constructor_name(trait_type).map(str::to_owned) else {
                self.diagnostics.push(Diagnostic::error(
                    "E6011",
                    "implemented trait must use a nominal trait path",
                    trait_type.span,
                ));
                continue;
            };
            let definition = self.types.traits.get(&trait_name).cloned();
            if definition.is_none() && !is_builtin_trait(&trait_name) {
                self.diagnostics.push(Diagnostic::error(
                    "E6011",
                    format!("unknown trait `{trait_name}`"),
                    trait_type.span,
                ));
                continue;
            }
            let argument_count = match &trait_type.kind {
                TypeNameKind::Generic { arguments, .. } => arguments.len(),
                _ => 0,
            };
            let expected_argument_count = definition.as_ref().map_or(0, |definition| {
                definition.declaration.generic_parameters.len()
            });
            if argument_count != expected_argument_count {
                self.diagnostics.push(Diagnostic::error(
                    "E6011",
                    format!("trait `{trait_name}` expects {expected_argument_count} generic argument(s), but {argument_count} were provided"),
                    trait_type.span,
                ));
            }
            let implementation_key = type_pattern_key(&implementation.target);
            if self.types.trait_implementations.iter().any(|existing| {
                existing.trait_name == trait_name
                    && type_pattern_key(&existing.target) == implementation_key
            }) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6012",
                        format!(
                            "trait `{trait_name}` is implemented more than once for `{implementation_key}`"
                        ),
                        implementation.span,
                    )
                    .with_help("keep one coherent implementation for each trait and target"),
                );
                continue;
            }
            if let Some(definition) = &definition {
                self.validate_trait_methods(&definition.declaration, implementation);
            } else if !implementation.methods.is_empty() {
                self.diagnostics.push(Diagnostic::error(
                    "E6013",
                    format!("built-in marker trait `{trait_name}` does not declare methods"),
                    implementation.span,
                ));
            }
            self.types.trait_implementations.push(TraitImplementation {
                trait_name,
                target: implementation.target.clone(),
                parameters: implementation.generic_parameters.clone(),
                where_predicates: implementation.where_predicates.clone(),
            });
        }
    }

    fn validate_trait_methods(
        &mut self,
        declaration: &ast::TraitDeclaration,
        implementation: &ast::ImplDeclaration,
    ) {
        let concrete_environment = if implementation.generic_parameters.is_empty() {
            self.concrete_trait_environment(declaration, implementation)
        } else {
            None
        };
        for required in &declaration.methods {
            let Some(provided) = implementation
                .methods
                .iter()
                .find(|method| method.name.name == required.name.name)
            else {
                self.diagnostics.push(Diagnostic::error(
                    "E6013",
                    format!(
                        "implementation of `{}` is missing method `{}`",
                        declaration.name.name, required.name.name
                    ),
                    implementation.span,
                ));
                continue;
            };
            if provided.parameters.len() != required.parameters.len()
                || provided.generic_parameters.len() != required.generic_parameters.len()
                || provided.return_type.is_some() != required.return_type.is_some()
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6013",
                        format!(
                            "method `{}` does not match its trait signature",
                            required.name.name
                        ),
                        provided.span,
                    )
                    .with_help("match the trait parameter, generic, and return-type shape"),
                );
            }
            let required_receiver = required
                .parameters
                .first()
                .map(|parameter| &parameter.ty.kind);
            let provided_receiver = provided
                .parameters
                .first()
                .map(|parameter| &parameter.ty.kind);
            if !receiver_shapes_match(required_receiver, provided_receiver) {
                self.diagnostics.push(Diagnostic::error(
                    "E6013",
                    format!(
                        "method `{}` uses a different receiver than the trait",
                        required.name.name
                    ),
                    provided.span,
                ));
            }
            if required.generic_parameters.is_empty()
                && provided.generic_parameters.is_empty()
                && let Some(environment) = &concrete_environment
            {
                self.validate_concrete_trait_method_signature(
                    declaration,
                    required,
                    provided,
                    environment,
                );
            }
        }
        for provided in &implementation.methods {
            if !declaration
                .methods
                .iter()
                .any(|required| required.name.name == provided.name.name)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E6013",
                    format!(
                        "method `{}` is not part of trait `{}`",
                        provided.name.name, declaration.name.name
                    ),
                    provided.name.span,
                ));
            }
        }
    }

    fn concrete_trait_environment(
        &mut self,
        declaration: &ast::TraitDeclaration,
        implementation: &ast::ImplDeclaration,
    ) -> Option<GenericEnvironment> {
        let target = self
            .types
            .resolve_type_name(&implementation.target, &mut self.diagnostics)?;
        let mut environment = self.types.base_environment();
        environment.types.insert("Self".to_owned(), target);
        let trait_type = implementation.trait_type.as_ref()?;
        let arguments = match &trait_type.kind {
            TypeNameKind::Path(_) => &[][..],
            TypeNameKind::Generic { arguments, .. } => arguments.as_slice(),
            _ => return None,
        };
        self.types
            .bind_generic_arguments(
                &declaration.generic_parameters,
                arguments,
                &environment,
                trait_type.span,
                &mut self.diagnostics,
            )
            .map(|(_, environment)| environment)
    }

    fn validate_concrete_trait_method_signature(
        &mut self,
        declaration: &ast::TraitDeclaration,
        required: &ast::TraitMethod,
        provided: &ast::Function,
        environment: &GenericEnvironment,
    ) {
        for (required_parameter, provided_parameter) in
            required.parameters.iter().zip(&provided.parameters)
        {
            let required_type = self.types.resolve_type_name_in(
                &required_parameter.ty,
                environment,
                &mut self.diagnostics,
            );
            let provided_type = self.types.resolve_type_name_in(
                &provided_parameter.ty,
                environment,
                &mut self.diagnostics,
            );
            if required_type != provided_type {
                self.diagnostics.push(Diagnostic::error(
                    "E6013",
                    format!(
                        "parameter `{}` of method `{}` does not match trait `{}`",
                        provided_parameter.name.name, provided.name.name, declaration.name.name
                    ),
                    provided_parameter.ty.span,
                ));
            }
        }
        let required_return = required
            .return_type
            .as_ref()
            .map_or(Some(Type::Unit), |ty| {
                self.types
                    .resolve_type_name_in(ty, environment, &mut self.diagnostics)
            });
        let provided_return = provided
            .return_type
            .as_ref()
            .map_or(Some(Type::Unit), |ty| {
                self.types
                    .resolve_type_name_in(ty, environment, &mut self.diagnostics)
            });
        if required_return != provided_return {
            self.diagnostics.push(Diagnostic::error(
                "E6013",
                format!(
                    "return type of method `{}` does not match trait `{}`",
                    provided.name.name, declaration.name.name
                ),
                provided
                    .return_type
                    .as_ref()
                    .map_or(provided.span, |ty| ty.span),
            ));
        }
    }

    fn resolve_type_definitions(&mut self, program: &ast::Program) {
        for item in &program.items {
            match item {
                Item::Struct(declaration) => self.resolve_struct_definition(declaration),
                Item::Enum(declaration) => self.resolve_enum_definition(declaration),
                _ => {}
            }
        }
    }

    fn resolve_struct_definition(&mut self, declaration: &ast::StructDeclaration) {
        if !declaration.generic_parameters.is_empty() {
            return;
        }
        let Some(Type::Struct(id)) = self.types.names.get(&declaration.name.name).copied() else {
            return;
        };
        let index = usize::try_from(id.0).unwrap_or(usize::MAX);
        if !self
            .types
            .definitions
            .get(index)
            .is_some_and(|definition| definition.span == declaration.span)
        {
            return;
        }
        let fields = self.resolve_fields(&declaration.fields);
        if let Some(definition) = self.types.definitions.get_mut(index) {
            definition.kind = hir::TypeDefinitionKind::Struct { fields };
        }
    }

    fn resolve_enum_definition(&mut self, declaration: &ast::EnumDeclaration) {
        if !declaration.generic_parameters.is_empty() {
            return;
        }
        let Some(Type::Enum(id)) = self.types.names.get(&declaration.name.name).copied() else {
            return;
        };
        let index = usize::try_from(id.0).unwrap_or(usize::MAX);
        if !self
            .types
            .definitions
            .get(index)
            .is_some_and(|definition| definition.span == declaration.span)
        {
            return;
        }
        let mut names = HashMap::new();
        let mut variants = Vec::with_capacity(declaration.variants.len());
        for variant in &declaration.variants {
            if names
                .insert(&variant.name.name, variant.name.span)
                .is_some()
            {
                self.diagnostics.push(Diagnostic::error(
                    "E3009",
                    format!(
                        "enum variant `{}` is declared more than once",
                        variant.name.name
                    ),
                    variant.name.span,
                ));
            }
            let fields = match &variant.payload {
                ast::EnumVariantPayload::Unit => hir::EnumVariantFields::Unit,
                ast::EnumVariantPayload::Tuple(types) => {
                    let mut resolved = Vec::with_capacity(types.len());
                    for ty in types {
                        if let Some(ty) = self.types.resolve_type_name(ty, &mut self.diagnostics) {
                            resolved.push(ty);
                        }
                    }
                    hir::EnumVariantFields::Tuple(resolved)
                }
                ast::EnumVariantPayload::Struct(fields) => {
                    hir::EnumVariantFields::Struct(self.resolve_fields(fields))
                }
            };
            variants.push(hir::EnumVariant {
                name: variant.name.name.clone(),
                fields,
                span: variant.span,
            });
        }
        if let Some(definition) = self.types.definitions.get_mut(index) {
            definition.kind = hir::TypeDefinitionKind::Enum { variants };
        }
    }

    fn validate_derived_traits(&mut self) {
        let types = self
            .types
            .definitions
            .iter()
            .filter_map(|definition| match definition.kind {
                hir::TypeDefinitionKind::Struct { .. } => Some(Type::Struct(definition.id)),
                hir::TypeDefinitionKind::Enum { .. } => Some(Type::Enum(definition.id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        for ty in types {
            self.types.validate_type_derives(ty, &mut self.diagnostics);
        }
    }

    fn resolve_fields(&mut self, fields: &[ast::StructField]) -> Vec<hir::TypeField> {
        let mut names = HashMap::new();
        let mut resolved = Vec::with_capacity(fields.len());
        for field in fields {
            if names.insert(&field.name.name, field.name.span).is_some() {
                self.diagnostics.push(Diagnostic::error(
                    "E3010",
                    format!("field `{}` is declared more than once", field.name.name),
                    field.name.span,
                ));
            }
            if let Some(ty) = self
                .types
                .resolve_type_name(&field.ty, &mut self.diagnostics)
            {
                resolved.push(hir::TypeField {
                    name: field.name.name.clone(),
                    is_public: field.is_public,
                    ty,
                    span: field.span,
                });
            }
        }
        resolved
    }

    fn validate_type_cycles(&mut self) {
        let mut states = vec![0_u8; self.types.definitions.len()];
        for index in 0..self.types.definitions.len() {
            if states[index] != 0 {
                continue;
            }
            let Ok(raw_id) = u32::try_from(index) else {
                break;
            };
            let Some(cyclic) =
                find_type_cycle(&self.types.definitions, &mut states, TypeId(raw_id))
            else {
                continue;
            };
            let Some(definition) = self
                .types
                .definitions
                .get(usize::try_from(cyclic.0).unwrap_or(usize::MAX))
            else {
                continue;
            };
            let name = definition.name.as_deref().unwrap_or("structural type");
            self.diagnostics.push(
                Diagnostic::error(
                    "E3012",
                    format!("type `{name}` contains itself by value"),
                    definition.span,
                )
                .with_help("break the cycle with an owning handle or raw pointer"),
            );
        }
    }

    fn collect_declarations<'ast>(
        &mut self,
        program: &'ast ast::Program,
    ) -> Vec<Declaration<'ast>> {
        let mut declarations = Vec::new();

        for item in &program.items {
            match item {
                Item::Function(function) => {
                    if function.is_comptime {
                        continue;
                    }
                    self.collect_source_declaration(
                        &mut declarations,
                        function,
                        function.name.name.clone(),
                        function.is_public,
                    );
                }
                Item::ExternFunction(function) => {
                    self.collect_extern_declaration(&mut declarations, function);
                }
                Item::Impl(implementation) => {
                    self.collect_impl_declarations(&mut declarations, implementation);
                }
                Item::Import(_)
                | Item::Struct(_)
                | Item::Enum(_)
                | Item::TypeAlias(_)
                | Item::Trait(_)
                | Item::Constant(_)
                | Item::Static(_)
                | Item::Comptime(_) => {}
            }
        }

        declarations
    }

    fn collect_source_declaration<'ast>(
        &mut self,
        declarations: &mut Vec<Declaration<'ast>>,
        function: &'ast ast::Function,
        resolved_name: String,
        is_public: bool,
    ) {
        if !function.generic_parameters.is_empty() {
            self.register_generic_function_template(
                function,
                function.generic_parameters.clone(),
                function.where_predicates.clone(),
                resolved_name,
                0,
                is_public,
            );
            return;
        }
        let source = SignatureSource {
            resolved_name: &resolved_name,
            source_name: &function.name,
            parameters: &function.parameters,
            return_type: function.return_type.as_ref(),
            span: function.span,
            requires_unsafe: false,
            is_public,
        };
        let Some(signature) = self.build_signature(declarations.len(), &source) else {
            return;
        };
        self.signatures
            .insert(resolved_name.clone(), signature.clone());
        declarations.push(Declaration::Source {
            function,
            resolved_name,
            signature,
        });
    }

    fn register_generic_function_template(
        &mut self,
        function: &ast::Function,
        parameters: Vec<ast::GenericParameter>,
        where_predicates: Vec<ast::WherePredicate>,
        resolved_name: String,
        explicit_parameter_start: usize,
        is_public: bool,
    ) {
        if explicit_parameter_start == 0 {
            validate_generic_parameter_names(&parameters, &mut self.diagnostics);
        } else {
            let (implicit, explicit) = parameters.split_at(explicit_parameter_start);
            validate_generic_parameter_names(implicit, &mut self.diagnostics);
            validate_generic_parameter_names(explicit, &mut self.diagnostics);
        }
        if self.signatures.contains_key(&resolved_name)
            || self
                .generic_functions
                .templates
                .contains_key(&resolved_name)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3004",
                    format!("function `{resolved_name}` is declared more than once"),
                    function.name.span,
                )
                .with_help("rename or remove one of the declarations"),
            );
            return;
        }
        self.generic_functions.templates.insert(
            resolved_name.clone(),
            GenericFunctionTemplate {
                function: function.clone(),
                parameters,
                where_predicates,
                module_identity: symbol_module_identity(&resolved_name).map(str::to_owned),
                resolved_name,
                explicit_parameter_start,
                is_public,
            },
        );
    }

    fn collect_extern_declaration<'ast>(
        &mut self,
        declarations: &mut Vec<Declaration<'ast>>,
        function: &'ast ast::ExternFunction,
    ) {
        let source = SignatureSource {
            resolved_name: &function.name.name,
            source_name: &function.name,
            parameters: &function.parameters,
            return_type: function.return_type.as_ref(),
            span: function.span,
            requires_unsafe: true,
            is_public: function.is_public,
        };
        let Some(signature) = self.build_signature(declarations.len(), &source) else {
            return;
        };
        self.validate_extern_signature(function, &signature);
        self.signatures
            .insert(function.name.name.clone(), signature.clone());
        declarations.push(Declaration::Extern {
            function,
            signature,
        });
    }

    fn collect_impl_declarations<'ast>(
        &mut self,
        declarations: &mut Vec<Declaration<'ast>>,
        implementation: &'ast ast::ImplDeclaration,
    ) {
        let has_generic_methods = implementation
            .methods
            .iter()
            .any(|method| !method.generic_parameters.is_empty());
        if !implementation.generic_parameters.is_empty() || has_generic_methods {
            let Some(owner) = type_constructor_name(&implementation.target).map(str::to_owned)
            else {
                self.diagnostics.push(Diagnostic::error(
                    "E6001",
                    "generic impl requires a single nominal target type",
                    implementation.target.span,
                ));
                return;
            };
            if !self.types.generic_templates.contains_key(&owner)
                && !self.types.names.contains_key(&owner)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E6001",
                    format!("cannot implement methods for unknown type `{owner}`"),
                    implementation.target.span,
                ));
                return;
            }
            validate_generic_parameter_names(
                &implementation.generic_parameters,
                &mut self.diagnostics,
            );
            for method in &implementation.methods {
                if method.is_comptime {
                    continue;
                }
                let mut parameters = implementation.generic_parameters.clone();
                parameters.extend(method.generic_parameters.iter().cloned());
                let mut where_predicates = implementation.where_predicates.clone();
                where_predicates.extend(method.where_predicates.iter().cloned());
                let resolved_name = format!("{owner}::{}", method.name.name);
                self.register_generic_function_template(
                    method,
                    parameters,
                    where_predicates,
                    resolved_name,
                    implementation.generic_parameters.len(),
                    method.is_public || implementation.trait_type.is_some(),
                );
            }
            return;
        }
        let Some(target) = self
            .types
            .resolve_type_name(&implementation.target, &mut self.diagnostics)
        else {
            return;
        };
        let Some(owner) = self
            .types
            .definition(target)
            .and_then(|definition| definition.name.clone())
        else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6001",
                    "inherent impl requires a named struct or enum",
                    implementation.target.span,
                )
                .with_help("implement methods on a declared nominal type"),
            );
            return;
        };
        for method in &implementation.methods {
            if method.is_comptime {
                continue;
            }
            let resolved_name = format!("{owner}::{}", method.name.name);
            self.collect_source_declaration(
                declarations,
                method,
                resolved_name,
                method.is_public || implementation.trait_type.is_some(),
            );
        }
    }

    fn build_signature(
        &mut self,
        declaration_count: usize,
        source: &SignatureSource<'_>,
    ) -> Option<Signature> {
        if self.signatures.contains_key(source.resolved_name)
            || self
                .generic_functions
                .templates
                .contains_key(source.resolved_name)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3004",
                    format!(
                        "function `{}` is declared more than once",
                        source.resolved_name
                    ),
                    source.source_name.span,
                )
                .with_help("rename or remove one of the declarations"),
            );
            return None;
        }
        let Ok(index) = u32::try_from(declaration_count) else {
            self.diagnostics.push(Diagnostic::error(
                "E3999",
                "this compilation unit contains too many functions",
                source.span,
            ));
            return None;
        };
        let parameter_types = source
            .parameters
            .iter()
            .map(|parameter| {
                self.types
                    .resolve_type_name(&parameter.ty, &mut self.diagnostics)
                    .unwrap_or(Type::Unit)
            })
            .collect();
        let return_type = source.return_type.map_or(Type::Unit, |ty| {
            self.types
                .resolve_type_name(ty, &mut self.diagnostics)
                .unwrap_or(Type::Unit)
        });
        Some(Signature {
            id: FunctionId(index),
            parameter_types,
            return_type,
            requires_unsafe: source.requires_unsafe,
            is_public: source.is_public,
        })
    }

    fn validate_extern_signature(&mut self, function: &ast::ExternFunction, signature: &Signature) {
        for (parameter, ty) in function.parameters.iter().zip(&signature.parameter_types) {
            if !self.types.is_ffi_safe_extern_value(*ty) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E5001",
                        format!("type `{ty}` is not ABI-safe in an extern parameter"),
                        parameter.ty.span,
                    )
                    .with_help(
                        "use C-compatible scalars, cstr, raw pointers, or an ABI-safe `@repr(C)` struct",
                    ),
                );
            }
        }
        if !self.types.is_ffi_safe_return_type(signature.return_type) {
            let span = function
                .return_type
                .as_ref()
                .map_or(function.span, |ty| ty.span);
            self.diagnostics.push(
                Diagnostic::error(
                    "E5001",
                    format!(
                        "type `{}` is not ABI-safe as an extern return value",
                        signature.return_type
                    ),
                    span,
                )
                .with_help(
                    "use `()`, a C-compatible scalar, cstr, a raw pointer, or an ABI-safe `@repr(C)` struct",
                ),
            );
        }
    }

    fn validate_entry(&mut self) -> Option<FunctionId> {
        let Some(main) = self.signatures.get("main") else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3001",
                    "program requires a `main` function",
                    Span::empty(0),
                )
                .with_help("add `fn main() -> i32 { ... }`"),
            );
            return None;
        };
        let id = main.id;
        if main.requires_unsafe {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3001",
                    "`main` must have a Reimer function body",
                    Span::empty(0),
                )
                .with_help("declare `fn main() -> i32 { ... }`"),
            );
        }
        let has_parameters = !main.parameter_types.is_empty();
        let returns_i32 = main.return_type == Type::I32;

        if has_parameters {
            self.diagnostics.push(Diagnostic::error(
                "E3002",
                "`main` cannot accept parameters",
                Span::empty(0),
            ));
        }
        if !returns_i32 {
            self.diagnostics.push(
                Diagnostic::error("E3003", "`main` must return `i32`", Span::empty(0))
                    .with_help("declare `fn main() -> i32`"),
            );
        }
        Some(id)
    }
}

fn find_type_cycle(
    definitions: &[hir::TypeDefinition],
    states: &mut [u8],
    id: TypeId,
) -> Option<TypeId> {
    let index = usize::try_from(id.0).ok()?;
    match states.get(index).copied()? {
        1 => return Some(id),
        2 => return None,
        _ => {}
    }
    states[index] = 1;
    let definition = definitions.get(index)?;
    for child in composite_children(definition) {
        if let Some(cyclic) = find_type_cycle(definitions, states, child) {
            return Some(cyclic);
        }
    }
    states[index] = 2;
    None
}

fn composite_children(definition: &hir::TypeDefinition) -> Vec<TypeId> {
    let mut children = Vec::new();
    match &definition.kind {
        hir::TypeDefinitionKind::Struct { fields } => {
            children.extend(
                fields
                    .iter()
                    .filter_map(|field| composite_type_id(field.ty)),
            );
        }
        hir::TypeDefinitionKind::Enum { variants } => {
            for variant in variants {
                match &variant.fields {
                    hir::EnumVariantFields::Unit => {}
                    hir::EnumVariantFields::Tuple(types) => {
                        children.extend(types.iter().filter_map(|ty| composite_type_id(*ty)));
                    }
                    hir::EnumVariantFields::Struct(fields) => {
                        children.extend(
                            fields
                                .iter()
                                .filter_map(|field| composite_type_id(field.ty)),
                        );
                    }
                }
            }
        }
        hir::TypeDefinitionKind::Tuple { elements } => {
            children.extend(elements.iter().filter_map(|ty| composite_type_id(*ty)));
        }
        hir::TypeDefinitionKind::Array { element, .. } => {
            children.extend(composite_type_id(*element));
        }
        hir::TypeDefinitionKind::Reference { .. }
        | hir::TypeDefinitionKind::RawPointer { .. }
        | hir::TypeDefinitionKind::Slice { .. }
        | hir::TypeDefinitionKind::Function { .. } => {}
    }
    children
}

fn composite_type_id(ty: Type) -> Option<TypeId> {
    match ty {
        Type::Struct(id)
        | Type::Enum(id)
        | Type::Tuple(id)
        | Type::Array(id)
        | Type::Reference(id)
        | Type::RawPointer(id)
        | Type::Slice(id)
        | Type::Function(id) => Some(id),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct Binding {
    local: LocalId,
    ty: Type,
    mutable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continues,
    Stops,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    Statement,
    Expression,
}

#[derive(Debug, Clone, Copy)]
struct LoopContext {
    kind: LoopKind,
    expected: Option<Type>,
    break_type: Option<Type>,
}

#[derive(Debug, Default, Clone, Copy)]
struct BorrowState {
    shared: u32,
    mutable: bool,
}

#[derive(Debug, Clone, Copy)]
struct DeferredUse {
    span: Span,
    consuming: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FieldProjection {
    Named(String),
    Tuple(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MovedField {
    local: LocalId,
    projections: Vec<FieldProjection>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum PlaceAvailability {
    #[default]
    InitializedOnly,
    AllowReinitialization,
}

struct ValueSymbols<'context> {
    functions: &'context mut HashMap<String, Signature>,
    statics: &'context HashMap<String, StaticSymbol>,
}

struct FunctionAnalyzer<'context> {
    signatures: &'context mut HashMap<String, Signature>,
    statics: &'context HashMap<String, StaticSymbol>,
    generic_functions: &'context mut GenericFunctionRegistry,
    types: &'context mut TypeRegistry,
    diagnostics: &'context mut Vec<Diagnostic>,
    signature: Signature,
    generic_environment: GenericEnvironment,
    scopes: Vec<HashMap<String, Binding>>,
    next_local: u32,
    loops: Vec<LoopContext>,
    unsafe_depth: usize,
    parameter_count: u32,
    borrow_states: HashMap<LocalId, BorrowState>,
    scoped_roots: HashMap<LocalId, LocalId>,
    scoped_expression_roots: HashMap<Span, LocalId>,
    borrow_scopes: Vec<Vec<(LocalId, bool)>>,
    persistent_borrow: bool,
    defer_depth: usize,
    moved_locals: HashMap<LocalId, Span>,
    moved_fields: HashMap<MovedField, Span>,
    deferred_uses: HashMap<LocalId, DeferredUse>,
    deferred_use_scopes: Vec<Vec<LocalId>>,
    consuming_value: bool,
    reborrow_argument: bool,
    field_base_depth: usize,
    place_availability: PlaceAvailability,
    module_identity: Option<String>,
}

impl<'context> FunctionAnalyzer<'context> {
    fn new(
        symbols: ValueSymbols<'context>,
        generic_functions: &'context mut GenericFunctionRegistry,
        types: &'context mut TypeRegistry,
        diagnostics: &'context mut Vec<Diagnostic>,
        signature: Signature,
        generic_environment: GenericEnvironment,
        module_identity: Option<String>,
    ) -> Self {
        let ValueSymbols { functions, statics } = symbols;
        Self {
            signatures: functions,
            statics,
            generic_functions,
            types,
            diagnostics,
            signature,
            generic_environment,
            scopes: vec![HashMap::new()],
            next_local: 0,
            loops: Vec::new(),
            unsafe_depth: 0,
            parameter_count: 0,
            borrow_states: HashMap::new(),
            scoped_roots: HashMap::new(),
            scoped_expression_roots: HashMap::new(),
            borrow_scopes: vec![Vec::new()],
            persistent_borrow: false,
            defer_depth: 0,
            moved_locals: HashMap::new(),
            moved_fields: HashMap::new(),
            deferred_uses: HashMap::new(),
            deferred_use_scopes: vec![Vec::new()],
            consuming_value: true,
            reborrow_argument: false,
            field_base_depth: 0,
            place_availability: PlaceAvailability::default(),
            module_identity,
        }
    }

    fn analyze(&mut self, function: &ast::Function, resolved_name: String) -> hir::Function {
        let mut parameters = Vec::with_capacity(function.parameters.len());
        let parameter_types = self.signature.parameter_types.clone();
        for (parameter, ty) in function.parameters.iter().zip(parameter_types) {
            let local = self.new_local(parameter.span);
            if self.scopes[0]
                .insert(
                    parameter.name.name.clone(),
                    Binding {
                        local,
                        ty,
                        mutable: false,
                    },
                )
                .is_some()
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3101",
                        format!(
                            "parameter `{}` is declared more than once",
                            parameter.name.name
                        ),
                        parameter.name.span,
                    )
                    .with_help("give each parameter a unique name"),
                );
            }
            parameters.push(hir::Parameter {
                local,
                name: parameter.name.name.clone(),
                ty,
                span: parameter.span,
            });
        }
        self.parameter_count = self.next_local;

        let analyzed = self.analyze_block(&function.body, Some(self.signature.return_type));
        if let Some(tail) = function.body.tail.as_deref() {
            self.validate_scoped_return(tail, tail.span());
        }
        if !self.signature.return_type.accepts(analyzed.ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3111",
                    format!(
                        "function `{}` returns `{}`, but its body produces `{}`",
                        function.name.name, self.signature.return_type, analyzed.ty
                    ),
                    function.body.span,
                )
                .with_help(format!(
                    "return a `{}` value on every reachable path",
                    self.signature.return_type
                )),
            );
        }

        hir::Function {
            id: self.signature.id,
            name: resolved_name,
            is_public: self.signature.is_public,
            attributes: function_attributes(function),
            parameters,
            return_type: self.signature.return_type,
            body: analyzed,
            span: function.span,
        }
    }

    fn analyze_block(&mut self, block: &ast::Block, expected_tail: Option<Type>) -> hir::Block {
        self.push_scope();
        let mut statements = Vec::with_capacity(block.statements.len());
        let mut flow = Flow::Continues;

        for statement in &block.statements {
            let (statement, statement_flow) = self.analyze_statement(statement);
            statements.push(statement);
            if flow == Flow::Continues && statement_flow == Flow::Stops {
                flow = Flow::Stops;
            }
        }

        let tail = block.tail.as_deref().map(|expression| {
            Box::new(self.analyze_expression_expected(expression, expected_tail))
        });
        let ty = if flow == Flow::Stops {
            Type::Never
        } else {
            tail.as_ref().map_or(Type::Unit, |expression| expression.ty)
        };
        self.pop_scope();

        hir::Block {
            statements,
            tail,
            ty,
            span: block.span,
        }
    }

    fn analyze_statement(&mut self, statement: &AstStatement) -> (hir::Statement, Flow) {
        match statement {
            AstStatement::Let(binding) => self.analyze_let_statement(binding),
            AstStatement::Expression(statement) => {
                let expression = self.analyze_expression(&statement.expression);
                let flow = if expression.ty == Type::Never {
                    Flow::Stops
                } else {
                    Flow::Continues
                };
                (hir::Statement::Expression(expression), flow)
            }
            AstStatement::Defer(statement) => {
                self.defer_depth += 1;
                let action = self.analyze_expression_expected(&statement.action, Some(Type::Unit));
                self.defer_depth -= 1;
                self.require_type(Type::Unit, action.ty, action.span, "deferred action");
                (
                    hir::Statement::Defer {
                        action,
                        span: statement.span,
                    },
                    Flow::Continues,
                )
            }
            AstStatement::Return(statement) => {
                self.reject_deferred_control_flow("`return`", statement.span);
                if let Some(value) = &statement.value {
                    self.validate_scoped_return(value, statement.span);
                }
                let value = statement.value.as_ref().map(|value| {
                    self.analyze_expression_expected(value, Some(self.signature.return_type))
                });
                let actual = value.as_ref().map_or(Type::Unit, |value| value.ty);
                self.require_type(
                    self.signature.return_type,
                    actual,
                    statement.span,
                    "return value",
                );
                (
                    hir::Statement::Return {
                        value,
                        span: statement.span,
                    },
                    Flow::Stops,
                )
            }
            AstStatement::While(statement) => self.analyze_while_statement(statement),
            AstStatement::For(statement) => self.analyze_for_statement(statement),
            AstStatement::Break(statement) => {
                self.reject_deferred_control_flow("`break`", statement.span);
                let expected = self.loops.last().and_then(|context| context.expected);
                let value = statement
                    .value
                    .as_ref()
                    .map(|value| self.analyze_expression_expected(value, expected));
                let actual = value.as_ref().map_or(Type::Unit, |value| value.ty);
                if self.loops.is_empty() {
                    self.diagnostics.push(Diagnostic::error(
                        "E3109",
                        "`break` can only be used inside a loop",
                        statement.span,
                    ));
                } else {
                    self.record_break_type(actual, statement.span);
                }
                (
                    hir::Statement::Break {
                        value,
                        span: statement.span,
                    },
                    Flow::Stops,
                )
            }
            AstStatement::Continue(span) => {
                self.reject_deferred_control_flow("`continue`", *span);
                if self.loops.is_empty() {
                    self.diagnostics.push(Diagnostic::error(
                        "E3110",
                        "`continue` can only be used inside a loop",
                        *span,
                    ));
                }
                (hir::Statement::Continue(*span), Flow::Stops)
            }
        }
    }

    fn reject_deferred_control_flow(&mut self, operation: &str, span: Span) {
        if self.defer_depth > 0 {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3142",
                    format!("{operation} cannot transfer control from a deferred action"),
                    span,
                )
                .with_help("move the control-flow operation outside the `defer` action"),
            );
        }
    }

    fn analyze_let_statement(&mut self, binding: &ast::LetStatement) -> (hir::Statement, Flow) {
        let declared_type = binding.ty.as_ref().and_then(|ty| {
            self.types
                .resolve_type_name_in(ty, &self.generic_environment, self.diagnostics)
        });
        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow = initializer_stores_borrow(&binding.initializer);
        let initializer = self.analyze_expression_expected(&binding.initializer, declared_type);
        self.persistent_borrow = previous_persistence;
        let ty = declared_type.unwrap_or(initializer.ty);
        if let Some(declared_type) = declared_type {
            self.require_type(
                declared_type,
                initializer.ty,
                binding.initializer.span(),
                "binding initializer",
            );
        }
        let local = self.new_local(binding.name.span);
        if self.types.is_scoped(ty)
            && let Some(root) = self.scoped_source_local(&binding.initializer)
        {
            self.scoped_roots.insert(local, root);
        }
        self.current_scope().insert(
            binding.name.name.clone(),
            Binding {
                local,
                ty,
                mutable: binding.mutable,
            },
        );
        let flow = if initializer.ty == Type::Never {
            Flow::Stops
        } else {
            Flow::Continues
        };
        (
            hir::Statement::Let {
                local,
                name: binding.name.name.clone(),
                mutable: binding.mutable,
                ty,
                initializer,
                span: binding.span,
            },
            flow,
        )
    }

    fn validate_scoped_return(&mut self, expression: &AstExpression, span: Span) {
        if !self.types.is_scoped(self.signature.return_type) {
            return;
        }
        match expression {
            AstExpression::Match(expression) => {
                for arm in &expression.arms {
                    if !self.scoped_return_is_empty(&arm.body) {
                        self.validate_scoped_return(&arm.body, arm.span);
                    }
                }
                return;
            }
            AstExpression::If(expression) => {
                if let Some(tail) = expression.then_branch.tail.as_deref()
                    && !self.scoped_return_is_empty(tail)
                {
                    self.validate_scoped_return(tail, tail.span());
                }
                if let Some(else_branch) = expression.else_branch.as_ref()
                    && !self.scoped_return_is_empty(else_branch)
                {
                    self.validate_scoped_return(else_branch, else_branch.span());
                }
                return;
            }
            AstExpression::Block(block) => {
                if let Some(tail) = block.tail.as_deref()
                    && !self.scoped_return_is_empty(tail)
                {
                    self.validate_scoped_return(tail, tail.span());
                }
                return;
            }
            _ if self.scoped_return_is_empty(expression) => return,
            _ => {}
        }
        let rooted_in_static = scoped_return_root(expression)
            .and_then(single_path_name)
            .is_some_and(|name| self.statics.contains_key(name));
        if rooted_in_static {
            return;
        }
        let scoped_parameters = self
            .signature
            .parameter_types
            .iter()
            .filter(|ty| self.types.is_scoped(**ty))
            .count();
        if scoped_parameters != 1
            || !self
                .signature
                .parameter_types
                .first()
                .is_some_and(|ty| self.types.is_scoped(*ty))
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3137",
                    "a scoped return requires exactly one scoped source as its first parameter",
                    span,
                )
                .with_help(
                    "place the owner or borrowed source first and return owned data when multiple lifetimes are involved",
                ),
            );
            return;
        }
        let safe_parameter = self
            .scoped_expression_roots
            .get(&expression.span())
            .copied()
            .or_else(|| {
                scoped_return_root(expression)
                    .and_then(|path| single_path_name(path))
                    .and_then(|name| self.lookup(name))
                    .map(|binding| self.scoped_root(binding.local))
            })
            .is_some_and(|root| {
                if root.0 >= self.parameter_count {
                    return false;
                }
                self.scopes
                    .first()
                    .and_then(|scope| scope.values().find(|candidate| candidate.local == root))
                    .is_some_and(|candidate| self.types.is_scoped(candidate.ty))
            });
        if !safe_parameter {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3137",
                    "a scoped reference or view cannot escape this function",
                    span,
                )
                .with_help("return data rooted in the first scoped parameter or return owned data"),
            );
        }
    }

    fn scoped_return_is_empty(&self, expression: &AstExpression) -> bool {
        if matches!(expression, AstExpression::Path(path) if single_path_name(path) == Some("None"))
        {
            return true;
        }
        let AstExpression::Call(call) = expression else {
            return false;
        };
        let AstExpression::Path(path) = &call.callee else {
            return false;
        };
        match (
            single_path_name(path),
            self.types.intrinsic(self.signature.return_type),
        ) {
            (Some("Some"), Some(IntrinsicType::Option { value })) => !self.types.is_scoped(value),
            (Some("Ok"), Some(IntrinsicType::Result { success, .. })) => {
                !self.types.is_scoped(success)
            }
            (Some("Err"), Some(IntrinsicType::Result { error, .. })) => {
                !self.types.is_scoped(error)
            }
            _ => false,
        }
    }

    fn scoped_source_local(&self, expression: &AstExpression) -> Option<LocalId> {
        let path = scoped_return_root(expression)?;
        let name = single_path_name(path)?;
        let binding = self.lookup(name)?;
        Some(self.scoped_root(binding.local))
    }

    fn scoped_root(&self, mut local: LocalId) -> LocalId {
        while let Some(root) = self.scoped_roots.get(&local).copied() {
            if root == local {
                break;
            }
            local = root;
        }
        local
    }

    fn analyze_while_statement(
        &mut self,
        statement: &ast::WhileStatement,
    ) -> (hir::Statement, Flow) {
        let condition = self.analyze_expression_expected(&statement.condition, Some(Type::Bool));
        self.require_type(
            Type::Bool,
            condition.ty,
            statement.condition.span(),
            "while condition",
        );
        self.loops.push(LoopContext {
            kind: LoopKind::Statement,
            expected: Some(Type::Unit),
            break_type: None,
        });
        let body = self.analyze_block(&statement.body, None);
        self.loops.pop();
        (
            hir::Statement::While {
                condition,
                body,
                span: statement.span,
            },
            Flow::Continues,
        )
    }

    fn analyze_for_statement(&mut self, statement: &ast::ForStatement) -> (hir::Statement, Flow) {
        let iterable = self.analyze_expression(&statement.iterable);
        let indexed_element = self
            .array_shape(iterable.ty)
            .map(|(element, _)| element)
            .or_else(|| {
                self.types
                    .slice_shape(iterable.ty)
                    .map(|(element, _)| element)
            })
            .or_else(|| {
                let (target, _, _) = self.types.pointer_shape(iterable.ty)?;
                self.array_shape(target).map(|(element, _)| element)
            });
        let iteration = indexed_element
            .map(|element| (element, hir::ForIteration::Indexed))
            .or_else(|| {
                self.types
                    .is_chars(iterable.ty)
                    .then_some((Type::Char, hir::ForIteration::Chars))
            });
        let Some((element_type, iteration)) = iteration else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3130",
                    format!("`for` cannot iterate over `{}`", iterable.ty),
                    statement.iterable.span(),
                )
                .with_help(
                    "iteration requires an array, array reference, slice, or `str.chars()` iterator",
                ),
            );
            return (
                hir::Statement::Expression(invalid_composite_expression(statement.span)),
                Flow::Continues,
            );
        };
        self.push_scope();
        let pattern = self.analyze_pattern(&statement.pattern, element_type);
        if !pattern_is_irrefutable(&pattern) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3131",
                    "`for` requires an irrefutable pattern",
                    statement.pattern.span(),
                )
                .with_help("use bindings, `_`, or tuple patterns containing only those forms"),
            );
        }
        self.loops.push(LoopContext {
            kind: LoopKind::Statement,
            expected: Some(Type::Unit),
            break_type: None,
        });
        let body = self.analyze_block(&statement.body, None);
        self.loops.pop();
        self.pop_scope();
        (
            hir::Statement::For {
                pattern,
                element_type,
                iteration,
                iterable,
                body,
                span: statement.span,
            },
            Flow::Continues,
        )
    }

    fn record_break_type(&mut self, actual: Type, span: Span) {
        let Some(index) = self.loops.len().checked_sub(1) else {
            return;
        };
        let context = self.loops[index];
        if context.kind == LoopKind::Statement {
            if actual != Type::Never && actual != Type::Unit {
                self.type_mismatch(Type::Unit, actual, span, "statement loop break");
            }
            return;
        }
        let expected = context.expected.or(context.break_type);
        if let Some(expected) = expected
            && !expected.accepts(actual)
        {
            self.type_mismatch(expected, actual, span, "loop break value");
            return;
        }
        if actual != Type::Never && context.break_type.is_none() {
            self.loops[index].break_type = Some(actual);
        }
    }

    fn analyze_expression(&mut self, expression: &AstExpression) -> Expression {
        self.analyze_expression_expected(expression, None)
    }

    fn analyze_expression_non_consuming(&mut self, expression: &AstExpression) -> Expression {
        let previous = self.consuming_value;
        self.consuming_value = false;
        let analyzed = self.analyze_expression(expression);
        self.consuming_value = previous;
        analyzed
    }

    fn analyze_expression_expected(
        &mut self,
        expression: &AstExpression,
        expected: Option<Type>,
    ) -> Expression {
        let analyzed = match expression {
            AstExpression::Integer(literal) => self.analyze_integer_literal(literal, expected),
            AstExpression::Float(literal) => self.analyze_float_literal(literal, expected),
            AstExpression::Character(literal) => Expression {
                kind: ExpressionKind::Character(literal.value),
                ty: Type::Char,
                span: literal.span,
            },
            AstExpression::String(literal) => {
                if let Some(expected) = expected {
                    self.require_type(expected, Type::Str, literal.span, "string literal");
                }
                Expression {
                    kind: ExpressionKind::String(literal.value.clone()),
                    ty: Type::Str,
                    span: literal.span,
                }
            }
            AstExpression::FormattedString(formatted) => {
                self.reject_standalone_formatted_string(formatted)
            }
            AstExpression::CString(literal) => {
                if let Some(expected) = expected {
                    self.require_type(expected, Type::CStr, literal.span, "C string literal");
                }
                Expression {
                    kind: ExpressionKind::CString(literal.value.clone()),
                    ty: Type::CStr,
                    span: literal.span,
                }
            }
            AstExpression::Boolean(literal) => Expression {
                kind: ExpressionKind::Boolean(literal.value),
                ty: Type::Bool,
                span: literal.span,
            },
            AstExpression::Unit(span) => Expression {
                kind: ExpressionKind::Unit,
                ty: Type::Unit,
                span: *span,
            },
            AstExpression::Path(path) => self.analyze_path(path, expected),
            AstExpression::Unary(expression) => self.analyze_unary(expression, expected),
            AstExpression::Binary(expression) => self.analyze_binary(expression, expected),
            AstExpression::Call(expression) => self.analyze_call(expression, expected),
            AstExpression::If(expression) => self.analyze_if(expression, expected),
            AstExpression::Match(expression) => self.analyze_match(expression, expected),
            AstExpression::Loop(expression) => self.analyze_loop(expression, expected),
            AstExpression::Unsafe(block) => {
                self.unsafe_depth += 1;
                let block = self.analyze_block(block, expected);
                self.unsafe_depth -= 1;
                let ty = block.ty;
                Expression {
                    kind: ExpressionKind::Block(Box::new(block)),
                    ty,
                    span: expression.span(),
                }
            }
            AstExpression::Block(block) => {
                let block = self.analyze_block(block, expected);
                let ty = block.ty;
                Expression {
                    kind: ExpressionKind::Block(Box::new(block)),
                    ty,
                    span: expression.span(),
                }
            }
            AstExpression::Assignment(expression) => self.analyze_assignment(expression),
            AstExpression::Cast(expression) => self.analyze_cast(expression),
            AstExpression::Tuple(expression) => self.analyze_tuple(expression, expected),
            AstExpression::PackExpansion(expansion) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6021",
                        "a value pack can only expand directly inside a tuple or array",
                        expansion.span,
                    )
                    .with_help(
                        "place the expansion in `( ...Pack => expression, )` or `[...Pack => expression]`",
                    ),
                );
                invalid_composite_expression(expansion.span)
            }
            AstExpression::Array(expression) => self.analyze_array(expression, expected),
            AstExpression::Struct(expression) => self.analyze_struct(expression, expected),
            AstExpression::Field(expression) => self.analyze_field(expression),
            AstExpression::Index(expression) => self.analyze_index(expression),
            AstExpression::Try { value, span } => self.analyze_try(value, *span),
        };
        if self.types.is_scoped(analyzed.ty)
            && let Some(root) = self.scoped_source_local(expression)
        {
            self.scoped_expression_roots.insert(expression.span(), root);
        }
        analyzed
    }

    fn reject_standalone_formatted_string(
        &mut self,
        formatted: &ast::FormattedStringExpression,
    ) -> Expression {
        for fragment in &formatted.fragments {
            if let ast::FormattedStringFragment::Display(expression)
            | ast::FormattedStringFragment::Debug(expression) = fragment
            {
                self.analyze_expression(expression);
            }
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3160",
                "formatted string requires a formatting destination",
                formatted.span,
            )
            .with_help("append it with `string.push_format(f\"...\")`"),
        );
        Expression {
            kind: ExpressionKind::Unit,
            ty: Type::Unit,
            span: formatted.span,
        }
    }

    fn analyze_integer_literal(
        &mut self,
        literal: &ast::IntegerLiteral,
        expected: Option<Type>,
    ) -> Expression {
        let ty = expected.filter(|ty| ty.is_integer()).unwrap_or(Type::I32);
        let maximum = integer_positive_maximum(ty);
        let value = if literal.value <= maximum {
            literal.value
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3104",
                    format!("integer literal does not fit in `{ty}`"),
                    literal.span,
                )
                .with_help(format!("use a value no greater than {maximum}")),
            );
            0
        };
        Expression {
            kind: ExpressionKind::Integer(value),
            ty,
            span: literal.span,
        }
    }

    fn analyze_float_literal(
        &mut self,
        literal: &ast::FloatLiteral,
        expected: Option<Type>,
    ) -> Expression {
        let ty = expected.filter(|ty| ty.is_float()).unwrap_or(Type::F64);
        let value = literal.value();
        let kind = if ty == Type::F32 {
            let narrowed = narrow_f64_to_f32(value);
            if narrowed.is_finite() {
                ExpressionKind::Float32(narrowed.to_bits())
            } else {
                self.diagnostics.push(Diagnostic::error(
                    "E3104",
                    "floating-point literal does not fit in `f32`",
                    literal.span,
                ));
                ExpressionKind::Float32(0.0_f32.to_bits())
            }
        } else {
            ExpressionKind::Float64(literal.bits)
        };
        Expression {
            kind,
            ty,
            span: literal.span,
        }
    }

    fn analyze_tuple(
        &mut self,
        tuple: &ast::TupleExpression,
        expected: Option<Type>,
    ) -> Expression {
        let expected_elements = expected.and_then(|ty| {
            let definition = self.types.definition(ty)?;
            let hir::TypeDefinitionKind::Tuple { elements } = &definition.kind else {
                return None;
            };
            Some(elements.clone())
        });
        let mut elements = Vec::new();
        for element in &tuple.elements {
            if let AstExpression::PackExpansion(expansion) = element {
                let Some(pack_types) = self
                    .generic_environment
                    .type_packs
                    .get(&expansion.pack.name)
                    .cloned()
                else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "E6021",
                            format!("unknown type pack `{}`", expansion.pack.name),
                            expansion.pack.span,
                        )
                        .with_help("declare the pack with `<...Types>`"),
                    );
                    continue;
                };
                for pack_type in pack_types {
                    let previous = self
                        .generic_environment
                        .types
                        .insert(expansion.pack.name.clone(), pack_type);
                    let expected_element = expected_elements
                        .as_ref()
                        .and_then(|types| types.get(elements.len()).copied());
                    elements.push(
                        self.analyze_expression_expected(&expansion.template, expected_element),
                    );
                    if let Some(previous) = previous {
                        self.generic_environment
                            .types
                            .insert(expansion.pack.name.clone(), previous);
                    } else {
                        self.generic_environment.types.remove(&expansion.pack.name);
                    }
                }
            } else {
                let expected_element = expected_elements
                    .as_ref()
                    .and_then(|types| types.get(elements.len()).copied());
                elements.push(self.analyze_expression_expected(element, expected_element));
            }
        }
        if let Some(expected_elements) = &expected_elements
            && expected_elements.len() != elements.len()
        {
            self.diagnostics.push(Diagnostic::error(
                "E3115",
                format!(
                    "tuple requires {} element(s), found {}",
                    expected_elements.len(),
                    elements.len()
                ),
                tuple.span,
            ));
        }
        let ty = expected
            .filter(|ty| matches!(ty, Type::Tuple(_)))
            .or_else(|| {
                self.types.intern_tuple(
                    elements.iter().map(|element| element.ty).collect(),
                    tuple.span,
                    self.diagnostics,
                )
            })
            .unwrap_or(Type::Unit);
        Expression {
            kind: ExpressionKind::Tuple(elements),
            ty,
            span: tuple.span,
        }
    }

    fn analyze_array(
        &mut self,
        array: &ast::ArrayExpression,
        expected: Option<Type>,
    ) -> Expression {
        match &array.kind {
            ast::ArrayExpressionKind::List(elements) => {
                self.analyze_array_list(elements, array.span, expected)
            }
            ast::ArrayExpressionKind::Repeat { value, length } => {
                self.analyze_array_repeat(value, length, array.span, expected)
            }
        }
    }

    fn analyze_array_list(
        &mut self,
        array_elements: &[AstExpression],
        span: Span,
        expected: Option<Type>,
    ) -> Expression {
        let expected_shape = expected.and_then(|ty| {
            let definition = self.types.definition(ty)?;
            let hir::TypeDefinitionKind::Array { element, length } = definition.kind else {
                return None;
            };
            Some((element, length))
        });
        let mut elements = Vec::with_capacity(array_elements.len());
        let mut element_type = expected_shape.map(|shape| shape.0);
        for element in array_elements {
            if let AstExpression::PackExpansion(expansion) = element {
                let Some(pack_types) = self
                    .generic_environment
                    .type_packs
                    .get(&expansion.pack.name)
                    .cloned()
                else {
                    self.diagnostics.push(Diagnostic::error(
                        "E6021",
                        format!("unknown type pack `{}`", expansion.pack.name),
                        expansion.pack.span,
                    ));
                    continue;
                };
                for pack_type in pack_types {
                    let previous = self
                        .generic_environment
                        .types
                        .insert(expansion.pack.name.clone(), pack_type);
                    let analyzed =
                        self.analyze_expression_expected(&expansion.template, element_type);
                    if let Some(expected) = element_type {
                        self.require_type(expected, analyzed.ty, analyzed.span, "array element");
                    } else {
                        element_type = Some(analyzed.ty);
                    }
                    elements.push(analyzed);
                    if let Some(previous) = previous {
                        self.generic_environment
                            .types
                            .insert(expansion.pack.name.clone(), previous);
                    } else {
                        self.generic_environment.types.remove(&expansion.pack.name);
                    }
                }
            } else {
                let analyzed = self.analyze_expression_expected(element, element_type);
                if let Some(expected) = element_type {
                    self.require_type(expected, analyzed.ty, analyzed.span, "array element");
                } else {
                    element_type = Some(analyzed.ty);
                }
                elements.push(analyzed);
            }
        }
        if let Some((_, length)) = expected_shape
            && usize::try_from(length).ok() != Some(elements.len())
        {
            self.diagnostics.push(Diagnostic::error(
                "E3116",
                format!(
                    "array requires {length} element(s), found {}",
                    elements.len()
                ),
                span,
            ));
        }
        let Some(element_type) = element_type else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3117",
                    "cannot infer the element type of an empty array",
                    span,
                )
                .with_help("add an explicit array type annotation"),
            );
            return Expression {
                kind: ExpressionKind::Array(elements),
                ty: Type::Unit,
                span,
            };
        };
        let length = u64::try_from(elements.len()).unwrap_or(u64::MAX);
        let ty = expected
            .filter(|ty| matches!(ty, Type::Array(_)))
            .or_else(|| {
                self.types
                    .intern_array(element_type, length, span, self.diagnostics)
            })
            .unwrap_or(Type::Unit);
        Expression {
            kind: ExpressionKind::Array(elements),
            ty,
            span,
        }
    }

    fn analyze_array_repeat(
        &mut self,
        value: &AstExpression,
        length_expression: &AstExpression,
        span: Span,
        expected: Option<Type>,
    ) -> Expression {
        let length = evaluate_array_length_in(
            length_expression,
            &self.generic_environment,
            self.diagnostics,
        )
        .unwrap_or(0);
        let expected_shape = expected.and_then(|ty| {
            let definition = self.types.definition(ty)?;
            let hir::TypeDefinitionKind::Array {
                element,
                length: expected_length,
            } = definition.kind
            else {
                return None;
            };
            Some((element, expected_length))
        });
        if let Some((_, expected_length)) = expected_shape
            && expected_length != length
        {
            self.diagnostics.push(Diagnostic::error(
                "E3116",
                format!(
                    "array requires {expected_length} element(s), found repeated length {length}"
                ),
                span,
            ));
        }
        let element_type = expected_shape.map(|shape| shape.0);
        let analyzed = self.analyze_expression_expected(value, element_type);
        if let Some(expected_element) = element_type {
            self.require_type(
                expected_element,
                analyzed.ty,
                analyzed.span,
                "repeated array element",
            );
        }
        if !self.types.satisfies_trait(analyzed.ty, "Copy") {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3164",
                    format!(
                        "repeated array element type `{}` does not satisfy `Copy`",
                        analyzed.ty
                    ),
                    analyzed.span,
                )
                .with_help("use an explicit element list or make the element type satisfy `Copy`"),
            );
        }
        let ty = expected
            .filter(|ty| matches!(ty, Type::Array(_)))
            .or_else(|| {
                self.types
                    .intern_array(analyzed.ty, length, span, self.diagnostics)
            })
            .unwrap_or(Type::Unit);
        Expression {
            kind: ExpressionKind::ArrayRepeat {
                value: Box::new(analyzed),
                length,
            },
            ty,
            span,
        }
    }

    fn analyze_struct(
        &mut self,
        structure: &ast::StructExpression,
        expected: Option<Type>,
    ) -> Expression {
        match structure.path.segments.as_slice() {
            [name] => {
                let ty = self
                    .types
                    .names
                    .get(&name.name)
                    .copied()
                    .filter(|ty| matches!(ty, Type::Struct(_)))
                    .or_else(|| {
                        expected.filter(|ty| {
                            matches!(ty, Type::Struct(_))
                                && self
                                    .types
                                    .generic_instance(*ty)
                                    .is_some_and(|instance| instance.base_name == name.name)
                        })
                    });
                let Some(ty) = ty else {
                    self.diagnostics.push(Diagnostic::error(
                        "E3118",
                        format!("`{}` is not a struct type", structure.path.display()),
                        structure.path.span,
                    ));
                    return invalid_composite_expression(structure.span);
                };
                let fields = self.struct_fields(ty);
                self.reject_external_private_construction(ty, &fields, structure.path.span);
                let values = self.analyze_named_fields(&structure.fields, &fields);
                Expression {
                    kind: ExpressionKind::Struct(values),
                    ty,
                    span: structure.span,
                }
            }
            [enum_name, variant_name] => {
                let Some(ty @ Type::Enum(_)) = self.types.names.get(&enum_name.name).copied()
                else {
                    self.diagnostics.push(Diagnostic::error(
                        "E3118",
                        format!("`{}` is not an enum type", enum_name.name),
                        enum_name.span,
                    ));
                    return invalid_composite_expression(structure.span);
                };
                let Some((variant, hir::EnumVariantFields::Struct(fields))) =
                    self.enum_variant(ty, &variant_name.name)
                else {
                    self.diagnostics.push(Diagnostic::error(
                        "E3119",
                        format!(
                            "`{}` is not a struct-like enum variant",
                            structure.path.display()
                        ),
                        structure.path.span,
                    ));
                    return invalid_composite_expression(structure.span);
                };
                self.reject_external_private_construction(ty, &fields, structure.path.span);
                let values = self.analyze_named_fields(&structure.fields, &fields);
                Expression {
                    kind: ExpressionKind::Enum {
                        variant,
                        fields: values,
                    },
                    ty,
                    span: structure.span,
                }
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "E3118",
                    "aggregate construction requires a local type or enum variant path",
                    structure.path.span,
                ));
                invalid_composite_expression(structure.span)
            }
        }
    }

    fn analyze_named_fields(
        &mut self,
        initializers: &[ast::FieldInitializer],
        fields: &[hir::TypeField],
    ) -> Vec<Expression> {
        let mut by_name = HashMap::new();
        for initializer in initializers {
            if by_name
                .insert(initializer.name.name.as_str(), initializer)
                .is_some()
            {
                self.diagnostics.push(Diagnostic::error(
                    "E3120",
                    format!(
                        "field `{}` is initialized more than once",
                        initializer.name.name
                    ),
                    initializer.name.span,
                ));
            }
        }
        for initializer in initializers {
            if !fields
                .iter()
                .any(|field| field.name == initializer.name.name)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E3121",
                    format!("unknown field `{}`", initializer.name.name),
                    initializer.name.span,
                ));
            }
        }
        fields
            .iter()
            .map(|field| {
                let Some(initializer) = by_name.get(field.name.as_str()) else {
                    self.diagnostics.push(Diagnostic::error(
                        "E3122",
                        format!("missing initializer for field `{}`", field.name),
                        field.span,
                    ));
                    return Expression {
                        kind: ExpressionKind::Unit,
                        ty: field.ty,
                        span: field.span,
                    };
                };
                let value = self.analyze_expression_expected(&initializer.value, Some(field.ty));
                self.require_type(field.ty, value.ty, value.span, "field initializer");
                value
            })
            .collect()
    }

    fn analyze_field(&mut self, field: &ast::FieldExpression) -> Expression {
        self.field_base_depth += 1;
        let base = self.analyze_expression_non_consuming(&field.base);
        self.field_base_depth = self.field_base_depth.saturating_sub(1);
        let (base, aggregate_type) =
            if let Some((target, _, false)) = self.types.pointer_shape(base.ty) {
                (
                    Expression {
                        kind: ExpressionKind::Dereference(Box::new(base)),
                        ty: target,
                        span: field.base.span(),
                    },
                    target,
                )
            } else {
                let ty = base.ty;
                (base, ty)
            };
        let selection = match (aggregate_type, &field.field) {
            (Type::Struct(_), ast::FieldName::Named(name)) => self
                .struct_fields(aggregate_type)
                .iter()
                .enumerate()
                .find(|(_, candidate)| candidate.name == name.name)
                .and_then(|(index, candidate)| {
                    u32::try_from(index)
                        .ok()
                        .map(|index| (index, candidate.ty, candidate.is_public))
                }),
            (Type::Tuple(_), ast::FieldName::TupleIndex { index, .. }) => {
                self.tuple_elements(base.ty).and_then(|elements| {
                    usize::try_from(*index)
                        .ok()
                        .and_then(|index| elements.get(index).copied())
                        .map(|ty| (*index, ty, true))
                })
            }
            _ => None,
        };
        let Some((field_index, ty, is_public)) = selection else {
            self.diagnostics.push(Diagnostic::error(
                "E3123",
                "field does not exist on this value",
                field.span,
            ));
            return invalid_composite_expression(field.span);
        };
        if !is_public && self.type_is_external(aggregate_type) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E4005",
                    "field is private to its defining module",
                    field.span,
                )
                .with_help("access it through a public method or function"),
            );
        }
        if self.field_base_depth == 0
            && let Some(place) = self.moved_field_from_field(field)
        {
            if self.consuming_value {
                self.consume_field(place, ty, field.span);
            } else {
                self.require_field_available(
                    &place,
                    field.span,
                    self.place_availability == PlaceAvailability::AllowReinitialization,
                );
            }
        }
        Expression {
            kind: ExpressionKind::Field {
                base: Box::new(base),
                field: field_index,
            },
            ty,
            span: field.span,
        }
    }

    fn analyze_index(&mut self, index: &ast::IndexExpression) -> Expression {
        if self.supports_index_method(&index.base, "index") {
            return self.analyze_index_method_call(index, "index", None);
        }
        let base = self.analyze_expression_non_consuming(&index.base);
        let element = self
            .array_shape(base.ty)
            .map(|(element, _)| element)
            .or_else(|| self.types.slice_shape(base.ty).map(|(element, _)| element))
            .or_else(|| {
                let (target, _, _) = self.types.pointer_shape(base.ty)?;
                self.array_shape(target).map(|(element, _)| element)
            });
        let Some(element) = element else {
            self.diagnostics.push(Diagnostic::error(
                "E3124",
                "indexing requires an array, array reference, or slice",
                index.base.span(),
            ));
            return invalid_composite_expression(index.span);
        };
        if index.indices.len() != 1 {
            self.diagnostics.push(Diagnostic::error(
                "E3125",
                "array indexing requires exactly one index",
                index.span,
            ));
        }
        let Some(index_expression) = index.indices.first() else {
            return invalid_composite_expression(index.span);
        };
        let index_expression =
            self.analyze_expression_expected(index_expression, Some(Type::Usize));
        self.require_type(
            Type::Usize,
            index_expression.ty,
            index_expression.span,
            "array index",
        );
        if self.consuming_value {
            self.consume_expression_root(&index.base, element, index.span);
        }
        Expression {
            kind: ExpressionKind::Index {
                base: Box::new(base),
                index: Box::new(index_expression),
            },
            ty: element,
            span: index.span,
        }
    }

    fn supports_index_method(&self, base: &AstExpression, method: &str) -> bool {
        let Some(receiver_type) = self.place_expression_type(base) else {
            return false;
        };
        let Some(owner) = self.types.nominal_name(receiver_type) else {
            return false;
        };
        let resolved_name = format!("{owner}::{method}");
        if self.signatures.contains_key(&resolved_name) {
            return true;
        }
        let template_name = self.types.generic_instance(receiver_type).map_or_else(
            || resolved_name,
            |instance| format!("{}::{method}", instance.base_name),
        );
        self.generic_functions
            .templates
            .contains_key(&template_name)
    }

    fn analyze_index_method_call(
        &mut self,
        index: &ast::IndexExpression,
        method: &str,
        value: Option<&AstExpression>,
    ) -> Expression {
        let method = ast::Identifier {
            name: method.to_owned(),
            span: index.span,
        };
        let field = ast::FieldExpression {
            base: index.base.clone(),
            field: ast::FieldName::Named(method),
            span: index.span,
        };
        let mut arguments = vec![AstExpression::Array(ast::ArrayExpression {
            kind: ast::ArrayExpressionKind::List(index.indices.clone()),
            span: index.span,
        })];
        if let Some(value) = value {
            arguments.push(value.clone());
        }
        let call = ast::CallExpression {
            callee: AstExpression::Field(Box::new(field.clone())),
            generic_arguments: Vec::new(),
            arguments,
            span: index.span,
        };
        self.analyze_method_call(&call, &field, None)
    }

    fn analyze_path(&mut self, path: &ast::Path, expected: Option<Type>) -> Expression {
        let binding = single_path_name(path).and_then(|name| self.lookup(name));
        if let Some(binding) = binding {
            return self.analyze_local_path(binding, path.span);
        }

        if let Some(value) = single_path_name(path)
            .and_then(|name| self.statics.get(name))
            .cloned()
        {
            return self.analyze_static_path(&value, path, expected);
        }

        if let Some(constant) = self.analyze_const_path(path, expected) {
            return constant;
        }

        if let Some(expected) = expected
            && self.types.intrinsic(expected).is_some()
            && let Some(name) = single_path_name(path)
            && let Some((variant, hir::EnumVariantFields::Unit)) = self.enum_variant(expected, name)
        {
            return Expression {
                kind: ExpressionKind::Enum {
                    variant,
                    fields: Vec::new(),
                },
                ty: expected,
                span: path.span,
            };
        }

        if single_path_name(path) == Some("None") {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3139",
                    "`None` requires an expected `Option<T>` type",
                    path.span,
                )
                .with_help("add an `Option<T>` type annotation or use `None` in a typed context"),
            );
            return invalid_composite_expression(path.span);
        }

        if let [enum_name, variant_name] = path.segments.as_slice()
            && let Some(ty @ Type::Enum(_)) = self.types.names.get(&enum_name.name).copied()
            && let Some((variant, hir::EnumVariantFields::Unit)) =
                self.enum_variant(ty, &variant_name.name)
        {
            return Expression {
                kind: ExpressionKind::Enum {
                    variant,
                    fields: Vec::new(),
                },
                ty,
                span: path.span,
            };
        }

        if let Some(resolved_name) = function_path_name(path)
            && let Some(signature) = self.signatures.get(&resolved_name).cloned()
        {
            self.validate_function_access(path, path.span, &resolved_name, &signature);
            let ty = self
                .types
                .intern_function(
                    signature.parameter_types.clone(),
                    signature.return_type,
                    path.span,
                    self.diagnostics,
                )
                .unwrap_or(Type::Unit);
            if let Some(expected) = expected {
                self.require_type(expected, ty, path.span, "function value");
            }
            return Expression {
                kind: ExpressionKind::Function(signature.id),
                ty,
                span: path.span,
            };
        }

        self.diagnostics.push(
            Diagnostic::error(
                "E3102",
                format!("cannot resolve value `{}`", path.display()),
                path.span,
            )
            .with_help("declare a local binding before using it"),
        );
        Expression {
            kind: ExpressionKind::Unit,
            ty: Type::Unit,
            span: path.span,
        }
    }

    fn analyze_static_path(
        &mut self,
        value: &StaticSymbol,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_static_access(value, path.span);
        if self.consuming_value && !self.types.is_copy(value.ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3144",
                    format!("cannot move a value out of static `{}`", path.display()),
                    path.span,
                )
                .with_help("borrow the static value or copy one of its `Copy` fields"),
            );
        }
        if let Some(expected) = expected {
            self.require_type(expected, value.ty, path.span, "static value");
        }
        Expression {
            kind: ExpressionKind::Static(value.id),
            ty: value.ty,
            span: path.span,
        }
    }

    fn require_static_access(&mut self, value: &StaticSymbol, span: Span) {
        if value.mutable && self.unsafe_depth == 0 {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3156",
                    "mutable static storage can only be accessed inside `unsafe`",
                    span,
                )
                .with_help(
                    "wrap the access in `unsafe { ... }` or use atomics, locks, or an encapsulated synchronization API",
                ),
            );
        }
    }

    fn analyze_local_path(&mut self, binding: Binding, span: Span) -> Expression {
        self.require_local_available(binding, span, self.field_base_depth == 0);
        let borrow_state = self
            .borrow_states
            .get(&binding.local)
            .copied()
            .unwrap_or_default();
        if borrow_state.mutable {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3138",
                    "cannot read a value while it is mutably borrowed",
                    span,
                )
                .with_help("read through the mutable reference instead"),
            );
        }
        let implicit_reborrow = self.reborrow_argument && self.types.is_mutable_view(binding.ty);
        if self.defer_depth > 0 && !self.types.is_copy(binding.ty) {
            self.record_deferred_use(
                binding.local,
                span,
                self.consuming_value && !implicit_reborrow,
            );
        } else if self.consuming_value && !self.types.is_copy(binding.ty) && !implicit_reborrow {
            if borrow_state.mutable || borrow_state.shared != 0 {
                self.diagnostics.push(
                    Diagnostic::error("E3138", "cannot move a value while it is borrowed", span)
                        .with_help("let the reference scope end before moving the value"),
                );
            }
            if self.require_not_reserved_by_defer(binding.local, span) {
                self.moved_locals.insert(binding.local, span);
            }
        }
        Expression {
            kind: ExpressionKind::Local(binding.local),
            ty: binding.ty,
            span,
        }
    }

    fn analyze_const_path(
        &mut self,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Option<Expression> {
        let name = single_path_name(path)?;
        if let Some(value) = self.generic_environment.constants.get(name).copied() {
            return Some(self.analyze_integer_literal(
                &ast::IntegerLiteral {
                    value: u128::from(value),
                    span: path.span,
                },
                expected.or(Some(Type::Usize)),
            ));
        }
        let mut constant = self.types.constants.get(name)?.clone();
        constant.span = path.span;
        if let Some(expected) = expected {
            self.require_type(expected, constant.ty, path.span, "constant value");
        }
        Some(constant)
    }

    fn require_local_available(
        &mut self,
        binding: Binding,
        span: Span,
        include_partial_moves: bool,
    ) {
        let moved_at = self.moved_locals.get(&binding.local).copied().or_else(|| {
            if !include_partial_moves {
                return None;
            }
            self.moved_fields
                .iter()
                .find(|(field, _)| field.local == binding.local)
                .map(|(_, moved_at)| *moved_at)
        });
        let Some(moved_at) = moved_at else {
            return;
        };
        self.diagnostics.push(
            Diagnostic::error("E3143", "use of a value after it was moved", span).with_help(
                format!(
                    "the earlier consuming use starts at source byte {}; borrow the value or create a new owned value",
                    moved_at.start
                ),
            ),
        );
    }

    fn record_deferred_use(&mut self, local: LocalId, span: Span, consuming: bool) {
        if let Some(existing) = self.deferred_uses.get(&local).copied() {
            if consuming {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3147",
                        "deferred action would consume a value needed by another deferred action",
                        span,
                    )
                    .with_help(format!(
                        "the earlier deferred use starts at source byte {}; register the consuming cleanup first",
                        existing.span.start
                    )),
                );
            }
            return;
        }
        self.deferred_uses
            .insert(local, DeferredUse { span, consuming });
        if let Some(scope) = self.deferred_use_scopes.last_mut() {
            scope.push(local);
        }
    }

    fn require_not_reserved_by_defer(&mut self, local: LocalId, span: Span) -> bool {
        let Some(deferred) = self.deferred_uses.get(&local).copied() else {
            return true;
        };
        self.diagnostics.push(
            Diagnostic::error(
                "E3147",
                "cannot invalidate a value that is reserved by a deferred action",
                span,
            )
            .with_help(format!(
                "the deferred {} starts at source byte {}; borrow the value until the scope exits",
                if deferred.consuming { "cleanup" } else { "use" },
                deferred.span.start
            )),
        );
        false
    }

    fn consume_expression_root(&mut self, expression: &AstExpression, ty: Type, span: Span) {
        if self.types.is_copy(ty) {
            return;
        }
        let Some(path) = assignment_root_path(expression) else {
            return;
        };
        let Some(name) = single_path_name(path) else {
            return;
        };
        if self.statics.contains_key(name) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3144",
                    format!("cannot move a value out of static `{name}`"),
                    span,
                )
                .with_help("borrow the static value or copy one of its `Copy` fields"),
            );
            return;
        }
        let Some(binding) = self.lookup(name) else {
            return;
        };
        self.require_local_available(binding, span, true);
        if self.defer_depth > 0 {
            self.record_deferred_use(binding.local, span, true);
        } else if self.require_not_reserved_by_defer(binding.local, span) {
            self.moved_locals.insert(binding.local, span);
        }
    }

    fn moved_field_from_field(&self, field: &ast::FieldExpression) -> Option<MovedField> {
        let (local, mut projections) = self.field_place(&field.base)?;
        projections.push(match &field.field {
            ast::FieldName::Named(name) => FieldProjection::Named(name.name.clone()),
            ast::FieldName::TupleIndex { index, .. } => FieldProjection::Tuple(*index),
        });
        Some(MovedField { local, projections })
    }

    fn field_place(&self, expression: &AstExpression) -> Option<(LocalId, Vec<FieldProjection>)> {
        match expression {
            AstExpression::Path(path) => {
                let binding = single_path_name(path).and_then(|name| self.lookup(name))?;
                Some((binding.local, Vec::new()))
            }
            AstExpression::Field(field) => {
                let (local, mut projections) = self.field_place(&field.base)?;
                projections.push(match &field.field {
                    ast::FieldName::Named(name) => FieldProjection::Named(name.name.clone()),
                    ast::FieldName::TupleIndex { index, .. } => FieldProjection::Tuple(*index),
                });
                Some((local, projections))
            }
            _ => None,
        }
    }

    fn consume_field(&mut self, place: MovedField, ty: Type, span: Span) {
        self.require_field_available(&place, span, false);
        if self.types.is_copy(ty) {
            return;
        }
        if self.defer_depth > 0 {
            self.record_deferred_use(place.local, span, true);
        } else if self.require_not_reserved_by_defer(place.local, span) {
            self.moved_fields.insert(place, span);
        }
    }

    fn require_field_available(
        &mut self,
        place: &MovedField,
        span: Span,
        allow_reinitialization: bool,
    ) {
        let moved_at = self.moved_locals.get(&place.local).copied().or_else(|| {
            self.moved_fields.iter().find_map(|(moved, moved_at)| {
                if moved.local != place.local {
                    return None;
                }
                let moved_is_parent = place.projections.starts_with(&moved.projections);
                let requested_is_parent = moved.projections.starts_with(&place.projections);
                let unavailable = if allow_reinitialization {
                    moved_is_parent && moved.projections != place.projections
                } else {
                    moved_is_parent || requested_is_parent
                };
                unavailable.then_some(*moved_at)
            })
        });
        let Some(moved_at) = moved_at else {
            return;
        };
        self.diagnostics.push(
            Diagnostic::error("E3143", "use of a value after it was moved", span).with_help(
                format!(
                    "the earlier consuming use starts at source byte {}; borrow the value or create a new owned value",
                    moved_at.start
                ),
            ),
        );
    }

    fn restore_field(&mut self, place: &MovedField) {
        self.moved_fields.retain(|moved, _| {
            moved.local != place.local || !moved.projections.starts_with(&place.projections)
        });
    }

    fn require_expression_root_available(&mut self, expression: &AstExpression, span: Span) {
        let Some(path) = assignment_root_path(expression) else {
            return;
        };
        let Some(binding) = single_path_name(path).and_then(|name| self.lookup(name)) else {
            return;
        };
        self.require_local_available(binding, span, true);
    }

    fn analyze_unary(
        &mut self,
        expression: &ast::UnaryExpression,
        expected: Option<Type>,
    ) -> Expression {
        match expression.operator {
            AstUnaryOperator::Borrow | AstUnaryOperator::BorrowMut => {
                return self.analyze_borrow(expression, expected);
            }
            AstUnaryOperator::Dereference => return self.analyze_dereference(expression),
            AstUnaryOperator::Negate | AstUnaryOperator::Not => {}
        }
        if expression.operator == AstUnaryOperator::Negate
            && let AstExpression::Integer(literal) = &expression.operand
        {
            let ty = expected
                .filter(|ty| ty.is_signed_integer())
                .unwrap_or(Type::I32);
            let minimum_magnitude = integer_minimum_magnitude(ty);
            if literal.value == minimum_magnitude {
                return Expression {
                    kind: ExpressionKind::Integer(minimum_magnitude),
                    ty,
                    span: expression.span,
                };
            }
            if literal.value > minimum_magnitude {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3104",
                        format!("negative integer literal does not fit in `{ty}`"),
                        expression.span,
                    )
                    .with_help(format!(
                        "use a magnitude no greater than {minimum_magnitude}"
                    )),
                );
                return Expression {
                    kind: ExpressionKind::Integer(0),
                    ty,
                    span: expression.span,
                };
            }
        }

        let operand = self.analyze_expression_expected(&expression.operand, expected);
        let (operator, valid) = match expression.operator {
            AstUnaryOperator::Negate => (
                UnaryOperator::Negate,
                operand.ty.is_signed_integer() || operand.ty.is_float(),
            ),
            AstUnaryOperator::Not => (
                UnaryOperator::Not,
                operand.ty == Type::Bool || operand.ty.is_integer(),
            ),
            AstUnaryOperator::Borrow
            | AstUnaryOperator::BorrowMut
            | AstUnaryOperator::Dereference => {
                unreachable!("indirection operators return before scalar unary analysis")
            }
        };
        if !valid && operand.ty != Type::Never {
            self.invalid_operator("unary", operand.ty, expression.span);
        }
        let result = operand.ty;
        Expression {
            kind: ExpressionKind::Unary {
                operator,
                operand: Box::new(operand),
            },
            ty: result,
            span: expression.span,
        }
    }

    fn analyze_borrow(
        &mut self,
        expression: &ast::UnaryExpression,
        expected: Option<Type>,
    ) -> Expression {
        let mutable = expression.operator == AstUnaryOperator::BorrowMut;
        let Some(place) = self.analyze_place(&expression.operand) else {
            return invalid_composite_expression(expression.span);
        };
        self.require_expression_root_available(&expression.operand, expression.span);
        if mutable {
            self.require_mutable_place(&expression.operand);
        }
        self.check_and_record_borrow(&expression.operand, mutable, expression.span);
        if let Some(expected) = expected
            && let Some((element, slice_mutable)) = self.types.slice_shape(expected)
            && let Some((actual_element, length)) = self.array_shape(place.ty)
        {
            if element != actual_element || (slice_mutable && !mutable) {
                self.type_mismatch(expected, place.ty, expression.span, "slice borrow");
            }
            return Expression {
                kind: ExpressionKind::Borrow {
                    place,
                    mutable: slice_mutable,
                    slice_length: Some(length),
                },
                ty: expected,
                span: expression.span,
            };
        }
        let ty = self
            .types
            .intern_reference(place.ty, mutable, expression.span, self.diagnostics)
            .unwrap_or(Type::Unit);
        if let Some(expected) = expected {
            self.require_type(expected, ty, expression.span, "borrow expression");
        }
        Expression {
            kind: ExpressionKind::Borrow {
                place,
                mutable,
                slice_length: None,
            },
            ty,
            span: expression.span,
        }
    }

    fn check_and_record_borrow(&mut self, operand: &AstExpression, mutable: bool, span: Span) {
        let Some(path) = assignment_root_path(operand) else {
            return;
        };
        let Some(binding) = single_path_name(path).and_then(|name| self.lookup(name)) else {
            return;
        };
        let state = self
            .borrow_states
            .get(&binding.local)
            .copied()
            .unwrap_or_default();
        let conflicts = if mutable {
            state.mutable || state.shared != 0
        } else {
            state.mutable
        };
        if conflicts {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3138",
                    "borrow conflicts with an active scoped borrow",
                    span,
                )
                .with_help("end the earlier reference's scope before borrowing again"),
            );
            return;
        }
        if !self.persistent_borrow {
            return;
        }
        let state = self.borrow_states.entry(binding.local).or_default();
        if mutable {
            state.mutable = true;
        } else {
            state.shared = state.shared.saturating_add(1);
        }
        if let Some(scope) = self.borrow_scopes.last_mut() {
            scope.push((binding.local, mutable));
        }
    }

    fn analyze_dereference(&mut self, expression: &ast::UnaryExpression) -> Expression {
        let pointer = self.analyze_expression_non_consuming(&expression.operand);
        let Some((target, _, is_raw)) = self.types.pointer_shape(pointer.ty) else {
            self.diagnostics.push(Diagnostic::error(
                "E3135",
                format!("cannot dereference `{}`", pointer.ty),
                expression.span,
            ));
            return invalid_composite_expression(expression.span);
        };
        if is_raw && self.unsafe_depth == 0 {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3136",
                    "raw pointers can only be dereferenced inside `unsafe`",
                    expression.span,
                )
                .with_help("wrap the operation in `unsafe { ... }`"),
            );
        }
        if self.consuming_value && !self.types.is_copy(target) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3144",
                    "cannot move an owned value through a reference or raw pointer",
                    expression.span,
                )
                .with_help("borrow the pointee or move its owning local instead"),
            );
        }
        Expression {
            kind: ExpressionKind::Dereference(Box::new(pointer)),
            ty: target,
            span: expression.span,
        }
    }

    fn analyze_binary(
        &mut self,
        expression: &ast::BinaryExpression,
        expected: Option<Type>,
    ) -> Expression {
        let logical = matches!(
            expression.operator,
            AstBinaryOperator::And | AstBinaryOperator::Or
        );
        let preferred = if logical { Some(Type::Bool) } else { expected };
        let left = self.analyze_expression_expected(&expression.left, preferred);
        let right = self.analyze_expression_expected(&expression.right, Some(left.ty));
        if left.ty != right.ty && left.ty != Type::Never && right.ty != Type::Never {
            self.type_mismatch(
                left.ty,
                right.ty,
                expression.right.span(),
                "binary operands",
            );
        }

        let (operator, valid, result_type) = match expression.operator {
            AstBinaryOperator::Add => (BinaryOperator::Add, left.ty.is_numeric(), left.ty),
            AstBinaryOperator::Subtract => {
                (BinaryOperator::Subtract, left.ty.is_numeric(), left.ty)
            }
            AstBinaryOperator::Multiply => {
                (BinaryOperator::Multiply, left.ty.is_numeric(), left.ty)
            }
            AstBinaryOperator::Divide => (BinaryOperator::Divide, left.ty.is_numeric(), left.ty),
            AstBinaryOperator::Remainder => {
                (BinaryOperator::Remainder, left.ty.is_integer(), left.ty)
            }
            AstBinaryOperator::BitAnd => (BinaryOperator::BitAnd, left.ty.is_integer(), left.ty),
            AstBinaryOperator::BitXor => (BinaryOperator::BitXor, left.ty.is_integer(), left.ty),
            AstBinaryOperator::BitOr => (BinaryOperator::BitOr, left.ty.is_integer(), left.ty),
            AstBinaryOperator::ShiftLeft => {
                (BinaryOperator::ShiftLeft, left.ty.is_integer(), left.ty)
            }
            AstBinaryOperator::ShiftRight => {
                (BinaryOperator::ShiftRight, left.ty.is_integer(), left.ty)
            }
            AstBinaryOperator::Equal => (
                BinaryOperator::Equal,
                self.types.satisfies_trait(left.ty, "Eq"),
                Type::Bool,
            ),
            AstBinaryOperator::NotEqual => (
                BinaryOperator::NotEqual,
                self.types.satisfies_trait(left.ty, "Eq"),
                Type::Bool,
            ),
            AstBinaryOperator::Less => (BinaryOperator::Less, is_ordered_type(left.ty), Type::Bool),
            AstBinaryOperator::LessEqual => (
                BinaryOperator::LessEqual,
                is_ordered_type(left.ty),
                Type::Bool,
            ),
            AstBinaryOperator::Greater => (
                BinaryOperator::Greater,
                is_ordered_type(left.ty),
                Type::Bool,
            ),
            AstBinaryOperator::GreaterEqual => (
                BinaryOperator::GreaterEqual,
                is_ordered_type(left.ty),
                Type::Bool,
            ),
            AstBinaryOperator::And => (BinaryOperator::And, left.ty == Type::Bool, Type::Bool),
            AstBinaryOperator::Or => (BinaryOperator::Or, left.ty == Type::Bool, Type::Bool),
        };
        if !valid && left.ty != Type::Never {
            self.invalid_operator("binary", left.ty, expression.span);
        }
        Expression {
            kind: ExpressionKind::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            },
            ty: result_type,
            span: expression.span,
        }
    }

    fn analyze_call(&mut self, call: &ast::CallExpression, expected: Option<Type>) -> Expression {
        if let AstExpression::Field(field) = &call.callee
            && matches!(field.field, ast::FieldName::Named(_))
        {
            return self.analyze_method_call(call, field, expected);
        }
        let AstExpression::Path(path) = &call.callee else {
            return self.analyze_indirect_call(call, expected);
        };
        if single_path_name(path).is_some_and(|name| self.lookup(name).is_some()) {
            return self.analyze_indirect_call(call, expected);
        }
        if let Some(expression) = self.analyze_default_call(call, path) {
            return expression;
        }
        if single_path_name(path) == Some("panic") {
            return self.analyze_panic_call(call, path);
        }
        if let Some(mode) = assertion_mode(path) {
            return self.analyze_assert_call(call, path, mode);
        }
        if let Some(expression) = self.analyze_runtime_intrinsic_call(call, path, expected) {
            return expression;
        }
        if let Some(expression) = self.analyze_intrinsic_call(call, path, expected) {
            return expression;
        }
        if let [enum_name, variant_name] = path.segments.as_slice()
            && let Some(ty @ Type::Enum(_)) = self.types.names.get(&enum_name.name).copied()
            && self.enum_variant(ty, &variant_name.name).is_some()
        {
            return self.analyze_enum_tuple_call(call, path, ty, &variant_name.name);
        }
        self.analyze_function_call(call, path, expected)
    }

    fn analyze_indirect_call(
        &mut self,
        call: &ast::CallExpression,
        _expected_return: Option<Type>,
    ) -> Expression {
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    "function values do not accept explicit generic arguments",
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        let callee = self.analyze_expression(&call.callee);
        let Some((parameter_types, return_type)) = self
            .types
            .function_shape(callee.ty)
            .map(|(parameters, return_type)| (parameters.to_vec(), return_type))
        else {
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            self.diagnostics.push(
                Diagnostic::error(
                    "E3106",
                    format!("value of type `{}` is not callable", callee.ty),
                    call.callee.span(),
                )
                .with_help("call a function path or a value with type `fn(...) -> ...`"),
            );
            return invalid_composite_expression(call.span);
        };
        let signature = Signature {
            id: FunctionId(u32::MAX),
            parameter_types,
            return_type,
            requires_unsafe: false,
            is_public: true,
        };
        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow |= self.types.is_scoped(return_type);
        let arguments = self.analyze_typed_call_arguments(&call.arguments, &signature);
        self.persistent_borrow = previous_persistence;
        self.validate_call_arguments("function value", call.span, &signature, &arguments);
        Expression {
            kind: ExpressionKind::IndirectCall {
                callee: Box::new(callee),
                arguments,
            },
            ty: return_type,
            span: call.span,
        }
    }

    fn analyze_method_call(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        expected_return: Option<Type>,
    ) -> Expression {
        let ast::FieldName::Named(method) = &field.field else {
            return invalid_composite_expression(call.span);
        };
        if let Some(expression) =
            self.analyze_builtin_method_call(call, field, method, expected_return)
        {
            return expression;
        }
        let place_receiver_type = self.place_expression_type(&field.base);
        let Some(receiver_type) =
            place_receiver_type.or_else(|| self.temporary_method_receiver_type(&field.base))
        else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6002",
                    "cannot determine the method receiver type",
                    field.base.span(),
                )
                .with_help("bind the value to a typed local before calling its method"),
            );
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            return invalid_composite_expression(call.span);
        };
        let Some(owner) = self.types.nominal_name(receiver_type).map(str::to_owned) else {
            self.diagnostics.push(Diagnostic::error(
                "E6002",
                format!("type `{receiver_type}` has no inherent methods"),
                field.span,
            ));
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            return invalid_composite_expression(call.span);
        };
        let resolved_name = format!("{owner}::{}", method.name);
        let signature = self.signatures.get(&resolved_name).cloned();
        if signature.is_none()
            && let Some(expression) = self.try_analyze_generic_method_call(
                call,
                field,
                method,
                receiver_type,
                &resolved_name,
                expected_return,
            )
        {
            return expression;
        }
        if signature.is_none()
            && method.name == "clone"
            && self.types.satisfies_trait(receiver_type, "Clone")
        {
            self.validate_derived_clone_call(call);
            let mut cloned = self.analyze_expression_non_consuming(&field.base);
            cloned.span = call.span;
            return cloned;
        }
        let Some(signature) = signature else {
            self.diagnostics.push(Diagnostic::error(
                "E6002",
                format!("method `{}` does not exist on `{owner}`", method.name),
                method.span,
            ));
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            return invalid_composite_expression(call.span);
        };
        if place_receiver_type.is_none()
            && signature
                .parameter_types
                .first()
                .is_some_and(|receiver| self.types.pointer_shape(*receiver).is_some())
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6002",
                    "borrowed method receiver must be an addressable value",
                    field.base.span(),
                )
                .with_help("bind the value to a local before calling the borrowed method"),
            );
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            return invalid_composite_expression(call.span);
        }
        self.analyze_resolved_method_call(call, field, receiver_type, &resolved_name, &signature)
    }

    fn validate_derived_clone_call(&mut self, call: &ast::CallExpression) {
        if !call.arguments.is_empty() || !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E7007",
                    "derived `clone` does not accept arguments",
                    call.span,
                )
                .with_help("write `value.clone()`"),
            );
        }
        for argument in &call.arguments {
            self.analyze_expression(argument);
        }
    }

    fn analyze_builtin_method_call(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        if let Some(expression) =
            self.analyze_tuple_type_access_method(call, field, method, expected_return)
        {
            return Some(expression);
        }
        if let Some(expression) =
            self.analyze_format_push_method(call, field, method, expected_return)
        {
            return Some(expression);
        }
        if let Some(expression) =
            self.analyze_string_iteration_method(call, field, method, expected_return)
        {
            return Some(expression);
        }
        if let Some(expression) =
            self.analyze_chars_next_method(call, field, method, expected_return)
        {
            return Some(expression);
        }
        if let Some(expression) =
            self.analyze_integer_addition_method(call, field, method, expected_return)
        {
            return Some(expression);
        }
        self.analyze_slice_access_method(call, field, method, expected_return)
    }

    fn analyze_tuple_type_access_method(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        if method.name == "assert_unique_types" {
            return self.analyze_tuple_unique_types(call, field);
        }
        let (mutable_first, returns_tuple) = match method.name.as_str() {
            "get_type" => (false, false),
            "get_type_mut" => (true, false),
            "split_type_mut" => (true, true),
            _ => return None,
        };
        let receiver_type = self.place_expression_type(&field.base)?;
        let tuple_elements = self.tuple_elements(receiver_type)?;
        if !call.arguments.is_empty() {
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            self.diagnostics.push(
                Diagnostic::error(
                    "E6022",
                    format!("`{}` accepts only compile-time type arguments", method.name),
                    call.span,
                )
                .with_help(format!("write `tuple.{}<Type>()`", method.name)),
            );
            return Some(invalid_composite_expression(call.span));
        }
        let Some(requested) = self.resolve_tuple_type_requests(call, method, returns_tuple) else {
            return Some(invalid_composite_expression(call.span));
        };

        let Some(selected) = self.select_tuple_type_fields(&tuple_elements, &requested, call.span)
        else {
            return Some(invalid_composite_expression(call.span));
        };

        self.require_expression_root_available(&field.base, call.span);
        if mutable_first {
            self.require_mutable_place(&field.base);
        }
        self.check_and_record_borrow(&field.base, mutable_first, call.span);

        let Some(borrowed) =
            self.borrow_tuple_type_fields(field, &selected, mutable_first, call.span)
        else {
            return Some(invalid_composite_expression(call.span));
        };
        if !returns_tuple {
            let mut result = borrowed
                .into_iter()
                .next()
                .unwrap_or_else(|| invalid_composite_expression(call.span));
            if let Some(expected) = expected_return {
                self.require_type(expected, result.ty, call.span, "tuple type access");
            }
            result.span = call.span;
            return Some(result);
        }
        let result_type = self
            .types
            .intern_tuple(
                borrowed.iter().map(|value| value.ty).collect(),
                call.span,
                self.diagnostics,
            )
            .unwrap_or(Type::Unit);
        if let Some(expected) = expected_return {
            self.require_type(expected, result_type, call.span, "tuple type split");
        }
        Some(Expression {
            kind: ExpressionKind::Tuple(borrowed),
            ty: result_type,
            span: call.span,
        })
    }

    fn analyze_tuple_unique_types(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
    ) -> Option<Expression> {
        let receiver_type = self.place_expression_type(&field.base)?;
        let tuple_elements = self.tuple_elements(receiver_type)?;
        if !call.arguments.is_empty() || !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6022",
                    "`assert_unique_types` does not accept arguments",
                    call.span,
                )
                .with_help("write `tuple.assert_unique_types()`"),
            );
        }
        let mut seen = HashSet::new();
        for element in tuple_elements {
            if !seen.insert(element) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6022",
                        format!("tuple contains type `{element}` more than once"),
                        call.span,
                    )
                    .with_help("heterogeneous type-addressable tuples require unique types"),
                );
            }
        }
        Some(Expression {
            kind: ExpressionKind::Unit,
            ty: Type::Unit,
            span: call.span,
        })
    }

    fn resolve_tuple_type_requests(
        &mut self,
        call: &ast::CallExpression,
        method: &ast::Identifier,
        returns_tuple: bool,
    ) -> Option<Vec<Type>> {
        let values = self.types.resolve_generic_argument_values(
            &call.generic_arguments,
            &self.generic_environment,
            self.diagnostics,
        )?;
        let Some(requested) = values
            .into_iter()
            .map(|value| match value {
                GenericValue::Type(ty) => Some(ty),
                GenericValue::Const(_) => None,
            })
            .collect::<Option<Vec<_>>>()
        else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6022",
                    "tuple type access accepts only type arguments",
                    call.span,
                )
                .with_help("remove const arguments from the method call"),
            );
            return None;
        };
        let expected_count = if returns_tuple { None } else { Some(1) };
        if requested.is_empty() || expected_count.is_some_and(|count| requested.len() != count) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6022",
                    if returns_tuple {
                        "`split_type_mut` requires at least one requested type".to_owned()
                    } else {
                        format!("`{}` requires exactly one requested type", method.name)
                    },
                    call.span,
                )
                .with_help("provide the requested tuple element types between `<` and `>`"),
            );
            return None;
        }
        Some(requested)
    }

    fn select_tuple_type_fields(
        &mut self,
        tuple_elements: &[Type],
        requested: &[Type],
        span: Span,
    ) -> Option<Vec<usize>> {
        let mut selected = Vec::with_capacity(requested.len());
        let mut selected_set = HashSet::with_capacity(requested.len());
        for requested_type in requested {
            let mut matched = None;
            let mut duplicate = false;
            for (index, element) in tuple_elements.iter().enumerate() {
                if element == requested_type {
                    duplicate = matched.replace(index).is_some();
                    if duplicate {
                        break;
                    }
                }
            }
            let Some(index) = matched else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6022",
                        format!("tuple does not contain `{requested_type}`"),
                        span,
                    )
                    .with_help("each type-addressable tuple element must be unique"),
                );
                return None;
            };
            if duplicate {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6022",
                        format!("tuple contains `{requested_type}` more than once"),
                        span,
                    )
                    .with_help("each type-addressable tuple element must be unique"),
                );
                return None;
            }
            if !selected_set.insert(index) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6022",
                        format!("type `{requested_type}` is requested more than once"),
                        span,
                    )
                    .with_help("request each tuple element at most once"),
                );
                return None;
            }
            selected.push(index);
        }
        Some(selected)
    }

    fn borrow_tuple_type_fields(
        &mut self,
        field: &ast::FieldExpression,
        selected: &[usize],
        mutable_first: bool,
        span: Span,
    ) -> Option<Vec<Expression>> {
        let mut borrowed = Vec::with_capacity(selected.len());
        for (request_index, field_index) in selected.iter().copied().enumerate() {
            let field_number = u32::try_from(field_index).ok()?;
            let element_expression = AstExpression::Field(Box::new(ast::FieldExpression {
                base: field.base.clone(),
                field: ast::FieldName::TupleIndex {
                    index: field_number,
                    span,
                },
                span,
            }));
            let place = self.analyze_place(&element_expression)?;
            let mutable = mutable_first && request_index == 0;
            let reference_type = self
                .types
                .intern_reference(place.ty, mutable, span, self.diagnostics)
                .unwrap_or(Type::Unit);
            borrowed.push(Expression {
                kind: ExpressionKind::Borrow {
                    place,
                    mutable,
                    slice_length: None,
                },
                ty: reference_type,
                span,
            });
        }
        Some(borrowed)
    }

    fn analyze_format_push_method(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        _expected_return: Option<Type>,
    ) -> Option<Expression> {
        if method.name != "push_format" {
            return None;
        }
        let receiver_type = self.place_expression_type(&field.base)?;
        if self.types.nominal_name(receiver_type) != Some(STANDARD_STRING_TYPE) {
            return None;
        }
        let resolved_name = format!("{STANDARD_STRING_TYPE}::push_format");
        let signature = self.signatures.get(&resolved_name).cloned()?;
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    "`push_format` does not accept generic arguments",
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        let [AstExpression::FormattedString(formatted)] = call.arguments.as_slice() else {
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            self.diagnostics.push(
                Diagnostic::error(
                    "E3160",
                    "`push_format` expects one `f\"...\"` expression",
                    call.span,
                )
                .with_help("write `string.push_format(f\"value: {value}\")`"),
            );
            return Some(invalid_composite_expression(call.span));
        };

        let mut calls = Vec::with_capacity(formatted.fragments.len().max(1));
        for fragment in &formatted.fragments {
            match fragment {
                ast::FormattedStringFragment::Text(literal) => {
                    calls.push(self.analyze_string_push_call(
                        field,
                        receiver_type,
                        "push_str",
                        &AstExpression::String(literal.clone()),
                        signature.return_type,
                        literal.span,
                    ));
                }
                ast::FormattedStringFragment::Display(value) => {
                    if let Some(expression) = self.analyze_formatted_value(
                        field,
                        receiver_type,
                        value,
                        signature.return_type,
                        FormattingMode::Display,
                    ) {
                        calls.push(expression);
                    }
                }
                ast::FormattedStringFragment::Debug(value) => {
                    if let Some(expression) = self.analyze_formatted_value(
                        field,
                        receiver_type,
                        value,
                        signature.return_type,
                        FormattingMode::Debug,
                    ) {
                        calls.push(expression);
                    }
                }
            }
        }
        if calls.is_empty() {
            calls.push(self.analyze_string_push_call(
                field,
                receiver_type,
                "push_str",
                &AstExpression::String(ast::StringLiteral {
                    value: String::new(),
                    span: formatted.span,
                }),
                signature.return_type,
                formatted.span,
            ));
        }
        let success_variant = self
            .enum_variant(signature.return_type, "Ok")
            .map_or(0, |(variant, _)| variant);
        Some(Expression {
            kind: ExpressionKind::FormatPush {
                function: signature.id,
                calls,
                success_variant,
            },
            ty: signature.return_type,
            span: call.span,
        })
    }

    fn analyze_formatted_value(
        &mut self,
        field: &ast::FieldExpression,
        receiver_type: Type,
        value: &AstExpression,
        result_type: Type,
        mode: FormattingMode,
    ) -> Option<Expression> {
        let place_type = self.place_expression_type(value);
        let mut analyzed = None;
        let value_type = place_type.unwrap_or_else(|| {
            let expression = self.analyze_expression_non_consuming(value);
            let ty = expression.ty;
            analyzed = Some(expression);
            ty
        });
        let method = if self.types.nominal_name(value_type) == Some(STANDARD_STRING_TYPE) {
            Some("push_string")
        } else {
            string_format_method(value_type)
        };
        if let Some(method) = method {
            let argument = if value_type == Type::Str
                || value_type.is_numeric()
                || matches!(value_type, Type::Bool | Type::Char)
            {
                analyzed.unwrap_or_else(|| self.analyze_expression(value))
            } else {
                let borrow = ast::UnaryExpression {
                    operator: AstUnaryOperator::Borrow,
                    operand: value.clone(),
                    span: value.span(),
                };
                self.analyze_borrow(&borrow, None)
            };
            return Some(self.build_string_push_call(
                field,
                receiver_type,
                method,
                argument,
                result_type,
                value.span(),
            ));
        }

        let (trait_name, append_function, trait_label) = match mode {
            FormattingMode::Display => (STANDARD_DISPLAY_TRAIT, STANDARD_APPEND_DISPLAY, "Display"),
            FormattingMode::Debug => (STANDARD_DEBUG_TRAIT, STANDARD_APPEND_DEBUG, "Debug"),
        };
        if self.types.satisfies_trait(value_type, trait_name) {
            if place_type.is_none() {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3162",
                        "formatted owned values must be bound before borrowing",
                        value.span(),
                    )
                    .with_help("bind the value to a local, then interpolate that local"),
                );
                return None;
            }
            return Some(self.analyze_trait_append_call(
                field,
                value,
                result_type,
                append_function,
            ));
        }

        let value_type_name = self.types.reflection_type_name(value_type);
        self.diagnostics.push(
            Diagnostic::error(
                "E3161",
                format!("type `{value_type_name}` cannot be interpolated"),
                value.span(),
            )
            .with_help(format!(
                "implement `std::fmt::{trait_label}` for this nominal type"
            )),
        );
        None
    }

    fn analyze_string_push_call(
        &mut self,
        field: &ast::FieldExpression,
        receiver_type: Type,
        method: &str,
        value: &AstExpression,
        result_type: Type,
        span: Span,
    ) -> Expression {
        let analyzed = self.analyze_expression(value);
        self.build_string_push_call(field, receiver_type, method, analyzed, result_type, span)
    }

    fn build_string_push_call(
        &mut self,
        field: &ast::FieldExpression,
        receiver_type: Type,
        method: &str,
        value: Expression,
        result_type: Type,
        span: Span,
    ) -> Expression {
        let resolved_name = format!("{STANDARD_STRING_TYPE}::{method}");
        let Some(signature) = self.signatures.get(&resolved_name).cloned() else {
            self.diagnostics.push(Diagnostic::error(
                "E3163",
                format!("standard formatting method `{method}` is unavailable"),
                span,
            ));
            return invalid_composite_expression(span);
        };
        let Some(expected_receiver) = signature.parameter_types.first().copied() else {
            return invalid_composite_expression(span);
        };
        let receiver = self.analyze_method_receiver(&field.base, receiver_type, expected_receiver);
        let arguments = vec![receiver, value];
        self.validate_call_arguments(&resolved_name, span, &signature, &arguments);
        self.require_type(result_type, signature.return_type, span, "format write");
        Expression {
            kind: ExpressionKind::Call {
                function: signature.id,
                arguments,
            },
            ty: signature.return_type,
            span,
        }
    }

    fn analyze_trait_append_call(
        &mut self,
        field: &ast::FieldExpression,
        value: &AstExpression,
        result_type: Type,
        append_function: &str,
    ) -> Expression {
        let span = Span::new(field.base.span().start, value.span().end);
        let call = ast::CallExpression {
            callee: AstExpression::Path(ast::Path {
                segments: vec![ast::Identifier {
                    name: append_function.to_owned(),
                    span,
                }],
                span,
            }),
            generic_arguments: Vec::new(),
            arguments: vec![
                AstExpression::Unary(Box::new(ast::UnaryExpression {
                    operator: AstUnaryOperator::BorrowMut,
                    operand: field.base.clone(),
                    span: field.base.span(),
                })),
                AstExpression::Unary(Box::new(ast::UnaryExpression {
                    operator: AstUnaryOperator::Borrow,
                    operand: value.clone(),
                    span: value.span(),
                })),
            ],
            span,
        };
        self.analyze_call(&call, Some(result_type))
    }

    fn analyze_integer_addition_method(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        let mode = match method.name.as_str() {
            "wrapping_add" => IntegerAdditionMode::Wrapping,
            "checked_add" => IntegerAdditionMode::Checked,
            "saturating_add" => IntegerAdditionMode::Saturating,
            _ => return None,
        };
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!("integer method `{}` is not generic", method.name),
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        if call.arguments.len() != 1 {
            self.diagnostics.push(Diagnostic::error(
                "E3105",
                format!(
                    "integer method `{}` expects 1 argument(s), but {} were provided",
                    method.name,
                    call.arguments.len()
                ),
                call.span,
            ));
        }

        let expected_receiver = match mode {
            IntegerAdditionMode::Checked => expected_return.and_then(|expected| {
                let IntrinsicType::Option { value } = self.types.intrinsic(expected)? else {
                    return None;
                };
                value.is_integer().then_some(value)
            }),
            IntegerAdditionMode::Wrapping | IntegerAdditionMode::Saturating => {
                expected_return.filter(|expected| expected.is_integer())
            }
        };
        let left = self.analyze_expression_expected(&field.base, expected_receiver);
        let right = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(left.ty)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        if !left.ty.is_integer() {
            self.diagnostics.push(Diagnostic::error(
                "E6002",
                format!(
                    "method `{}` does not exist on non-integer type `{}`",
                    method.name, left.ty
                ),
                method.span,
            ));
            return Some(invalid_composite_expression(call.span));
        }
        self.require_type(left.ty, right.ty, right.span, "integer method argument");
        let result_type = match mode {
            IntegerAdditionMode::Checked => self
                .types
                .intern_option(left.ty, call.span, self.diagnostics)
                .unwrap_or(Type::Unit),
            IntegerAdditionMode::Wrapping | IntegerAdditionMode::Saturating => left.ty,
        };
        if let Some(expected) = expected_return {
            self.require_type(expected, result_type, call.span, "integer method result");
        }
        Some(Expression {
            kind: ExpressionKind::IntegerAddition {
                mode,
                left: Box::new(left),
                right: Box::new(right),
            },
            ty: result_type,
            span: call.span,
        })
    }

    fn analyze_string_iteration_method(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        let bytes = match method.name.as_str() {
            "bytes" => true,
            "chars" => false,
            _ => return None,
        };
        let receiver_is_string = matches!(&field.base, AstExpression::String(_))
            || self.place_expression_type(&field.base) == Some(Type::Str);
        if !receiver_is_string {
            return None;
        }
        self.validate_intrinsic_method_arguments(call, method, "string", 0);
        self.require_expression_root_available(&field.base, call.span);
        let source = self.analyze_expression_non_consuming(&field.base);
        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow = true;
        self.check_and_record_borrow(&field.base, false, call.span);
        self.persistent_borrow = previous_persistence;
        let result_type = if bytes {
            self.types
                .intern_slice(Type::U8, false, call.span, self.diagnostics)
        } else {
            self.types.intern_chars(call.span, self.diagnostics)
        }
        .unwrap_or(Type::Unit);
        if let Some(expected) = expected_return {
            self.require_type(expected, result_type, call.span, "string method result");
        }
        Some(Expression {
            kind: if bytes {
                ExpressionKind::StringBytes(Box::new(source))
            } else {
                ExpressionKind::StringChars(Box::new(source))
            },
            ty: result_type,
            span: call.span,
        })
    }

    fn analyze_chars_next_method(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        if method.name != "next" {
            return None;
        }
        let receiver_type = self.place_expression_type(&field.base)?;
        if !self.types.is_chars(receiver_type) {
            return None;
        }
        self.validate_intrinsic_method_arguments(call, method, "character iterator", 0);
        self.require_expression_root_available(&field.base, call.span);
        self.require_mutable_place(&field.base);
        let iterator = self.analyze_place(&field.base)?;
        let result_type = self
            .types
            .intern_option(Type::Char, call.span, self.diagnostics)
            .unwrap_or(Type::Unit);
        if let Some(expected) = expected_return {
            self.require_type(expected, result_type, call.span, "iterator method result");
        }
        Some(Expression {
            kind: ExpressionKind::CharsNext { iterator },
            ty: result_type,
            span: call.span,
        })
    }

    fn validate_intrinsic_method_arguments(
        &mut self,
        call: &ast::CallExpression,
        method: &ast::Identifier,
        receiver: &str,
        expected: usize,
    ) {
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!("{receiver} method `{}` is not generic", method.name),
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        if call.arguments.len() != expected {
            self.diagnostics.push(Diagnostic::error(
                "E3105",
                format!(
                    "{receiver} method `{}` expects {expected} argument(s), but {} were provided",
                    method.name,
                    call.arguments.len()
                ),
                call.span,
            ));
        }
        for argument in &call.arguments {
            self.analyze_expression(argument);
        }
    }

    fn analyze_slice_access_method(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        let mutable = match method.name.as_str() {
            "get" => false,
            "get_mut" => true,
            _ => return None,
        };
        let receiver_type = self.place_expression_type(&field.base)?;
        let (element, receiver_mutable) = self.types.slice_shape(receiver_type)?;
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!("slice method `{}` is not generic", method.name),
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        if call.arguments.len() != 1 {
            self.diagnostics.push(Diagnostic::error(
                "E3105",
                format!(
                    "slice method `{}` expects 1 argument(s), but {} were provided",
                    method.name,
                    call.arguments.len()
                ),
                call.span,
            ));
        }
        if mutable && !receiver_mutable {
            self.diagnostics.push(
                Diagnostic::error("E3107", "`get_mut` requires a mutable slice", field.span)
                    .with_help("borrow the source as `&mut [T]` before calling `get_mut`"),
            );
        }

        self.require_expression_root_available(&field.base, call.span);
        let slice = self.analyze_expression_non_consuming(&field.base);
        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow = true;
        self.check_and_record_borrow(&field.base, mutable, call.span);
        self.persistent_borrow = previous_persistence;
        let index = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, index.ty, index.span, "slice index");

        let reference_type = self
            .types
            .intern_reference(element, mutable, call.span, self.diagnostics)
            .unwrap_or(Type::Unit);
        let result_type = self
            .types
            .intern_option(reference_type, call.span, self.diagnostics)
            .unwrap_or(Type::Unit);
        if let Some(expected) = expected_return {
            self.require_type(expected, result_type, call.span, "slice method result");
        }
        Some(Expression {
            kind: ExpressionKind::SliceGet {
                slice: Box::new(slice),
                index: Box::new(index),
                reference_type,
                mutable,
            },
            ty: result_type,
            span: call.span,
        })
    }

    fn analyze_resolved_method_call(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        receiver_type: Type,
        resolved_name: &str,
        signature: &Signature,
    ) -> Expression {
        let ast::FieldName::Named(method) = &field.field else {
            return invalid_composite_expression(call.span);
        };
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!("method `{}` is not generic", method.name),
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        if !signature.is_public && self.type_is_external(receiver_type) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E4005",
                    format!("method `{}` is private to its defining module", method.name),
                    method.span,
                )
                .with_help("mark the method `pub` or expose a public wrapper"),
            );
        }
        let Some(expected_receiver) = signature.parameter_types.first().copied() else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6002",
                    format!("associated function `{resolved_name}` has no `self` receiver"),
                    method.span,
                )
                .with_help("call it with `Type::function(...)`"),
            );
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            return invalid_composite_expression(call.span);
        };
        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow |= self.types.is_scoped(signature.return_type);
        let receiver = self.analyze_method_receiver(&field.base, receiver_type, expected_receiver);
        let mut arguments = Vec::with_capacity(call.arguments.len() + 1);
        arguments.push(receiver);
        for (index, argument) in call.arguments.iter().enumerate() {
            let expected = signature.parameter_types.get(index + 1).copied();
            arguments.push(self.analyze_expression_expected(argument, expected));
        }
        self.persistent_borrow = previous_persistence;
        self.validate_call_arguments(resolved_name, call.span, signature, &arguments);
        Expression {
            kind: ExpressionKind::Call {
                function: signature.id,
                arguments,
            },
            ty: signature.return_type,
            span: call.span,
        }
    }

    fn analyze_default_call(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Option<Expression> {
        let [owner, method] = path.segments.as_slice() else {
            return None;
        };
        if method.name != "default" {
            return None;
        }
        let ty = self.types.names.get(&owner.name).copied()?;
        if !self.types.satisfies_trait(ty, "Default") {
            return None;
        }
        if !call.arguments.is_empty() || !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E7007",
                    "derived `default` does not accept arguments",
                    call.span,
                )
                .with_help(format!("write `{}::default()`", owner.name)),
            );
        }
        for argument in &call.arguments {
            self.analyze_expression(argument);
        }
        self.types.default_expression(ty, call.span).or_else(|| {
            self.diagnostics.push(
                Diagnostic::error(
                    "E7007",
                    format!("cannot construct the derived default for `{}`", owner.name),
                    call.span,
                )
                .with_help("ensure every stored field has a deterministic default"),
            );
            Some(invalid_composite_expression(call.span))
        })
    }

    fn try_analyze_generic_method_call(
        &mut self,
        call: &ast::CallExpression,
        field: &ast::FieldExpression,
        method: &ast::Identifier,
        receiver_type: Type,
        resolved_name: &str,
        expected_return: Option<Type>,
    ) -> Option<Expression> {
        let nominal_receiver = self
            .types
            .pointer_shape(receiver_type)
            .filter(|(_, _, raw)| !raw)
            .map_or(receiver_type, |(target, _, _)| target);
        let template_name = self.types.generic_instance(nominal_receiver).map_or_else(
            || resolved_name.to_owned(),
            |instance| format!("{}::{}", instance.base_name, method.name),
        );
        let template = self
            .generic_functions
            .templates
            .get(&template_name)
            .cloned()?;
        if !template.is_public && self.type_is_external(receiver_type) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E4005",
                    format!("method `{}` is private to its defining module", method.name),
                    method.span,
                )
                .with_help("mark the method `pub` or expose a public wrapper"),
            );
        }
        let Some(receiver_parameter) = template.function.parameters.first() else {
            self.missing_receiver_diagnostic(&template_name, method.span, false);
            return Some(invalid_composite_expression(call.span));
        };
        if receiver_parameter.name.name != "self" {
            self.missing_receiver_diagnostic(&template_name, method.span, true);
            return Some(invalid_composite_expression(call.span));
        }
        let borrowed_receiver = self
            .types
            .pointer_shape(receiver_type)
            .filter(|(_, _, raw)| !raw);
        let receiver = match &receiver_parameter.ty.kind {
            TypeNameKind::Reference { mutable, .. }
                if borrowed_receiver
                    .is_some_and(|(_, actual_mutable, _)| actual_mutable == *mutable) =>
            {
                field.base.clone()
            }
            TypeNameKind::Reference { mutable: false, .. }
                if borrowed_receiver.is_some_and(|(_, actual_mutable, _)| actual_mutable) =>
            {
                AstExpression::Unary(Box::new(ast::UnaryExpression {
                    operator: AstUnaryOperator::Borrow,
                    operand: AstExpression::Unary(Box::new(ast::UnaryExpression {
                        operator: AstUnaryOperator::Dereference,
                        operand: field.base.clone(),
                        span: field.base.span(),
                    })),
                    span: field.base.span(),
                }))
            }
            TypeNameKind::Reference { .. } if borrowed_receiver.is_some() => field.base.clone(),
            TypeNameKind::Reference { mutable, .. } => {
                AstExpression::Unary(Box::new(ast::UnaryExpression {
                    operator: if *mutable {
                        AstUnaryOperator::BorrowMut
                    } else {
                        AstUnaryOperator::Borrow
                    },
                    operand: field.base.clone(),
                    span: field.base.span(),
                }))
            }
            _ => field.base.clone(),
        };
        let mut arguments = Vec::with_capacity(call.arguments.len() + 1);
        arguments.push(receiver);
        arguments.extend(call.arguments.iter().cloned());
        let generic_call = ast::CallExpression {
            callee: AstExpression::Path(ast::Path {
                segments: vec![method.clone()],
                span: method.span,
            }),
            generic_arguments: call.generic_arguments.clone(),
            arguments,
            span: call.span,
        };
        Some(self.analyze_generic_function_call(
            &generic_call,
            &template_name,
            &template,
            expected_return,
        ))
    }

    fn missing_receiver_diagnostic(&mut self, name: &str, span: Span, add_help: bool) {
        let diagnostic = Diagnostic::error(
            "E6002",
            format!("associated function `{name}` has no `self` receiver"),
            span,
        );
        self.diagnostics.push(if add_help {
            diagnostic.with_help("call it with `Type::function(...)`")
        } else {
            diagnostic
        });
    }

    fn analyze_method_receiver(
        &mut self,
        receiver: &AstExpression,
        receiver_type: Type,
        expected: Type,
    ) -> Expression {
        if let Some((expected_target, false, false)) = self.types.pointer_shape(expected)
            && let Some((actual_target, true, false)) = self.types.pointer_shape(receiver_type)
            && expected_target == actual_target
        {
            let dereference = AstExpression::Unary(Box::new(ast::UnaryExpression {
                operator: AstUnaryOperator::Dereference,
                operand: receiver.clone(),
                span: receiver.span(),
            }));
            let reborrow = ast::UnaryExpression {
                operator: AstUnaryOperator::Borrow,
                operand: dereference,
                span: receiver.span(),
            };
            return self.analyze_borrow(&reborrow, Some(expected));
        }
        if receiver_type == expected
            && let Some((_, true, false)) = self.types.pointer_shape(expected)
        {
            let dereference = AstExpression::Unary(Box::new(ast::UnaryExpression {
                operator: AstUnaryOperator::Dereference,
                operand: receiver.clone(),
                span: receiver.span(),
            }));
            let reborrow = ast::UnaryExpression {
                operator: AstUnaryOperator::BorrowMut,
                operand: dereference,
                span: receiver.span(),
            };
            return self.analyze_borrow(&reborrow, Some(expected));
        }
        if let Some((target, mutable, false)) = self.types.pointer_shape(expected)
            && target == receiver_type
        {
            let unary = ast::UnaryExpression {
                operator: if mutable {
                    AstUnaryOperator::BorrowMut
                } else {
                    AstUnaryOperator::Borrow
                },
                operand: receiver.clone(),
                span: receiver.span(),
            };
            return self.analyze_borrow(&unary, Some(expected));
        }
        self.analyze_expression_expected(receiver, Some(expected))
    }

    fn analyze_panic_call(&mut self, call: &ast::CallExpression, path: &ast::Path) -> Expression {
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let message = call.arguments.first().map_or_else(
            || Expression {
                kind: ExpressionKind::Unit,
                ty: Type::Unit,
                span: call.span,
            },
            |argument| self.analyze_expression_expected(argument, Some(Type::Str)),
        );
        self.require_type(Type::Str, message.ty, message.span, "panic message");
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        Expression {
            kind: ExpressionKind::Panic {
                message: Box::new(message),
            },
            ty: Type::Never,
            span: call.span,
        }
    }

    fn analyze_assert_call(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        mode: AssertionMode,
    ) -> Expression {
        if !(1..=2).contains(&call.arguments.len()) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!(
                        "intrinsic `{}` expects one condition and an optional message, but {} argument(s) were provided",
                        path.display(),
                        call.arguments.len()
                    ),
                    call.span,
                )
                .with_help(format!(
                    "write `{}(condition)` or `{}(condition, message)`",
                    path.display(),
                    path.display()
                )),
            );
        }
        let condition = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Bool)),
        );
        self.require_type(
            Type::Bool,
            condition.ty,
            condition.span,
            "assertion condition",
        );
        let message = call.arguments.get(1).map_or_else(
            || Expression {
                kind: ExpressionKind::String("assertion failed".to_owned()),
                ty: Type::Str,
                span: call.span,
            },
            |argument| self.analyze_expression_expected(argument, Some(Type::Str)),
        );
        self.require_type(Type::Str, message.ty, message.span, "assertion message");
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        Expression {
            kind: ExpressionKind::Assert {
                mode,
                condition: Box::new(condition),
                message: Box::new(message),
            },
            ty: Type::Unit,
            span: call.span,
        }
    }

    fn analyze_runtime_intrinsic_call(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Option<Expression> {
        match single_path_name(path)? {
            "__string_data" => {
                Some(self.analyze_string_view_part(call, path, StringViewIntrinsic::Data))
            }
            "__string_length" => {
                Some(self.analyze_string_view_part(call, path, StringViewIntrinsic::Length))
            }
            "__slice_length" => Some(self.analyze_slice_length(call, path)),
            "__str_from_parts" => Some(self.analyze_string_from_parts(call, path)),
            "__slice_from_parts" => Some(self.analyze_slice_from_parts(call, path, expected)),
            "__pointee_stride" => Some(self.analyze_pointee_stride(call, path)),
            "__hash_value" => Some(self.analyze_hash_value(call, path)),
            "__allocate_bytes" => Some(self.analyze_allocate_bytes(call, path, expected)),
            "__deallocate_bytes" => Some(self.analyze_deallocate_bytes(call, path)),
            "__thread_spawn" => Some(self.analyze_thread_spawn(call, path, expected, false)),
            "__thread_join" => Some(self.analyze_thread_join(call, path, expected)),
            "__thread_scope" => Some(self.analyze_thread_spawn(call, path, expected, true)),
            "__mutex_create" => Some(self.analyze_synchronization_create(
                call,
                path,
                hir::SynchronizationKind::Mutex,
            )),
            "__mutex_load" => Some(self.analyze_synchronization_load(
                call,
                path,
                expected,
                hir::SynchronizationKind::Mutex,
            )),
            "__mutex_replace" => Some(self.analyze_synchronization_replace(
                call,
                path,
                expected,
                hir::SynchronizationKind::Mutex,
            )),
            "__rwlock_create" => Some(self.analyze_synchronization_create(
                call,
                path,
                hir::SynchronizationKind::RwLock,
            )),
            "__rwlock_load" => Some(self.analyze_synchronization_load(
                call,
                path,
                expected,
                hir::SynchronizationKind::RwLock,
            )),
            "__rwlock_replace" => Some(self.analyze_synchronization_replace(
                call,
                path,
                expected,
                hir::SynchronizationKind::RwLock,
            )),
            "__thread_local_create" => Some(self.analyze_synchronization_create(
                call,
                path,
                hir::SynchronizationKind::ThreadLocal,
            )),
            "__thread_local_get" => Some(self.analyze_synchronization_load(
                call,
                path,
                expected,
                hir::SynchronizationKind::ThreadLocal,
            )),
            "__thread_local_set" => Some(self.analyze_thread_local_store(call, path)),
            "__channel_create" => Some(self.analyze_channel_create(call, path)),
            "__channel_send" => Some(self.analyze_channel_send(call, path, expected)),
            "__channel_receive" => Some(self.analyze_channel_receive(call, path, expected)),
            "__job_submit" => Some(self.analyze_job_submit(call, path, expected)),
            "__job_wait" => Some(self.analyze_job_wait(call, path, expected)),
            "__parallel_for_mut" => {
                Some(self.analyze_parallel_for_mut(call, path, expected, ParallelInputKind::Slice))
            }
            "__parallel_for_array_mut" => {
                Some(self.analyze_parallel_for_mut(call, path, expected, ParallelInputKind::Array))
            }
            _ => None,
        }
    }

    fn analyze_string_view_part(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        part: StringViewIntrinsic,
    ) -> Expression {
        self.require_standard_io_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let value = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Str)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Str, value.ty, value.span, "string view");
        let (kind, ty) = match part {
            StringViewIntrinsic::Data => {
                let ty = self
                    .types
                    .intern_raw_pointer(Type::U8, false, call.span, self.diagnostics)
                    .unwrap_or(Type::Unit);
                (ExpressionKind::StringData(Box::new(value)), ty)
            }
            StringViewIntrinsic::Length => {
                (ExpressionKind::StringLength(Box::new(value)), Type::Usize)
            }
        };
        Expression {
            kind,
            ty,
            span: call.span,
        }
    }

    fn analyze_slice_length(&mut self, call: &ast::CallExpression, path: &ast::Path) -> Expression {
        self.require_standard_slice_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let slice = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        if self.types.slice_shape(slice.ty).is_none() {
            self.diagnostics.push(Diagnostic::error(
                "E3154",
                format!("slice length requires a slice view, found `{}`", slice.ty),
                slice.span,
            ));
        }
        Expression {
            kind: ExpressionKind::SliceLength(Box::new(slice)),
            ty: Type::Usize,
            span: call.span,
        }
    }

    fn analyze_string_from_parts(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Expression {
        self.require_standard_string_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let data = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let length = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        let valid_data = self
            .types
            .pointer_shape(data.ty)
            .is_some_and(|(target, _, raw)| target == Type::U8 && raw);
        if !valid_data {
            self.diagnostics.push(Diagnostic::error(
                "E3149",
                format!(
                    "string construction requires a raw byte pointer, found `{}`",
                    data.ty
                ),
                data.span,
            ));
        }
        self.require_type(Type::Usize, length.ty, length.span, "string byte length");
        Expression {
            kind: ExpressionKind::StringFromParts {
                data: Box::new(data),
                length: Box::new(length),
            },
            ty: Type::Str,
            span: call.span,
        }
    }

    fn analyze_slice_from_parts(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_collections_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let data = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let length = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        let Some(slice_type) = expected.filter(|ty| self.types.slice_shape(*ty).is_some()) else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3151",
                    "`__slice_from_parts` requires an expected borrowed slice type",
                    call.span,
                )
                .with_help("call the intrinsic only from a typed standard collection wrapper"),
            );
            return invalid_composite_expression(call.span);
        };
        let (element, mutable) = self
            .types
            .slice_shape(slice_type)
            .expect("the expected type was checked as a slice");
        let valid_data =
            self.types
                .pointer_shape(data.ty)
                .is_some_and(|(target, pointer_mutable, raw)| {
                    target == element && raw && (!mutable || pointer_mutable)
                });
        if !valid_data {
            self.diagnostics.push(Diagnostic::error(
                "E3151",
                format!(
                    "slice construction requires a compatible raw element pointer, found `{}`",
                    data.ty
                ),
                data.span,
            ));
        }
        self.require_type(Type::Usize, length.ty, length.span, "slice element count");
        Expression {
            kind: ExpressionKind::SliceFromParts {
                data: Box::new(data),
                length: Box::new(length),
            },
            ty: slice_type,
            span: call.span,
        }
    }

    fn analyze_pointee_stride(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Expression {
        self.require_standard_collections_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let pointer = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        let Some((target, _, raw)) = self.types.pointer_shape(pointer.ty) else {
            self.diagnostics.push(Diagnostic::error(
                "E3150",
                format!(
                    "pointee stride requires a raw pointer, found `{}`",
                    pointer.ty
                ),
                pointer.span,
            ));
            return invalid_composite_expression(call.span);
        };
        if !raw {
            self.diagnostics.push(Diagnostic::error(
                "E3150",
                "pointee stride requires a raw pointer",
                pointer.span,
            ));
        }
        Expression {
            kind: ExpressionKind::TypeStride { target },
            ty: Type::Usize,
            span: call.span,
        }
    }

    fn analyze_hash_value(&mut self, call: &ast::CallExpression, path: &ast::Path) -> Expression {
        self.require_standard_collections_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let value = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let seed = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::U64)),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        if !self.types.is_hash_capable(value.ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3150",
                    format!("hashing requires `Hash`, found `{}`", value.ty),
                    value.span,
                )
                .with_help("derive or implement `Hash` for the key type"),
            );
        }
        self.require_type(Type::U64, seed.ty, seed.span, "hash seed");
        Expression {
            kind: ExpressionKind::HashValue {
                value: Box::new(value),
                seed: Box::new(seed),
            },
            ty: Type::U64,
            span: call.span,
        }
    }

    fn analyze_allocate_bytes(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_allocator_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let allocator = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let length = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        self.require_type(
            Type::Usize,
            allocator.ty,
            allocator.span,
            "allocator handle",
        );
        self.require_type(Type::Usize, length.ty, length.span, "allocation length");

        let Some(result_type) = expected else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3145",
                    "`__allocate_bytes` requires an expected `Result<OwnedBytes, AllocError>` type",
                    call.span,
                )
                .with_help("call the intrinsic only from the typed standard allocator wrapper"),
            );
            return invalid_composite_expression(call.span);
        };
        let Some(IntrinsicType::Result {
            success: allocation_type,
            error: error_type,
        }) = self.types.intrinsic(result_type)
        else {
            self.diagnostics.push(Diagnostic::error(
                "E3145",
                "`__allocate_bytes` must return a Result",
                call.span,
            ));
            return invalid_composite_expression(call.span);
        };
        let Some(error_variant) =
            self.validate_allocation_result_types(allocation_type, error_type, call.span)
        else {
            return invalid_composite_expression(call.span);
        };

        Expression {
            kind: ExpressionKind::AllocateBytes {
                allocator: Box::new(allocator),
                length: Box::new(length),
                allocation_type,
                error_type,
                error_variant,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_deallocate_bytes(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Expression {
        self.require_standard_allocator_intrinsic(call.span);
        if self.unsafe_depth == 0 {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3146",
                    "the raw deallocation intrinsic requires `unsafe`",
                    call.span,
                )
                .with_help("keep the intrinsic inside the owned standard-library wrapper"),
            );
        }
        if call.arguments.len() != 3 {
            self.intrinsic_arity_diagnostic(path, call, 3);
        }
        let allocator = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let data = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let length = call.arguments.get(2).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(3) {
            self.analyze_expression(extra);
        }
        self.require_type(
            Type::Usize,
            allocator.ty,
            allocator.span,
            "allocator handle",
        );
        self.require_type(Type::Usize, length.ty, length.span, "allocation length");
        let valid_data = self
            .types
            .pointer_shape(data.ty)
            .is_some_and(|(target, mutable, raw)| target == Type::U8 && mutable && raw);
        if !valid_data {
            self.diagnostics.push(Diagnostic::error(
                "E3146",
                format!("deallocation requires `*mut u8`, found `{}`", data.ty),
                data.span,
            ));
        }
        Expression {
            kind: ExpressionKind::DeallocateBytes {
                allocator: Box::new(allocator),
                data: Box::new(data),
                length: Box::new(length),
            },
            ty: Type::Unit,
            span: call.span,
        }
    }

    fn analyze_thread_spawn(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
        scoped: bool,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let callback = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let function_shape = self
            .types
            .function_shape(callback.ty)
            .map(|(parameters, output)| (parameters.to_vec(), output));
        let (parameter_type, output_type) = match function_shape {
            Some((parameters, output)) if parameters.len() == 1 => (parameters[0], output),
            Some((parameters, _)) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3151",
                        format!(
                            "thread callbacks require exactly one parameter, found {}",
                            parameters.len()
                        ),
                        callback.span,
                    )
                    .with_help("bundle multiple inputs in a struct or tuple"),
                );
                (Type::Unit, Type::Unit)
            }
            None => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3151",
                        format!(
                            "thread callback must be a function value, found `{}`",
                            callback.ty
                        ),
                        callback.span,
                    )
                    .with_help("pass a function with type `fn(Input) -> Output`"),
                );
                (Type::Unit, Type::Unit)
            }
        };
        let argument = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(parameter_type)),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        self.require_type(
            parameter_type,
            argument.ty,
            argument.span,
            "thread argument",
        );
        self.require_thread_transfer(argument.ty, argument.span, scoped, "argument");
        self.require_thread_transfer(output_type, callback.span, scoped, "result");

        let Some((result_type, success_type, error_type)) =
            self.expected_result_parts(expected, call.span, "thread operation")
        else {
            return invalid_composite_expression(call.span);
        };
        let expected_success = if scoped { output_type } else { Type::Usize };
        self.require_type(
            expected_success,
            success_type,
            call.span,
            "thread operation success",
        );
        let Some(variants) = self.thread_error_variants(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        let kind = if scoped {
            ExpressionKind::ThreadScope {
                callback: Box::new(callback),
                argument: Box::new(argument),
                output_type,
                error_type,
                spawn_failed_variant: variants.spawn_failed,
                invalid_handle_variant: variants.invalid_handle,
                worker_panicked_variant: variants.worker_panicked,
                result_mismatch_variant: variants.result_mismatch,
            }
        } else {
            ExpressionKind::ThreadSpawn {
                callback: Box::new(callback),
                argument: Box::new(argument),
                output_type,
                error_type,
                spawn_failed_variant: variants.spawn_failed,
            }
        };
        Expression {
            kind,
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_thread_join(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, handle.ty, handle.span, "thread handle");
        let Some((result_type, output_type, error_type)) =
            self.expected_result_parts(expected, call.span, "thread join")
        else {
            return invalid_composite_expression(call.span);
        };
        let Some(variants) = self.thread_error_variants(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::ThreadJoin {
                handle: Box::new(handle),
                output_type,
                error_type,
                invalid_handle_variant: variants.invalid_handle,
                worker_panicked_variant: variants.worker_panicked,
                result_mismatch_variant: variants.result_mismatch,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_synchronization_create(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        synchronization: hir::SynchronizationKind,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let value = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_synchronization_value(value.ty, value.span, synchronization);
        Expression {
            kind: ExpressionKind::SynchronizationCreate {
                value: Box::new(value),
                synchronization,
            },
            ty: Type::Usize,
            span: call.span,
        }
    }

    fn analyze_synchronization_load(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
        synchronization: hir::SynchronizationKind,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_type(
            Type::Usize,
            handle.ty,
            handle.span,
            "synchronization handle",
        );
        let Some(value_type) = expected else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3153",
                    "loading a synchronized value requires an expected result type",
                    call.span,
                )
                .with_help("call the intrinsic only from a typed standard-library method"),
            );
            return invalid_composite_expression(call.span);
        };
        if !self.types.satisfies_trait(value_type, "Copy") {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3153",
                    format!("synchronized load type `{value_type}` does not satisfy `Copy`"),
                    call.span,
                )
                .with_help("use `replace` to transfer ownership of a non-Copy value"),
            );
        }
        self.require_synchronization_value(value_type, call.span, synchronization);
        Expression {
            kind: ExpressionKind::SynchronizationLoad {
                handle: Box::new(handle),
                value_type,
                synchronization,
            },
            ty: value_type,
            span: call.span,
        }
    }

    fn analyze_synchronization_replace(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
        synchronization: hir::SynchronizationKind,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let value = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, expected),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        self.require_type(
            Type::Usize,
            handle.ty,
            handle.span,
            "synchronization handle",
        );
        if let Some(expected) = expected {
            self.require_type(expected, value.ty, value.span, "replacement value");
        }
        self.require_synchronization_value(value.ty, value.span, synchronization);
        let value_type = value.ty;
        Expression {
            kind: ExpressionKind::SynchronizationReplace {
                handle: Box::new(handle),
                value: Box::new(value),
                synchronization,
            },
            ty: value_type,
            span: call.span,
        }
    }

    fn analyze_thread_local_store(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let value = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, handle.ty, handle.span, "thread-local handle");
        self.require_synchronization_value(
            value.ty,
            value.span,
            hir::SynchronizationKind::ThreadLocal,
        );
        Expression {
            kind: ExpressionKind::ThreadLocalStore {
                handle: Box::new(handle),
                value: Box::new(value),
            },
            ty: Type::Unit,
            span: call.span,
        }
    }

    fn analyze_channel_create(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let probe = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let capacity = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, capacity.ty, capacity.span, "channel capacity");
        let Some((element_type, _, raw)) = self.types.pointer_shape(probe.ty) else {
            self.diagnostics.push(Diagnostic::error(
                "E3153",
                format!(
                    "channel type probe must be a raw pointer, found `{}`",
                    probe.ty
                ),
                probe.span,
            ));
            return invalid_composite_expression(call.span);
        };
        if !raw {
            self.diagnostics.push(Diagnostic::error(
                "E3153",
                "channel type probe must be a raw pointer",
                probe.span,
            ));
        }
        self.require_channel_value(element_type, probe.span);
        Expression {
            kind: ExpressionKind::ChannelCreate {
                probe: Box::new(probe),
                capacity: Box::new(capacity),
                element_type,
            },
            ty: Type::Usize,
            span: call.span,
        }
    }

    fn analyze_channel_send(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 2 {
            self.intrinsic_arity_diagnostic(path, call, 2);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let value = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        for extra in call.arguments.iter().skip(2) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, handle.ty, handle.span, "channel handle");
        self.require_channel_value(value.ty, value.span);
        let Some((result_type, success_type, error_type)) =
            self.expected_result_parts(expected, call.span, "channel send")
        else {
            return invalid_composite_expression(call.span);
        };
        self.require_type(Type::Unit, success_type, call.span, "channel send success");
        let Some(closed_variant) = self.channel_closed_variant(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::ChannelSend {
                handle: Box::new(handle),
                value: Box::new(value),
                error_type,
                closed_variant,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_channel_receive(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_thread_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, handle.ty, handle.span, "channel handle");
        let Some((result_type, value_type, error_type)) =
            self.expected_result_parts(expected, call.span, "channel receive")
        else {
            return invalid_composite_expression(call.span);
        };
        self.require_channel_value(value_type, call.span);
        let Some(closed_variant) = self.channel_closed_variant(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::ChannelReceive {
                handle: Box::new(handle),
                value_type,
                error_type,
                closed_variant,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_job_submit(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_job_intrinsic(call.span);
        if call.arguments.len() != 3 {
            self.intrinsic_arity_diagnostic(path, call, 3);
        }
        let pool = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let callback = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let function_shape = self
            .types
            .function_shape(callback.ty)
            .map(|(parameters, output)| (parameters.to_vec(), output));
        let (parameter_type, output_type) =
            self.require_single_parameter_callback(function_shape, &callback, "job");
        let argument = call.arguments.get(2).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(parameter_type)),
        );
        for extra in call.arguments.iter().skip(3) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, pool.ty, pool.span, "job pool handle");
        self.require_type(parameter_type, argument.ty, argument.span, "job argument");
        self.require_thread_transfer(argument.ty, argument.span, false, "job argument");
        self.require_thread_transfer(output_type, callback.span, false, "job result");
        let Some((result_type, success_type, error_type)) =
            self.expected_result_parts(expected, call.span, "job submission")
        else {
            return invalid_composite_expression(call.span);
        };
        self.require_type(
            Type::Usize,
            success_type,
            call.span,
            "job submission success",
        );
        let Some(variants) = self.job_error_variants(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::JobSubmit {
                pool: Box::new(pool),
                callback: Box::new(callback),
                argument: Box::new(argument),
                output_type,
                error_type,
                submit_failed_variant: variants.submit_failed,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_job_wait(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Expression {
        self.require_standard_job_intrinsic(call.span);
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let handle = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, handle.ty, handle.span, "job handle");
        let Some((result_type, output_type, error_type)) =
            self.expected_result_parts(expected, call.span, "job wait")
        else {
            return invalid_composite_expression(call.span);
        };
        let Some(variants) = self.job_error_variants(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::JobWait {
                handle: Box::new(handle),
                output_type,
                error_type,
                invalid_handle_variant: variants.invalid_handle,
                worker_panicked_variant: variants.worker_panicked,
                result_mismatch_variant: variants.result_mismatch,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn analyze_parallel_for_mut(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
        input_kind: ParallelInputKind,
    ) -> Expression {
        self.require_standard_job_intrinsic(call.span);
        if call.arguments.len() != 4 {
            self.intrinsic_arity_diagnostic(path, call, 4);
        }
        let pool = call.arguments.first().map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        let slice = call.arguments.get(1).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let callback = call.arguments.get(2).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression(argument),
        );
        let minimum_chunk = call.arguments.get(3).map_or_else(
            || invalid_composite_expression(call.span),
            |argument| self.analyze_expression_expected(argument, Some(Type::Usize)),
        );
        for extra in call.arguments.iter().skip(4) {
            self.analyze_expression(extra);
        }
        self.require_type(Type::Usize, pool.ty, pool.span, "job pool handle");
        self.require_type(
            Type::Usize,
            minimum_chunk.ty,
            minimum_chunk.span,
            "minimum parallel chunk",
        );
        let Some((element_type, array_length)) = self.parallel_input_shape(&slice, input_kind)
        else {
            return invalid_composite_expression(call.span);
        };
        self.require_channel_value(element_type, slice.span);
        let shape = self
            .types
            .function_shape(callback.ty)
            .map(|(parameters, output)| (parameters.to_vec(), output));
        let (parameter_type, output_type) =
            self.require_single_parameter_callback(shape, &callback, "parallel");
        let valid_callback = self
            .types
            .slice_shape(parameter_type)
            .is_some_and(|(element, mutable)| element == element_type && mutable);
        if !valid_callback {
            self.diagnostics.push(Diagnostic::error(
                "E3154",
                format!("parallel callback must accept `&mut [T]`, found `{parameter_type}`"),
                callback.span,
            ));
        }
        self.require_type(
            Type::Unit,
            output_type,
            callback.span,
            "parallel callback result",
        );
        let Some((result_type, success_type, error_type)) =
            self.expected_result_parts(expected, call.span, "parallel iteration")
        else {
            return invalid_composite_expression(call.span);
        };
        self.require_type(
            Type::Unit,
            success_type,
            call.span,
            "parallel iteration success",
        );
        let Some(variants) = self.job_error_variants(error_type, call.span) else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::ParallelFor {
                pool: Box::new(pool),
                slice: Box::new(slice),
                chunk_type: parameter_type,
                array_length,
                callback: Box::new(callback),
                minimum_chunk: Box::new(minimum_chunk),
                error_type,
                submit_failed_variant: variants.submit_failed,
                worker_panicked_variant: variants.worker_panicked,
                result_mismatch_variant: variants.result_mismatch,
            },
            ty: result_type,
            span: call.span,
        }
    }

    fn parallel_input_shape(
        &mut self,
        input: &Expression,
        kind: ParallelInputKind,
    ) -> Option<(Type, Option<u64>)> {
        let shape = match kind {
            ParallelInputKind::Slice => self
                .types
                .slice_shape(input.ty)
                .filter(|(_, mutable)| *mutable)
                .map(|(element, _)| (element, None)),
            ParallelInputKind::Array => self
                .types
                .pointer_shape(input.ty)
                .filter(|(_, mutable, raw)| *mutable && !raw)
                .and_then(|(target, _, _)| {
                    self.array_shape(target)
                        .map(|(element, length)| (element, Some(length)))
                }),
        };
        if shape.is_none() {
            let required = match kind {
                ParallelInputKind::Slice => "`&mut [T]`",
                ParallelInputKind::Array => "`&mut [T; N]`",
            };
            self.diagnostics.push(
                Diagnostic::error(
                    "E3154",
                    format!(
                        "parallel mutable iteration requires {required}, found `{}`",
                        input.ty
                    ),
                    input.span,
                )
                .with_help("borrow an array, vector slice, or tensor storage mutably"),
            );
        }
        shape
    }

    fn require_single_parameter_callback(
        &mut self,
        shape: Option<(Vec<Type>, Type)>,
        callback: &Expression,
        role: &str,
    ) -> (Type, Type) {
        match shape {
            Some((parameters, output)) if parameters.len() == 1 => (parameters[0], output),
            Some((parameters, _)) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3154",
                        format!(
                            "{role} callbacks require exactly one parameter, found {}",
                            parameters.len()
                        ),
                        callback.span,
                    )
                    .with_help("bundle multiple inputs in a struct or tuple"),
                );
                (Type::Unit, Type::Unit)
            }
            None => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3154",
                        format!(
                            "{role} callback must be a function value, found `{}`",
                            callback.ty
                        ),
                        callback.span,
                    )
                    .with_help("pass a function with type `fn(Input) -> Output`"),
                );
                (Type::Unit, Type::Unit)
            }
        }
    }

    fn expected_result_parts(
        &mut self,
        expected: Option<Type>,
        span: Span,
        role: &str,
    ) -> Option<(Type, Type, Type)> {
        let Some(result_type) = expected else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3151",
                    format!("{role} requires an expected `Result<T, E>` type"),
                    span,
                )
                .with_help("add a result type annotation or return the intrinsic directly"),
            );
            return None;
        };
        let Some(IntrinsicType::Result { success, error }) = self.types.intrinsic(result_type)
        else {
            self.diagnostics.push(Diagnostic::error(
                "E3151",
                format!("{role} must produce `Result<T, E>`"),
                span,
            ));
            return None;
        };
        Some((result_type, success, error))
    }

    fn thread_error_variants(
        &mut self,
        error_type: Type,
        span: Span,
    ) -> Option<ThreadErrorVariants> {
        let variant = |name: &str| {
            let (index, fields) = self.enum_variant(error_type, name)?;
            matches!(fields, hir::EnumVariantFields::Unit).then_some(index)
        };
        let (
            Some(spawn_failed),
            Some(invalid_handle),
            Some(worker_panicked),
            Some(result_mismatch),
        ) = (
            variant("SpawnFailed"),
            variant("InvalidHandle"),
            variant("WorkerPanicked"),
            variant("ResultMismatch"),
        )
        else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3151",
                    "thread errors require unit variants `SpawnFailed`, `InvalidHandle`, `WorkerPanicked`, and `ResultMismatch`",
                    span,
                )
                .with_help("use `std::thread::ThreadError`"),
            );
            return None;
        };
        Some(ThreadErrorVariants {
            spawn_failed,
            invalid_handle,
            worker_panicked,
            result_mismatch,
        })
    }

    fn job_error_variants(&mut self, error_type: Type, span: Span) -> Option<JobErrorVariants> {
        let variant = |name: &str| {
            let (index, fields) = self.enum_variant(error_type, name)?;
            matches!(fields, hir::EnumVariantFields::Unit).then_some(index)
        };
        let (
            Some(_pool_create_failed),
            Some(submit_failed),
            Some(invalid_handle),
            Some(worker_panicked),
            Some(result_mismatch),
        ) = (
            variant("PoolCreateFailed"),
            variant("SubmitFailed"),
            variant("InvalidHandle"),
            variant("WorkerPanicked"),
            variant("ResultMismatch"),
        )
        else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3154",
                    "job errors require unit variants `PoolCreateFailed`, `SubmitFailed`, `InvalidHandle`, `WorkerPanicked`, and `ResultMismatch`",
                    span,
                )
                .with_help("use `std::job::JobError`"),
            );
            return None;
        };
        Some(JobErrorVariants {
            submit_failed,
            invalid_handle,
            worker_panicked,
            result_mismatch,
        })
    }

    fn channel_closed_variant(&mut self, error_type: Type, span: Span) -> Option<u32> {
        let closed = self
            .enum_variant(error_type, "Closed")
            .and_then(|(index, fields)| {
                matches!(fields, hir::EnumVariantFields::Unit).then_some(index)
            });
        if closed.is_none() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3153",
                    "channel errors require a unit variant named `Closed`",
                    span,
                )
                .with_help("use `std::thread::ChannelError`"),
            );
        }
        closed
    }

    fn require_synchronization_value(
        &mut self,
        ty: Type,
        span: Span,
        synchronization: hir::SynchronizationKind,
    ) {
        let requirements: &[&str] = match synchronization {
            hir::SynchronizationKind::Mutex => &["Send"],
            hir::SynchronizationKind::RwLock => &["Send", "Sync"],
            hir::SynchronizationKind::ThreadLocal => &["Copy", "Send"],
        };
        for requirement in requirements {
            if !self.types.satisfies_trait(ty, requirement) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3153",
                        format!(
                            "synchronization value type `{ty}` does not satisfy `{requirement}`"
                        ),
                        span,
                    )
                    .with_help("store an owned value whose fields satisfy the required traits"),
                );
            }
        }
        if self.types.is_scoped(ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3153",
                    format!("synchronization value type `{ty}` cannot contain a scoped borrow"),
                    span,
                )
                .with_help("store owned data in synchronization resources"),
            );
        }
    }

    fn require_channel_value(&mut self, ty: Type, span: Span) {
        if !self.types.satisfies_trait(ty, "Send") {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3153",
                    format!("channel element type `{ty}` does not satisfy `Send`"),
                    span,
                )
                .with_help("send owned thread-safe values"),
            );
        }
        if self.types.is_scoped(ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3153",
                    format!("channel element type `{ty}` cannot contain a scoped borrow"),
                    span,
                )
                .with_help("send owned data or use `scope` with direct arguments"),
            );
        }
    }

    fn require_thread_transfer(&mut self, ty: Type, span: Span, scoped: bool, role: &str) {
        if !self.types.satisfies_trait(ty, "Send") {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3152",
                    format!("thread {role} type `{ty}` does not satisfy `Send`"),
                    span,
                )
                .with_help("move owned thread-safe data or use synchronization"),
            );
        }
        if !scoped && self.types.is_scoped(ty) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3152",
                    format!("native thread {role} cannot contain a scoped borrow"),
                    span,
                )
                .with_help("use `scope` so the worker is joined before the borrow ends"),
            );
        }
    }

    fn require_standard_allocator_intrinsic(&mut self, span: Span) {
        if self.module_identity.as_deref() == Some("3_std_5_alloc") {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3145",
                "runtime allocator intrinsics are private to `std::alloc`",
                span,
            )
            .with_help("import and call the public allocator wrapper instead"),
        );
    }

    fn require_standard_io_intrinsic(&mut self, span: Span) {
        if matches!(
            self.module_identity.as_deref(),
            Some(
                "3_std_2_fs" | "3_std_2_io" | "3_std_3_env" | "3_std_6_string" | "3_std_7_process"
            )
        ) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3148",
                "string-view runtime intrinsics are private to standard library wrappers",
                span,
            )
            .with_help("import and call the public string, file-system, or I/O wrapper instead"),
        );
    }

    fn require_standard_string_intrinsic(&mut self, span: Span) {
        if self.module_identity.as_deref() == Some("3_std_6_string") {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3149",
                "raw string construction is private to `std::string`",
                span,
            )
            .with_help("construct an owned `String` or use a string literal instead"),
        );
    }

    fn require_standard_collections_intrinsic(&mut self, span: Span) {
        if matches!(
            self.module_identity.as_deref(),
            Some("3_std_5_alloc" | "3_std_5_slice" | "3_std_11_collections" | "3_std_2_fs")
        ) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3150",
                "native slice intrinsics are private to standard slice wrappers",
                span,
            )
            .with_help("use `std::slice` or a standard owned collection instead"),
        );
    }

    fn require_standard_thread_intrinsic(&mut self, span: Span) {
        if self.module_identity.as_deref() == Some("3_std_6_thread") {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3151",
                "native thread intrinsics are private to `std::thread`",
                span,
            )
            .with_help("import and call the safe standard thread wrappers"),
        );
    }

    fn require_standard_job_intrinsic(&mut self, span: Span) {
        if self.module_identity.as_deref() == Some("3_std_3_job") {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error("E3154", "job intrinsics are private to `std::job`", span)
                .with_help("import and call the safe standard job wrappers"),
        );
    }

    fn require_standard_slice_intrinsic(&mut self, span: Span) {
        if matches!(
            self.module_identity.as_deref(),
            Some(
                "3_std_3_job" | "3_std_5_alloc" | "3_std_5_slice" | "3_std_6_string" | "3_std_2_fs"
            )
        ) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3154",
                "raw slice inspection is private to the standard library",
                span,
            )
            .with_help("use safe slice and string APIs instead"),
        );
    }

    fn validate_allocation_result_types(
        &mut self,
        allocation_type: Type,
        error_type: Type,
        span: Span,
    ) -> Option<u32> {
        let allocation_is_valid =
            self.types
                .definition(allocation_type)
                .is_some_and(|definition| {
                    let hir::TypeDefinitionKind::Struct { fields } = &definition.kind else {
                        return false;
                    };
                    let [data, length, allocator] = fields.as_slice() else {
                        return false;
                    };
                    data.name == "data"
                        && !data.is_public
                        && self.types.pointer_shape(data.ty).is_some_and(
                            |(target, mutable, raw)| target == Type::U8 && mutable && raw,
                        )
                        && length.name == "len"
                        && !length.is_public
                        && length.ty == Type::Usize
                        && allocator.name == "allocator"
                        && !allocator.is_public
                        && allocator.ty == Type::Usize
                });
        let error_variant = self.types.definition(error_type).and_then(|definition| {
            let hir::TypeDefinitionKind::Enum { variants } = &definition.kind else {
                return None;
            };
            variants
                .iter()
                .position(|variant| {
                    variant.name == "OutOfMemory" && variant.fields == hir::EnumVariantFields::Unit
                })
                .and_then(|index| u32::try_from(index).ok())
        });
        if allocation_is_valid && error_variant.is_some() {
            return error_variant;
        }
        self.diagnostics.push(
            Diagnostic::error(
                "E3145",
                "invalid standard allocator result representation",
                span,
            )
            .with_help(
                "`OwnedBytes` must contain private `data`, `len`, and `allocator` fields and `AllocError` must define `OutOfMemory`",
            ),
        );
        None
    }

    fn analyze_intrinsic_call(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected: Option<Type>,
    ) -> Option<Expression> {
        let name = single_path_name(path)?;
        if !matches!(name, "Some" | "Ok" | "Err") {
            return None;
        }

        let expected_intrinsic =
            expected.and_then(|ty| self.types.intrinsic(ty).map(|intrinsic| (ty, intrinsic)));
        match (name, expected_intrinsic) {
            ("Some", Some((ty, IntrinsicType::Option { value }))) => {
                Some(self.analyze_intrinsic_constructor(call, path, ty, 0, value))
            }
            ("Ok", Some((ty, IntrinsicType::Result { success, .. }))) => {
                Some(self.analyze_intrinsic_constructor(call, path, ty, 0, success))
            }
            ("Err", Some((ty, IntrinsicType::Result { error, .. }))) => {
                Some(self.analyze_intrinsic_constructor(call, path, ty, 1, error))
            }
            ("Some", None) => Some(self.infer_option_constructor(call, path)),
            _ => {
                for argument in &call.arguments {
                    self.analyze_expression(argument);
                }
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3139",
                        format!(
                            "`{name}` requires an expected {} type",
                            if name == "Some" {
                                "`Option<T>`"
                            } else {
                                "`Result<T, E>`"
                            }
                        ),
                        call.span,
                    )
                    .with_help(
                        "add a result type annotation or use the constructor in a typed context",
                    ),
                );
                Some(invalid_composite_expression(call.span))
            }
        }
    }

    fn infer_option_constructor(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
    ) -> Expression {
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let Some(argument) = call.arguments.first() else {
            return invalid_composite_expression(call.span);
        };
        let field = self.analyze_expression(argument);
        for extra in call.arguments.iter().skip(1) {
            self.analyze_expression(extra);
        }
        let Some(ty) = self
            .types
            .intern_option(field.ty, call.span, self.diagnostics)
        else {
            return invalid_composite_expression(call.span);
        };
        Expression {
            kind: ExpressionKind::Enum {
                variant: 0,
                fields: vec![field],
            },
            ty,
            span: call.span,
        }
    }

    fn analyze_intrinsic_constructor(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        ty: Type,
        variant: u32,
        field_type: Type,
    ) -> Expression {
        if call.arguments.len() != 1 {
            self.intrinsic_arity_diagnostic(path, call, 1);
        }
        let fields = call
            .arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                self.analyze_expression_expected(argument, (index == 0).then_some(field_type))
            })
            .collect::<Vec<_>>();
        if let Some(field) = fields.first() {
            self.require_type(field_type, field.ty, field.span, "constructor argument");
        }
        Expression {
            kind: ExpressionKind::Enum { variant, fields },
            ty,
            span: call.span,
        }
    }

    fn intrinsic_arity_diagnostic(
        &mut self,
        path: &ast::Path,
        call: &ast::CallExpression,
        expected: usize,
    ) {
        self.diagnostics.push(Diagnostic::error(
            "E3105",
            format!(
                "constructor `{}` expects {expected} argument(s), but {} were provided",
                path.display(),
                call.arguments.len()
            ),
            call.span,
        ));
    }

    fn analyze_try(&mut self, value: &AstExpression, span: Span) -> Expression {
        self.reject_deferred_control_flow("`?`", span);
        let value = self.analyze_expression_expected(value, Some(self.signature.return_type));
        let Some(intrinsic) = self.types.intrinsic(value.ty) else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3140",
                    format!(
                        "`?` requires `Option<T>` or `Result<T, E>`, found `{}`",
                        value.ty
                    ),
                    span,
                )
                .with_help("apply `?` only to an optional or fallible expression"),
            );
            return invalid_composite_expression(span);
        };
        let return_intrinsic = self.types.intrinsic(self.signature.return_type);
        let compatible = match (intrinsic, return_intrinsic) {
            (IntrinsicType::Option { .. }, Some(IntrinsicType::Option { .. })) => true,
            (
                IntrinsicType::Result { error, .. },
                Some(IntrinsicType::Result {
                    error: return_error,
                    ..
                }),
            ) => error == return_error,
            _ => false,
        };
        if !compatible {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3140",
                    format!(
                        "`?` cannot propagate `{}` from a function returning `{}`",
                        value.ty, self.signature.return_type
                    ),
                    span,
                )
                .with_help("use the same Option or Result type as the function return type"),
            );
        }
        let (output_type, failure_type) = match intrinsic {
            IntrinsicType::Option { value } => (value, None),
            IntrinsicType::Result { success, error } => (success, Some(error)),
        };
        Expression {
            kind: ExpressionKind::Try {
                value: Box::new(value),
                success_variant: 0,
                output_type,
                failure_variant: 1,
                failure_type,
                return_type: self.signature.return_type,
            },
            ty: output_type,
            span,
        }
    }

    fn analyze_enum_tuple_call(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        ty: Type,
        variant_name: &str,
    ) -> Expression {
        let Some((variant, fields)) = self.enum_variant(ty, variant_name) else {
            self.diagnostics.push(Diagnostic::error(
                "E3119",
                format!("enum variant `{}` does not exist", path.display()),
                path.span,
            ));
            return invalid_composite_expression(call.span);
        };
        let hir::EnumVariantFields::Tuple(field_types) = fields else {
            self.diagnostics.push(Diagnostic::error(
                "E3119",
                format!("enum variant `{}` is not tuple-like", path.display()),
                path.span,
            ));
            return invalid_composite_expression(call.span);
        };
        if field_types.len() != call.arguments.len() {
            self.diagnostics.push(Diagnostic::error(
                "E3105",
                format!(
                    "variant `{}` expects {} argument(s), but {} were provided",
                    path.display(),
                    field_types.len(),
                    call.arguments.len()
                ),
                call.span,
            ));
        }
        let fields: Vec<_> = call
            .arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                self.analyze_expression_expected(argument, field_types.get(index).copied())
            })
            .collect();
        for (field, expected) in fields.iter().zip(field_types.iter().copied()) {
            self.require_type(expected, field.ty, field.span, "variant argument");
        }
        Expression {
            kind: ExpressionKind::Enum { variant, fields },
            ty,
            span: call.span,
        }
    }

    fn analyze_function_call(
        &mut self,
        call: &ast::CallExpression,
        path: &ast::Path,
        expected_return: Option<Type>,
    ) -> Expression {
        let Some(resolved_name) = self.resolve_function_path(path) else {
            return invalid_composite_expression(call.span);
        };
        if path.segments.len() == 1 && self.lookup(&resolved_name).is_some() {
            for argument in &call.arguments {
                self.analyze_expression(argument);
            }
            self.diagnostics.push(
                Diagnostic::error(
                    "E3106",
                    format!("local binding `{resolved_name}` is not callable"),
                    path.span,
                )
                .with_help("call a declared function or method"),
            );
            return Expression {
                kind: ExpressionKind::Unit,
                ty: Type::Unit,
                span: call.span,
            };
        }
        let signature = self.signatures.get(&resolved_name).cloned();
        if signature.is_none()
            && let Some(template) = self
                .generic_functions
                .templates
                .get(&resolved_name)
                .cloned()
        {
            return self.analyze_generic_function_call(
                call,
                &resolved_name,
                &template,
                expected_return,
            );
        }
        let Some(signature) = signature else {
            self.diagnostics.push(Diagnostic::error(
                "E3106",
                format!("cannot resolve function `{}`", path.display()),
                path.span,
            ));
            return Expression {
                kind: ExpressionKind::Unit,
                ty: Type::Unit,
                span: call.span,
            };
        };
        if !call.generic_arguments.is_empty() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!("function `{resolved_name}` is not generic"),
                    call.span,
                )
                .with_help("remove the arguments between `<` and `>`"),
            );
        }
        self.validate_function_access(path, call.span, &resolved_name, &signature);
        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow |= self.types.is_scoped(signature.return_type);
        let arguments = self.analyze_typed_call_arguments(&call.arguments, &signature);
        self.persistent_borrow = previous_persistence;
        self.validate_call_arguments(&resolved_name, call.span, &signature, &arguments);

        Expression {
            kind: ExpressionKind::Call {
                function: signature.id,
                arguments,
            },
            ty: signature.return_type,
            span: call.span,
        }
    }

    fn resolve_function_path(&mut self, path: &ast::Path) -> Option<String> {
        if let Some(name) = function_path_name(path) {
            Some(name)
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3106",
                    format!(
                        "qualified call `{}` requires module resolution",
                        path.display()
                    ),
                    path.span,
                )
                .with_help("module paths must resolve to one canonical symbol before typing"),
            );
            None
        }
    }

    fn validate_function_access(
        &mut self,
        path: &ast::Path,
        span: Span,
        resolved_name: &str,
        signature: &Signature,
    ) {
        if signature.requires_unsafe && self.unsafe_depth == 0 {
            self.diagnostics.push(
                Diagnostic::error(
                    "E5002",
                    format!("native function `{resolved_name}` can only be called inside `unsafe`"),
                    span,
                )
                .with_help("review the binding contract and wrap the call in `unsafe { ... }`"),
            );
        }
        if let [owner, _] = path.segments.as_slice()
            && !signature.is_public
            && self
                .types
                .names
                .get(&owner.name)
                .is_some_and(|ty| self.type_is_external(*ty))
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E4005",
                    format!("associated function `{resolved_name}` is private"),
                    path.span,
                )
                .with_help("mark the associated function `pub`"),
            );
        }
    }

    fn analyze_typed_call_arguments(
        &mut self,
        source_arguments: &[AstExpression],
        signature: &Signature,
    ) -> Vec<Expression> {
        let mut arguments = Vec::with_capacity(source_arguments.len());
        for (index, argument) in source_arguments.iter().enumerate() {
            let expected = signature.parameter_types.get(index).copied();
            let previous_reborrow = self.reborrow_argument;
            self.reborrow_argument = expected.is_some_and(|ty| self.types.is_mutable_view(ty));
            arguments.push(self.analyze_expression_expected(argument, expected));
            self.reborrow_argument = previous_reborrow;
        }
        arguments
    }

    fn analyze_generic_function_call(
        &mut self,
        call: &ast::CallExpression,
        source_name: &str,
        template: &GenericFunctionTemplate,
        expected_return: Option<Type>,
    ) -> Expression {
        let parameters = &template.parameters;
        let mut environment = self.types.base_environment();
        let explicit_parameters = parameters
            .get(template.explicit_parameter_start..)
            .unwrap_or_default();
        self.bind_explicit_generic_arguments(call, explicit_parameters, &mut environment);
        if let (Some(return_type), Some(expected)) =
            (&template.function.return_type, expected_return)
        {
            self.types
                .infer_type_pattern(return_type, expected, parameters, &mut environment);
        }

        if call.arguments.len() != template.function.parameters.len() {
            self.diagnostics.push(Diagnostic::error(
                "E3105",
                format!(
                    "function `{source_name}` expects {} argument(s), but {} were provided",
                    template.function.parameters.len(),
                    call.arguments.len()
                ),
                call.span,
            ));
        }

        let previous_persistence = self.persistent_borrow;
        self.persistent_borrow |= template
            .function
            .return_type
            .as_ref()
            .is_some_and(|ty| self.types.type_name_may_be_scoped(ty));
        let arguments =
            self.infer_generic_call_arguments(call, source_name, template, &mut environment);
        self.persistent_borrow = previous_persistence;
        let Some(values) = self.complete_generic_environment(
            parameters,
            &template.where_predicates,
            &mut environment,
            template.function.span,
        ) else {
            return invalid_composite_expression(call.span);
        };
        let Some(signature) =
            self.instantiate_generic_function(template, values, &environment, call.span)
        else {
            return invalid_composite_expression(call.span);
        };
        self.validate_call_arguments(source_name, call.span, &signature, &arguments);
        Expression {
            kind: ExpressionKind::Call {
                function: signature.id,
                arguments,
            },
            ty: signature.return_type,
            span: call.span,
        }
    }

    fn bind_explicit_generic_arguments(
        &mut self,
        call: &ast::CallExpression,
        parameters: &[ast::GenericParameter],
        environment: &mut GenericEnvironment,
    ) {
        let has_pack = parameters
            .iter()
            .any(|parameter| matches!(parameter, ast::GenericParameter::TypePack { .. }));
        if !has_pack && call.generic_arguments.len() > parameters.len() {
            self.diagnostics.push(
                Diagnostic::error(
                    "E6004",
                    format!(
                        "generic call provides {} argument(s), but at most {} are accepted",
                        call.generic_arguments.len(),
                        parameters.len()
                    ),
                    call.span,
                )
                .with_help("remove the extra generic arguments"),
            );
        }
        let Some(values) = self.types.resolve_generic_argument_values(
            &call.generic_arguments,
            &self.generic_environment,
            self.diagnostics,
        ) else {
            return;
        };
        let mut value_index = 0;
        for parameter in parameters {
            match parameter {
                ast::GenericParameter::Type { name, .. } => match values.get(value_index) {
                    Some(GenericValue::Type(ty)) => {
                        environment.types.insert(name.name.clone(), *ty);
                        value_index += 1;
                    }
                    Some(GenericValue::Const(_)) => self.diagnostics.push(
                        Diagnostic::error("E6004", "expected a type generic argument", call.span)
                            .with_help("provide a type name at this position"),
                    ),
                    None => break,
                },
                ast::GenericParameter::TypePack { name, .. } => {
                    if value_index == values.len() && call.generic_arguments.is_empty() {
                        continue;
                    }
                    let mut types = Vec::new();
                    for value in &values[value_index..] {
                        let GenericValue::Type(ty) = value else {
                            self.diagnostics.push(
                                Diagnostic::error(
                                    "E6020",
                                    format!("type pack `{}` received a const argument", name.name),
                                    call.span,
                                )
                                .with_help("type packs accept only type arguments"),
                            );
                            return;
                        };
                        types.push(*ty);
                    }
                    environment.type_packs.insert(name.name.clone(), types);
                    value_index = values.len();
                }
                ast::GenericParameter::Const { name, .. } => match values.get(value_index) {
                    Some(GenericValue::Const(value)) => {
                        environment.constants.insert(name.name.clone(), *value);
                        value_index += 1;
                    }
                    Some(GenericValue::Type(_)) => self.diagnostics.push(
                        Diagnostic::error("E6004", "expected a const generic argument", call.span)
                            .with_help("provide an integer expression at this position"),
                    ),
                    None => break,
                },
            }
        }
    }

    fn infer_generic_call_arguments(
        &mut self,
        call: &ast::CallExpression,
        source_name: &str,
        template: &GenericFunctionTemplate,
        environment: &mut GenericEnvironment,
    ) -> Vec<Expression> {
        let parameters = &template.parameters;
        let mut arguments = Vec::with_capacity(call.arguments.len());
        for (index, argument) in call.arguments.iter().enumerate() {
            let parameter = template.function.parameters.get(index);
            let expected = parameter.and_then(|parameter| {
                if type_pattern_is_bound(&parameter.ty, parameters, environment) {
                    self.types
                        .resolve_type_name_in(&parameter.ty, environment, self.diagnostics)
                } else {
                    None
                }
            });
            let previous_reborrow = self.reborrow_argument;
            self.reborrow_argument = expected.is_some_and(|ty| self.types.is_mutable_view(ty))
                || parameter.is_some_and(|parameter| {
                    matches!(
                        &parameter.ty.kind,
                        TypeNameKind::Reference { mutable: true, .. }
                    )
                });
            let analyzed = self.analyze_expression_expected(argument, expected);
            self.reborrow_argument = previous_reborrow;
            if let Some(parameter) = parameter
                && !self.types.infer_type_pattern(
                    &parameter.ty,
                    analyzed.ty,
                    parameters,
                    environment,
                )
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E6004",
                        format!(
                            "argument {} does not match the generic parameter pattern `{}`",
                            index + 1,
                            source_name
                        ),
                        argument.span(),
                    )
                    .with_help("use one consistent concrete type for each generic parameter"),
                );
            }
            arguments.push(analyzed);
        }
        arguments
    }

    fn instantiate_generic_function(
        &mut self,
        template: &GenericFunctionTemplate,
        values: Vec<GenericValue>,
        environment: &GenericEnvironment,
        span: Span,
    ) -> Option<Signature> {
        let key = GenericFunctionKey {
            resolved_name: template.resolved_name.clone(),
            arguments: values,
            pack_lengths: template
                .parameters
                .iter()
                .filter_map(|parameter| {
                    let ast::GenericParameter::TypePack { name, .. } = parameter else {
                        return None;
                    };
                    Some(environment.type_packs.get(&name.name).map_or(0, Vec::len))
                })
                .collect(),
        };
        if let Some(signature) = self.generic_functions.instances.get(&key) {
            return Some(signature.clone());
        }
        let parameter_types = template
            .function
            .parameters
            .iter()
            .map(|parameter| {
                self.types
                    .resolve_type_name_in(&parameter.ty, environment, self.diagnostics)
                    .unwrap_or(Type::Unit)
            })
            .collect::<Vec<_>>();
        let return_type = template
            .function
            .return_type
            .as_ref()
            .map_or(Type::Unit, |ty| {
                self.types
                    .resolve_type_name_in(ty, environment, self.diagnostics)
                    .unwrap_or(Type::Unit)
            });
        let id = FunctionId(self.generic_functions.next_id);
        self.generic_functions.next_id =
            self.generic_functions.next_id.checked_add(1).or_else(|| {
                self.diagnostics.push(Diagnostic::error(
                    "E3999",
                    "this compilation unit contains too many function instances",
                    span,
                ));
                None
            })?;
        let signature = Signature {
            id,
            parameter_types,
            return_type,
            requires_unsafe: false,
            is_public: template.is_public,
        };
        self.generic_functions
            .instances
            .insert(key, signature.clone());
        self.generic_functions.pending.push(PendingFunction {
            function: template.function.clone(),
            resolved_name: format!("{}$instance${}", template.resolved_name, id.0),
            signature: signature.clone(),
            environment: environment.clone(),
            module_identity: template.module_identity.clone(),
        });
        Some(signature)
    }

    fn complete_generic_environment(
        &mut self,
        parameters: &[ast::GenericParameter],
        where_predicates: &[ast::WherePredicate],
        environment: &mut GenericEnvironment,
        span: Span,
    ) -> Option<Vec<GenericValue>> {
        let mut values = Vec::with_capacity(parameters.len());
        let mut complete = true;
        for parameter in parameters {
            match parameter {
                ast::GenericParameter::Type { name, default, .. } => {
                    let ty = environment.types.get(&name.name).copied().or_else(|| {
                        default.as_ref().and_then(|default| {
                            self.types
                                .resolve_type_name_in(default, environment, self.diagnostics)
                        })
                    });
                    if let Some(ty) = ty {
                        environment.types.insert(name.name.clone(), ty);
                        values.push(GenericValue::Type(ty));
                    } else {
                        complete = false;
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E6004",
                                format!(
                                    "cannot infer generic type parameter `{}`",
                                    name.name
                                ),
                                span,
                            )
                            .with_help(
                                "use the parameter in an argument or provide an expected return type",
                            ),
                        );
                    }
                }
                ast::GenericParameter::TypePack { name, .. } => {
                    if let Some(types) = environment.type_packs.get(&name.name) {
                        values.extend(types.iter().copied().map(GenericValue::Type));
                    } else {
                        complete = false;
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E6020",
                                format!("cannot infer generic type pack `{}`", name.name),
                                span,
                            )
                            .with_help(
                                "use the pack in a tuple parameter or provide explicit type arguments",
                            ),
                        );
                    }
                }
                ast::GenericParameter::Const {
                    name, ty, default, ..
                } => {
                    let const_type =
                        self.types
                            .resolve_type_name_in(ty, environment, self.diagnostics);
                    if const_type.is_some_and(|ty| !ty.is_integer()) {
                        self.diagnostics.push(Diagnostic::error(
                            "E6005",
                            format!("const parameter `{}` must use an integer type", name.name),
                            ty.span,
                        ));
                        complete = false;
                    }
                    let value = environment.constants.get(&name.name).copied().or_else(|| {
                        default.as_ref().and_then(|default| {
                            evaluate_array_length_in(default, environment, self.diagnostics)
                        })
                    });
                    if let Some(value) = value {
                        environment.constants.insert(name.name.clone(), value);
                        values.push(GenericValue::Const(value));
                    } else {
                        complete = false;
                        self.diagnostics.push(
                            Diagnostic::error(
                                "E6004",
                                format!("cannot infer const generic parameter `{}`", name.name),
                                span,
                            )
                            .with_help("use the const parameter in an array or generic argument"),
                        );
                    }
                }
            }
        }
        if complete
            && !self.types.validate_bounds(
                parameters,
                where_predicates,
                environment,
                span,
                self.diagnostics,
            )
        {
            complete = false;
        }
        complete.then_some(values)
    }

    fn validate_call_arguments(
        &mut self,
        name: &str,
        span: Span,
        signature: &Signature,
        arguments: &[Expression],
    ) {
        if arguments.len() != signature.parameter_types.len() {
            self.diagnostics.push(Diagnostic::error(
                "E3105",
                format!(
                    "function `{name}` expects {} argument(s), but {} were provided",
                    signature.parameter_types.len(),
                    arguments.len()
                ),
                span,
            ));
        }
        for (argument, expected) in arguments
            .iter()
            .zip(signature.parameter_types.iter().copied())
        {
            self.require_type(expected, argument.ty, argument.span, "function argument");
        }
    }

    fn analyze_if(&mut self, expression: &ast::IfExpression, expected: Option<Type>) -> Expression {
        let condition = self.analyze_expression_expected(&expression.condition, Some(Type::Bool));
        self.require_type(
            Type::Bool,
            condition.ty,
            expression.condition.span(),
            "if condition",
        );
        let moves_before_branches = self.moved_locals.clone();
        let field_moves_before_branches = self.moved_fields.clone();
        let then_branch = self.analyze_block(&expression.then_branch, expected);
        let then_moves = self.moved_locals.clone();
        let then_field_moves = self.moved_fields.clone();
        self.moved_locals.clone_from(&moves_before_branches);
        self.moved_fields.clone_from(&field_moves_before_branches);
        let else_branch = expression
            .else_branch
            .as_ref()
            .map(|branch| Box::new(self.analyze_expression_expected(branch, expected)));
        let else_moves = self.moved_locals.clone();
        let else_field_moves = self.moved_fields.clone();
        self.moved_locals = then_moves;
        self.moved_locals.extend(else_moves);
        self.moved_fields = then_field_moves;
        self.moved_fields.extend(else_field_moves);
        let ty = if let Some(else_branch) = &else_branch {
            self.unify_branch_types(then_branch.ty, else_branch.ty, expression.span)
        } else {
            self.require_type(
                Type::Unit,
                then_branch.ty,
                expression.then_branch.span,
                "`if` branch without `else`",
            );
            Type::Unit
        };

        Expression {
            kind: ExpressionKind::If(Box::new(hir::IfExpression {
                condition,
                then_branch,
                else_branch,
            })),
            ty,
            span: expression.span,
        }
    }

    fn analyze_match(
        &mut self,
        expression: &ast::MatchExpression,
        expected: Option<Type>,
    ) -> Expression {
        let scrutinee_root = self.scoped_source_local(&expression.scrutinee);
        let scrutinee = self.analyze_expression(&expression.scrutinee);
        let moves_before_arms = self.moved_locals.clone();
        let field_moves_before_arms = self.moved_fields.clone();
        let mut moves_after_arms = moves_before_arms.clone();
        let mut field_moves_after_arms = field_moves_before_arms.clone();
        let mut arms = Vec::with_capacity(expression.arms.len());
        let mut result_type = None;
        for arm in &expression.arms {
            self.moved_locals.clone_from(&moves_before_arms);
            self.moved_fields.clone_from(&field_moves_before_arms);
            self.push_scope();
            let pattern = self.analyze_pattern(&arm.pattern, scrutinee.ty);
            if let Some(root) = scrutinee_root {
                self.record_pattern_scoped_roots(&pattern, root);
            }
            let guard = arm.guard.as_ref().map(|guard| {
                let guard = self.analyze_expression_expected(guard, Some(Type::Bool));
                self.require_type(Type::Bool, guard.ty, guard.span, "match guard");
                guard
            });
            let body = self.analyze_expression_expected(&arm.body, expected);
            result_type = Some(result_type.map_or(body.ty, |previous| {
                self.unify_branch_types(previous, body.ty, arm.span)
            }));
            self.pop_scope();
            moves_after_arms.extend(self.moved_locals.clone());
            field_moves_after_arms.extend(self.moved_fields.clone());
            arms.push(hir::MatchArm {
                pattern,
                guard,
                body,
                span: arm.span,
            });
        }
        self.moved_locals = moves_after_arms;
        self.moved_fields = field_moves_after_arms;
        self.validate_match_coverage(scrutinee.ty, &arms, expression.span);
        let ty = result_type.unwrap_or(Type::Never);
        Expression {
            kind: ExpressionKind::Match(Box::new(hir::MatchExpression { scrutinee, arms })),
            ty,
            span: expression.span,
        }
    }

    fn record_pattern_scoped_roots(&mut self, pattern: &hir::Pattern, root: LocalId) {
        match &pattern.kind {
            hir::PatternKind::Binding { local, .. } => {
                if self.types.is_scoped(pattern.ty) {
                    self.scoped_roots.insert(*local, root);
                }
            }
            hir::PatternKind::Tuple(elements)
            | hir::PatternKind::Enum {
                fields: elements, ..
            } => {
                for element in elements {
                    self.record_pattern_scoped_roots(element, root);
                }
            }
            hir::PatternKind::Wildcard
            | hir::PatternKind::Integer(_)
            | hir::PatternKind::Float32(_)
            | hir::PatternKind::Float64(_)
            | hir::PatternKind::Character(_)
            | hir::PatternKind::Boolean(_) => {}
        }
    }

    fn analyze_loop(
        &mut self,
        expression: &ast::LoopExpression,
        expected: Option<Type>,
    ) -> Expression {
        let context_index = self.loops.len();
        self.loops.push(LoopContext {
            kind: LoopKind::Expression,
            expected,
            break_type: None,
        });
        let body = self.analyze_block(&expression.body, None);
        let ty = self
            .loops
            .get(context_index)
            .and_then(|context| context.break_type)
            .unwrap_or(Type::Never);
        self.loops.truncate(context_index);
        Expression {
            kind: ExpressionKind::Loop(Box::new(hir::LoopExpression { body })),
            ty,
            span: expression.span,
        }
    }

    fn analyze_pattern(&mut self, pattern: &ast::Pattern, expected: Type) -> hir::Pattern {
        let kind = match pattern {
            ast::Pattern::Wildcard(_) => hir::PatternKind::Wildcard,
            ast::Pattern::Identifier { mutable, name, .. } => {
                self.analyze_identifier_pattern(*mutable, name, expected)
            }
            ast::Pattern::Integer {
                value,
                negative,
                span,
            } => hir::PatternKind::Integer(
                self.analyze_pattern_integer(*value, *negative, expected, *span),
            ),
            ast::Pattern::Float {
                bits,
                negative,
                span,
            } => self.analyze_pattern_float(*bits, *negative, expected, *span),
            ast::Pattern::Character(literal) => {
                self.require_type(Type::Char, expected, literal.span, "character pattern");
                hir::PatternKind::Character(literal.value)
            }
            ast::Pattern::Boolean(literal) => {
                self.require_type(Type::Bool, expected, literal.span, "boolean pattern");
                hir::PatternKind::Boolean(literal.value)
            }
            ast::Pattern::Tuple { elements, span } => {
                self.analyze_tuple_pattern(elements, expected, *span)
            }
            ast::Pattern::Path(path) => self.analyze_unit_variant_pattern(path, expected),
            ast::Pattern::EnumTuple { path, fields, span } => {
                self.analyze_enum_tuple_pattern(path, fields, expected, *span)
            }
            ast::Pattern::EnumStruct { path, fields, span } => {
                self.analyze_enum_struct_pattern(path, fields, expected, *span)
            }
        };
        hir::Pattern {
            kind,
            ty: expected,
            span: pattern.span(),
        }
    }

    fn analyze_identifier_pattern(
        &mut self,
        mutable: bool,
        name: &ast::Identifier,
        expected: Type,
    ) -> hir::PatternKind {
        if !mutable
            && matches!(expected, Type::Enum(_))
            && let Some((variant, fields)) = self.enum_variant(expected, &name.name)
        {
            if fields == hir::EnumVariantFields::Unit {
                return hir::PatternKind::Enum {
                    variant,
                    fields: Vec::new(),
                };
            }
            self.diagnostics.push(
                Diagnostic::error(
                    "E3134",
                    format!("variant `{}` requires a payload pattern", name.name),
                    name.span,
                )
                .with_help("add positional patterns in parentheses"),
            );
            return hir::PatternKind::Wildcard;
        }
        let local = self.new_local(name.span);
        if self
            .current_scope()
            .insert(
                name.name.clone(),
                Binding {
                    local,
                    ty: expected,
                    mutable,
                },
            )
            .is_some()
        {
            self.diagnostics.push(Diagnostic::error(
                "E3132",
                format!("pattern binding `{}` is declared more than once", name.name),
                name.span,
            ));
        }
        hir::PatternKind::Binding {
            local,
            name: name.name.clone(),
            mutable,
        }
    }

    fn analyze_pattern_integer(
        &mut self,
        value: u128,
        negative: bool,
        expected: Type,
        span: Span,
    ) -> u128 {
        if !expected.is_integer() || (negative && !expected.is_signed_integer()) {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("integer pattern is not valid for `{expected}`"),
                span,
            ));
            return 0;
        }
        let maximum = if negative {
            integer_minimum_magnitude(expected)
        } else {
            integer_positive_maximum(expected)
        };
        if value > maximum {
            self.diagnostics.push(Diagnostic::error(
                "E3104",
                format!("integer pattern does not fit in `{expected}`"),
                span,
            ));
            return 0;
        }
        if negative {
            0_u128.wrapping_sub(value)
        } else {
            value
        }
    }

    fn analyze_pattern_float(
        &mut self,
        bits: u64,
        negative: bool,
        expected: Type,
        span: Span,
    ) -> hir::PatternKind {
        if !expected.is_float() {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("floating-point pattern is not valid for `{expected}`"),
                span,
            ));
            return hir::PatternKind::Float64(0);
        }
        let value = if negative {
            -f64::from_bits(bits)
        } else {
            f64::from_bits(bits)
        };
        if expected == Type::F32 {
            let value = narrow_f64_to_f32(value);
            if !value.is_finite() {
                self.diagnostics.push(Diagnostic::error(
                    "E3104",
                    "floating-point pattern does not fit in `f32`",
                    span,
                ));
            }
            hir::PatternKind::Float32(value.to_bits())
        } else {
            hir::PatternKind::Float64(value.to_bits())
        }
    }

    fn analyze_tuple_pattern(
        &mut self,
        elements: &[ast::Pattern],
        expected: Type,
        span: Span,
    ) -> hir::PatternKind {
        let Some(types) = self.tuple_elements(expected) else {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("tuple pattern is not valid for `{expected}`"),
                span,
            ));
            return hir::PatternKind::Tuple(Vec::new());
        };
        if types.len() != elements.len() {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!(
                    "tuple pattern requires {} element(s), found {}",
                    types.len(),
                    elements.len()
                ),
                span,
            ));
        }
        let patterns = elements
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                self.analyze_pattern(pattern, types.get(index).copied().unwrap_or(Type::Unit))
            })
            .collect();
        hir::PatternKind::Tuple(patterns)
    }

    fn analyze_unit_variant_pattern(
        &mut self,
        path: &ast::Path,
        expected: Type,
    ) -> hir::PatternKind {
        let Some((variant, fields)) = self.resolve_pattern_variant(expected, path) else {
            return hir::PatternKind::Wildcard;
        };
        if fields != hir::EnumVariantFields::Unit {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("variant `{}` requires a payload pattern", path.display()),
                path.span,
            ));
            return hir::PatternKind::Wildcard;
        }
        hir::PatternKind::Enum {
            variant,
            fields: Vec::new(),
        }
    }

    fn analyze_enum_tuple_pattern(
        &mut self,
        path: &ast::Path,
        fields: &[ast::Pattern],
        expected: Type,
        span: Span,
    ) -> hir::PatternKind {
        let Some((variant, variant_fields)) = self.resolve_pattern_variant(expected, path) else {
            return hir::PatternKind::Wildcard;
        };
        let hir::EnumVariantFields::Tuple(types) = variant_fields else {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("variant `{}` is not tuple-like", path.display()),
                span,
            ));
            return hir::PatternKind::Wildcard;
        };
        if types.len() != fields.len() {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!(
                    "variant pattern requires {} field(s), found {}",
                    types.len(),
                    fields.len()
                ),
                span,
            ));
        }
        let fields = fields
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                self.analyze_pattern(pattern, types.get(index).copied().unwrap_or(Type::Unit))
            })
            .collect();
        hir::PatternKind::Enum { variant, fields }
    }

    fn analyze_enum_struct_pattern(
        &mut self,
        path: &ast::Path,
        fields: &[ast::PatternField],
        expected: Type,
        span: Span,
    ) -> hir::PatternKind {
        let Some((variant, variant_fields)) = self.resolve_pattern_variant(expected, path) else {
            return hir::PatternKind::Wildcard;
        };
        let hir::EnumVariantFields::Struct(declared) = variant_fields else {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("variant `{}` is not struct-like", path.display()),
                span,
            ));
            return hir::PatternKind::Wildcard;
        };
        if self.type_is_external(expected)
            && fields.iter().any(|provided| {
                declared
                    .iter()
                    .any(|field| field.name == provided.name.name && !field.is_public)
            })
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E4005",
                    "enum payload field is private to its defining module",
                    span,
                )
                .with_help("match the variant without naming private fields"),
            );
        }
        let mut provided = HashMap::new();
        for field in fields {
            if provided.insert(field.name.name.as_str(), field).is_some() {
                self.diagnostics.push(Diagnostic::error(
                    "E3132",
                    format!(
                        "pattern field `{}` is specified more than once",
                        field.name.name
                    ),
                    field.span,
                ));
            }
            if !declared
                .iter()
                .any(|declared| declared.name == field.name.name)
            {
                self.diagnostics.push(Diagnostic::error(
                    "E3121",
                    format!("variant has no field named `{}`", field.name.name),
                    field.name.span,
                ));
            }
        }
        let mut resolved = Vec::with_capacity(declared.len());
        for field in &declared {
            if let Some(provided) = provided.get(field.name.as_str()) {
                resolved.push(self.analyze_pattern(&provided.pattern, field.ty));
            } else {
                self.diagnostics.push(Diagnostic::error(
                    "E3122",
                    format!("missing pattern for field `{}`", field.name),
                    span,
                ));
                resolved.push(hir::Pattern {
                    kind: hir::PatternKind::Wildcard,
                    ty: field.ty,
                    span,
                });
            }
        }
        hir::PatternKind::Enum {
            variant,
            fields: resolved,
        }
    }

    fn resolve_pattern_variant(
        &mut self,
        expected: Type,
        path: &ast::Path,
    ) -> Option<(u32, hir::EnumVariantFields)> {
        let Type::Enum(_) = expected else {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("enum pattern is not valid for `{expected}`"),
                path.span,
            ));
            return None;
        };
        let variant_name = path.segments.last()?;
        if path.segments.len() > 2 {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("invalid enum pattern path `{}`", path.display()),
                path.span,
            ));
            return None;
        }
        if let [type_name, _] = path.segments.as_slice() {
            let actual_name = self
                .types
                .definition(expected)
                .and_then(|definition| definition.name.as_deref());
            if actual_name != Some(type_name.name.as_str()) {
                self.diagnostics.push(Diagnostic::error(
                    "E3134",
                    format!("`{}` is not a variant of `{expected}`", path.display()),
                    path.span,
                ));
                return None;
            }
        }
        self.enum_variant(expected, &variant_name.name).or_else(|| {
            self.diagnostics.push(Diagnostic::error(
                "E3134",
                format!("enum variant `{}` does not exist", path.display()),
                path.span,
            ));
            None
        })
    }

    fn validate_match_coverage(&mut self, ty: Type, arms: &[hir::MatchArm], span: Span) {
        if match_is_exhaustive(self.types, ty, arms) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::error("E3133", format!("non-exhaustive match on `{ty}`"), span)
                .with_help("add the missing cases or a final `_` arm"),
        );
    }

    fn analyze_assignment(&mut self, assignment: &ast::AssignmentExpression) -> Expression {
        if let AstExpression::Index(index) = &assignment.target
            && self.supports_index_method(&index.base, "set_index")
        {
            if assignment.operator != AstAssignmentOperator::Assign {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3125",
                        "compound tensor indexing assignment is not supported",
                        assignment.span,
                    )
                    .with_help("read the element, compute the value, then assign it with `=`"),
                );
                self.analyze_expression(&assignment.value);
                return invalid_composite_expression(assignment.span);
            }
            return self.analyze_index_method_call(index, "set_index", Some(&assignment.value));
        }
        let previous_place_availability = self.place_availability;
        self.place_availability = if assignment.operator == AstAssignmentOperator::Assign {
            PlaceAvailability::AllowReinitialization
        } else {
            PlaceAvailability::InitializedOnly
        };
        let target = self.analyze_place(&assignment.target);
        self.place_availability = previous_place_availability;
        let Some(target) = target else {
            self.analyze_expression(&assignment.value);
            return invalid_composite_expression(assignment.span);
        };
        self.require_mutable_place(&assignment.target);
        let value = self.analyze_expression_expected(&assignment.value, Some(target.ty));
        let operator = match assignment.operator {
            AstAssignmentOperator::Assign => AssignmentOperator::Assign,
            AstAssignmentOperator::Add => AssignmentOperator::Add,
            AstAssignmentOperator::Subtract => AssignmentOperator::Subtract,
            AstAssignmentOperator::Multiply => AssignmentOperator::Multiply,
            AstAssignmentOperator::Divide => AssignmentOperator::Divide,
            AstAssignmentOperator::Remainder => AssignmentOperator::Remainder,
            AstAssignmentOperator::BitAnd => AssignmentOperator::BitAnd,
            AstAssignmentOperator::BitXor => AssignmentOperator::BitXor,
            AstAssignmentOperator::BitOr => AssignmentOperator::BitOr,
            AstAssignmentOperator::ShiftLeft => AssignmentOperator::ShiftLeft,
            AstAssignmentOperator::ShiftRight => AssignmentOperator::ShiftRight,
        };
        if let PlaceKind::Local(local) = &target.kind {
            let available_to_update = self.require_not_reserved_by_defer(*local, target.span);
            if operator == AssignmentOperator::Assign {
                if available_to_update {
                    self.moved_locals.remove(local);
                    self.moved_fields.retain(|field, _| field.local != *local);
                }
            } else {
                self.require_local_available(
                    Binding {
                        local: *local,
                        ty: target.ty,
                        mutable: true,
                    },
                    target.span,
                    true,
                );
            }
        } else if operator == AssignmentOperator::Assign
            && let Some((local, projections)) = self.field_place(&assignment.target)
        {
            let place = MovedField { local, projections };
            if self.require_not_reserved_by_defer(local, target.span) {
                self.restore_field(&place);
            }
        }
        self.require_type(target.ty, value.ty, value.span, "assignment value");
        let valid = match operator {
            AssignmentOperator::Assign => true,
            AssignmentOperator::Add
            | AssignmentOperator::Subtract
            | AssignmentOperator::Multiply
            | AssignmentOperator::Divide => target.ty.is_numeric(),
            AssignmentOperator::Remainder
            | AssignmentOperator::BitAnd
            | AssignmentOperator::BitXor
            | AssignmentOperator::BitOr
            | AssignmentOperator::ShiftLeft
            | AssignmentOperator::ShiftRight => target.ty.is_integer(),
        };
        if !valid {
            self.invalid_operator("compound assignment", target.ty, assignment.span);
        }
        Expression {
            kind: ExpressionKind::Assign {
                target,
                operator,
                value: Box::new(value),
            },
            ty: Type::Unit,
            span: assignment.span,
        }
    }

    fn analyze_place(&mut self, expression: &AstExpression) -> Option<Place> {
        match expression {
            AstExpression::Path(path) => {
                if let Some(binding) = single_path_name(path).and_then(|name| self.lookup(name)) {
                    return Some(Place {
                        kind: PlaceKind::Local(binding.local),
                        ty: binding.ty,
                        span: path.span,
                    });
                }
                if let Some(value) = single_path_name(path)
                    .and_then(|name| self.statics.get(name))
                    .cloned()
                {
                    self.require_static_access(&value, path.span);
                    return Some(Place {
                        kind: PlaceKind::Static(value.id),
                        ty: value.ty,
                        span: path.span,
                    });
                }
                self.diagnostics.push(Diagnostic::error(
                    "E3102",
                    format!("cannot resolve binding `{}`", path.display()),
                    path.span,
                ));
                None
            }
            AstExpression::Field(_) => {
                let analyzed = self.analyze_expression_non_consuming(expression);
                let ExpressionKind::Field { base, field } = analyzed.kind else {
                    return None;
                };
                Some(Place {
                    kind: PlaceKind::Field { base, field },
                    ty: analyzed.ty,
                    span: analyzed.span,
                })
            }
            AstExpression::Index(_) => {
                let analyzed = self.analyze_expression_non_consuming(expression);
                let ExpressionKind::Index { base, index } = analyzed.kind else {
                    return None;
                };
                Some(Place {
                    kind: PlaceKind::Index { base, index },
                    ty: analyzed.ty,
                    span: analyzed.span,
                })
            }
            AstExpression::Unary(unary) if unary.operator == AstUnaryOperator::Dereference => {
                let analyzed = self.analyze_expression_non_consuming(expression);
                let ExpressionKind::Dereference(pointer) = analyzed.kind else {
                    return None;
                };
                Some(Place {
                    kind: PlaceKind::Dereference { pointer },
                    ty: analyzed.ty,
                    span: analyzed.span,
                })
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3108",
                        "assignment target must be a local, field, or array element",
                        expression.span(),
                    )
                    .with_help("assign to `name`, `value.field`, or `values[index]`"),
                );
                None
            }
        }
    }

    fn place_expression_type(&self, expression: &AstExpression) -> Option<Type> {
        match expression {
            AstExpression::Path(path) => single_path_name(path).and_then(|name| {
                self.lookup(name)
                    .map(|binding| binding.ty)
                    .or_else(|| self.statics.get(name).map(|value| value.ty))
            }),
            AstExpression::Field(field) => {
                let base = self.place_expression_type(&field.base)?;
                let base = self
                    .types
                    .pointer_shape(base)
                    .filter(|(_, _, raw)| !raw)
                    .map_or(base, |(target, _, _)| target);
                match (base, &field.field) {
                    (Type::Struct(_), ast::FieldName::Named(name)) => self
                        .struct_fields(base)
                        .iter()
                        .find(|field| field.name == name.name)
                        .map(|field| field.ty),
                    (Type::Tuple(_), ast::FieldName::TupleIndex { index, .. }) => {
                        self.tuple_elements(base).and_then(|elements| {
                            usize::try_from(*index)
                                .ok()
                                .and_then(|i| elements.get(i).copied())
                        })
                    }
                    _ => None,
                }
            }
            AstExpression::Index(index) if index.indices.len() == 1 => {
                let base = self.place_expression_type(&index.base)?;
                self.array_shape(base)
                    .map(|(element, _)| element)
                    .or_else(|| self.types.slice_shape(base).map(|(element, _)| element))
            }
            AstExpression::Unary(unary) if unary.operator == AstUnaryOperator::Dereference => {
                let pointer = self.place_expression_type(&unary.operand)?;
                self.types
                    .pointer_shape(pointer)
                    .map(|(target, _, _)| target)
            }
            _ => None,
        }
    }

    fn temporary_method_receiver_type(&mut self, expression: &AstExpression) -> Option<Type> {
        let AstExpression::Call(call) = expression else {
            return None;
        };
        match &call.callee {
            AstExpression::Path(path) => function_path_name(path)
                .and_then(|name| self.signatures.get(&name))
                .map(|signature| signature.return_type),
            AstExpression::Field(field) => {
                let receiver = self
                    .place_expression_type(&field.base)
                    .or_else(|| self.temporary_method_receiver_type(&field.base))?;
                let owner = self.types.nominal_name(receiver)?;
                let ast::FieldName::Named(method) = &field.field else {
                    return None;
                };
                let resolved_name = format!("{owner}::{}", method.name);
                self.signatures
                    .get(&resolved_name)
                    .map(|signature| signature.return_type)
                    .or_else(|| {
                        self.temporary_generic_method_return_type(
                            call,
                            method,
                            receiver,
                            &resolved_name,
                        )
                    })
            }
            _ => None,
        }
    }

    fn temporary_generic_method_return_type(
        &mut self,
        call: &ast::CallExpression,
        method: &ast::Identifier,
        receiver_type: Type,
        resolved_name: &str,
    ) -> Option<Type> {
        let nominal_receiver = self
            .types
            .pointer_shape(receiver_type)
            .filter(|(_, _, raw)| !raw)
            .map_or(receiver_type, |(target, _, _)| target);
        let template_name = self.types.generic_instance(nominal_receiver).map_or_else(
            || resolved_name.to_owned(),
            |instance| format!("{}::{}", instance.base_name, method.name),
        );
        let template = self
            .generic_functions
            .templates
            .get(&template_name)
            .cloned()?;
        let diagnostic_count = self.diagnostics.len();
        let result = self.infer_temporary_generic_method_signature(call, receiver_type, &template);
        if result.is_none() {
            self.diagnostics.truncate(diagnostic_count);
        }
        result.map(|signature| signature.return_type)
    }

    fn infer_temporary_generic_method_signature(
        &mut self,
        call: &ast::CallExpression,
        receiver_type: Type,
        template: &GenericFunctionTemplate,
    ) -> Option<Signature> {
        let receiver_parameter = template.function.parameters.first()?;
        let receiver_argument_type = match &receiver_parameter.ty.kind {
            TypeNameKind::Reference { mutable, .. } => match self
                .types
                .pointer_shape(receiver_type)
                .filter(|(_, _, raw)| !raw)
            {
                Some((_, actual_mutable, _)) if actual_mutable == *mutable => receiver_type,
                Some((target, true, _)) if !*mutable => {
                    self.types
                        .intern_reference(target, false, call.span, self.diagnostics)?
                }
                Some(_) => receiver_type,
                None => self.types.intern_reference(
                    receiver_type,
                    *mutable,
                    call.span,
                    self.diagnostics,
                )?,
            },
            _ => receiver_type,
        };
        let mut argument_types = Vec::with_capacity(call.arguments.len() + 1);
        argument_types.push(receiver_argument_type);
        for argument in &call.arguments {
            argument_types.push(self.temporary_expression_type(argument)?);
        }
        if argument_types.len() != template.function.parameters.len() {
            return None;
        }

        let mut environment = self.types.base_environment();
        let explicit_parameters = template
            .parameters
            .get(template.explicit_parameter_start..)
            .unwrap_or_default();
        self.bind_explicit_generic_arguments(call, explicit_parameters, &mut environment);
        for (parameter, actual) in template.function.parameters.iter().zip(argument_types) {
            if !self.types.infer_type_pattern(
                &parameter.ty,
                actual,
                &template.parameters,
                &mut environment,
            ) {
                return None;
            }
        }
        let values = self.complete_generic_environment(
            &template.parameters,
            &template.where_predicates,
            &mut environment,
            call.span,
        )?;
        self.instantiate_generic_function(template, values, &environment, call.span)
    }

    fn temporary_expression_type(&mut self, expression: &AstExpression) -> Option<Type> {
        if let Some(ty) = self.place_expression_type(expression) {
            return Some(ty);
        }
        match expression {
            AstExpression::Call(_) => self.temporary_method_receiver_type(expression),
            AstExpression::Path(path) => {
                let signature = function_path_name(path)
                    .and_then(|name| self.signatures.get(&name))
                    .cloned()?;
                self.types.intern_function(
                    signature.parameter_types,
                    signature.return_type,
                    path.span,
                    self.diagnostics,
                )
            }
            _ => None,
        }
    }

    fn require_mutable_place(&mut self, expression: &AstExpression) {
        if let AstExpression::Unary(unary) = expression
            && unary.operator == AstUnaryOperator::Dereference
        {
            let pointer = self.analyze_expression_non_consuming(&unary.operand);
            let Some((_, mutable, _)) = self.types.pointer_shape(pointer.ty) else {
                return;
            };
            if !mutable {
                self.diagnostics.push(Diagnostic::error(
                    "E3107",
                    "cannot assign through an immutable reference or pointer",
                    expression.span(),
                ));
            }
            return;
        }
        if let AstExpression::Index(index) = expression
            && let Some(base_type) = self.place_expression_type(&index.base)
        {
            let indirectly_mutable = self
                .types
                .slice_shape(base_type)
                .is_some_and(|(_, mutable)| mutable)
                || self
                    .types
                    .pointer_shape(base_type)
                    .is_some_and(|(_, mutable, _)| mutable);
            if indirectly_mutable {
                return;
            }
        }
        if matches!(
            expression,
            AstExpression::Field(_) | AstExpression::Index(_)
        ) && let Some(path) = assignment_root_path(expression)
            && let Some(binding) = single_path_name(path).and_then(|name| self.lookup(name))
            && self
                .types
                .pointer_shape(binding.ty)
                .is_some_and(|(_, mutable, raw)| mutable && !raw)
        {
            return;
        }
        let Some(path) = assignment_root_path(expression) else {
            self.diagnostics.push(Diagnostic::error(
                "E3108",
                "assignment target is not rooted in a local binding",
                expression.span(),
            ));
            return;
        };
        let Some(name) = single_path_name(path) else {
            return;
        };
        if let Some(value) = self.statics.get(name) {
            if !value.mutable {
                self.diagnostics.push(
                    Diagnostic::error(
                        "E3107",
                        format!("cannot assign through immutable static `{name}`"),
                        path.span,
                    )
                    .with_help(format!(
                        "declare it as `static mut {name}` and access it in `unsafe`"
                    )),
                );
            }
            return;
        }
        let Some(binding) = self.lookup(name) else {
            return;
        };
        if self
            .borrow_states
            .get(&binding.local)
            .is_some_and(|state| state.mutable || state.shared != 0)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3138",
                    format!("cannot mutate `{name}` while it is borrowed"),
                    path.span,
                )
                .with_help("end the reference's scope before mutating the original binding"),
            );
            return;
        }
        if !binding.mutable {
            self.diagnostics.push(
                Diagnostic::error(
                    "E3107",
                    format!("cannot assign through immutable binding `{name}`"),
                    path.span,
                )
                .with_help(format!("declare it as `let mut {name}`")),
            );
        }
    }

    fn analyze_cast(&mut self, cast: &ast::CastExpression) -> Expression {
        let target = self
            .types
            .resolve_type_name_in(&cast.target, &self.generic_environment, self.diagnostics)
            .unwrap_or(Type::Unit);
        let value = self.analyze_expression(&cast.value);
        if matches!(target, Type::Function(_))
            && value.ty != target
            && value.ty != Type::Never
            && self.unsafe_depth == 0
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "E5002",
                    "casting an address to a function pointer requires `unsafe`",
                    cast.span,
                )
                .with_help(
                    "validate the loaded symbol and perform the cast inside `unsafe { ... }`",
                ),
            );
        }
        if value.ty != Type::Never && !is_valid_cast(value.ty, target) {
            let source_name = self.types.reflection_type_name(value.ty);
            let target_name = self.types.reflection_type_name(target);
            self.diagnostics.push(
                Diagnostic::error(
                    "E3113",
                    format!("cannot cast `{source_name}` to `{target_name}`"),
                    cast.span,
                )
                .with_help(
                    "use `as` only for integer, floating-point, or character-to-integer conversions",
                ),
            );
        }
        Expression {
            kind: ExpressionKind::Cast {
                value: Box::new(value),
                target,
            },
            ty: target,
            span: cast.span,
        }
    }

    fn unify_branch_types(&mut self, left: Type, right: Type, span: Span) -> Type {
        if left == Type::Never {
            return right;
        }
        if right == Type::Never || left == right {
            return left;
        }
        self.type_mismatch(left, right, span, "`if` branches");
        Type::Unit
    }

    fn require_type(&mut self, expected: Type, actual: Type, span: Span, role: &str) {
        if !expected.accepts(actual) {
            self.type_mismatch(expected, actual, span, role);
        }
    }

    fn type_mismatch(&mut self, expected: Type, actual: Type, span: Span, role: &str) {
        self.diagnostics.push(Diagnostic::error(
            "E3103",
            format!("{role} requires `{expected}`, found `{actual}`"),
            span,
        ));
    }

    fn invalid_operator(&mut self, role: &str, ty: Type, span: Span) {
        self.diagnostics.push(
            Diagnostic::error("E3112", format!("{role} is not defined for `{ty}`"), span)
                .with_help("use an operator supported by this operand type"),
        );
    }

    fn struct_fields(&self, ty: Type) -> Vec<hir::TypeField> {
        let Some(definition) = self.types.definition(ty) else {
            return Vec::new();
        };
        let hir::TypeDefinitionKind::Struct { fields } = &definition.kind else {
            return Vec::new();
        };
        fields.clone()
    }

    fn reject_external_private_construction(
        &mut self,
        ty: Type,
        fields: &[hir::TypeField],
        span: Span,
    ) {
        if self.type_is_external(ty) && fields.iter().any(|field| !field.is_public) {
            self.diagnostics.push(
                Diagnostic::error(
                    "E4005",
                    "cannot construct an external aggregate with private fields",
                    span,
                )
                .with_help("use a public constructor function from the defining module"),
            );
        }
    }

    fn type_is_external(&self, ty: Type) -> bool {
        let module = self
            .types
            .definition(ty)
            .and_then(|definition| definition.name.as_deref())
            .and_then(symbol_module_identity);
        module != self.module_identity.as_deref()
    }

    fn tuple_elements(&self, ty: Type) -> Option<Vec<Type>> {
        let definition = self.types.definition(ty)?;
        let hir::TypeDefinitionKind::Tuple { elements } = &definition.kind else {
            return None;
        };
        Some(elements.clone())
    }

    fn array_shape(&self, ty: Type) -> Option<(Type, u64)> {
        let definition = self.types.definition(ty)?;
        let hir::TypeDefinitionKind::Array { element, length } = definition.kind else {
            return None;
        };
        Some((element, length))
    }

    fn enum_variant(&self, ty: Type, name: &str) -> Option<(u32, hir::EnumVariantFields)> {
        let definition = self.types.definition(ty)?;
        let hir::TypeDefinitionKind::Enum { variants } = &definition.kind else {
            return None;
        };
        variants
            .iter()
            .enumerate()
            .find(|(_, variant)| variant.name == name)
            .and_then(|(index, variant)| {
                u32::try_from(index)
                    .ok()
                    .map(|index| (index, variant.fields.clone()))
            })
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.borrow_scopes.push(Vec::new());
        self.deferred_use_scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        if let Some(locals) = self.deferred_use_scopes.pop() {
            for local in locals {
                self.deferred_uses.remove(&local);
            }
        }
        if let Some(borrows) = self.borrow_scopes.pop() {
            for (local, mutable) in borrows {
                let Some(state) = self.borrow_states.get_mut(&local) else {
                    continue;
                };
                if mutable {
                    state.mutable = false;
                } else {
                    state.shared = state.shared.saturating_sub(1);
                }
                if state.shared == 0 && !state.mutable {
                    self.borrow_states.remove(&local);
                }
            }
        }
        self.scopes.pop();
    }

    fn current_scope(&mut self) -> &mut HashMap<String, Binding> {
        let index = self.scopes.len().saturating_sub(1);
        &mut self.scopes[index]
    }

    fn new_local(&mut self, span: Span) -> LocalId {
        let local = LocalId(self.next_local);
        if let Some(next) = self.next_local.checked_add(1) {
            self.next_local = next;
        } else {
            self.diagnostics.push(Diagnostic::error(
                "E3999",
                "function contains too many local bindings",
                span,
            ));
        }
        local
    }
}

fn primitive_type(name: &str) -> Option<Type> {
    Some(match name {
        "i8" | "c_schar" => Type::I8,
        "i16" | "c_short" => Type::I16,
        "i32" | "c_int" => Type::I32,
        "i64" | "c_longlong" => Type::I64,
        "i128" => Type::I128,
        "isize" | "c_ptrdiff" => Type::Isize,
        "u8" | "c_uchar" => Type::U8,
        "u16" | "c_ushort" => Type::U16,
        "u32" | "c_uint" => Type::U32,
        "u64" | "c_ulonglong" => Type::U64,
        "u128" => Type::U128,
        "usize" | "c_size" => Type::Usize,
        "f32" | "c_float" => Type::F32,
        "f64" | "c_double" => Type::F64,
        "bool" => Type::Bool,
        "char" => Type::Char,
        "str" => Type::Str,
        "cstr" => Type::CStr,
        "c_char" => {
            if std::ffi::c_char::MIN == 0 {
                Type::U8
            } else {
                Type::I8
            }
        }
        "c_long" => {
            if size_of::<std::ffi::c_long>() == size_of::<i32>() {
                Type::I32
            } else {
                Type::I64
            }
        }
        "c_ulong" => {
            if size_of::<std::ffi::c_ulong>() == size_of::<u32>() {
                Type::U32
            } else {
                Type::U64
            }
        }
        "never" => Type::Never,
        _ => return None,
    })
}

fn validate_generic_parameter_names(
    parameters: &[ast::GenericParameter],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut names = HashMap::new();
    let mut saw_default = false;
    for (index, parameter) in parameters.iter().enumerate() {
        let name = parameter.name();
        if names.insert(name.name.as_str(), name.span).is_some() {
            diagnostics.push(Diagnostic::error(
                "E6005",
                format!(
                    "generic parameter `{}` is declared more than once",
                    name.name
                ),
                name.span,
            ));
        }
        let has_default = match parameter {
            ast::GenericParameter::Type { default, .. } => default.is_some(),
            ast::GenericParameter::Const { default, .. } => default.is_some(),
            ast::GenericParameter::TypePack { .. } => false,
        };
        if matches!(parameter, ast::GenericParameter::TypePack { .. })
            && index + 1 != parameters.len()
        {
            diagnostics.push(
                Diagnostic::error(
                    "E6020",
                    "a type pack must be the final generic parameter",
                    parameter.span(),
                )
                .with_help("move `<...Types>` to the end of the generic parameter list"),
            );
        }
        if saw_default && !has_default {
            diagnostics.push(
                Diagnostic::error(
                    "E6005",
                    "required generic parameters cannot follow a defaulted parameter",
                    parameter.span(),
                )
                .with_help("move required parameters before parameters with defaults"),
            );
        }
        saw_default |= has_default;
    }
}

fn generic_type_parameter(parameters: &[ast::GenericParameter], name: &str) -> bool {
    parameters.iter().any(|parameter| {
        matches!(
            parameter,
            ast::GenericParameter::Type {
                name: parameter_name,
                ..
            } | ast::GenericParameter::TypePack {
                name: parameter_name,
                ..
            } if parameter_name.name == name
        )
    })
}

fn generic_const_parameter(parameters: &[ast::GenericParameter], name: &str) -> bool {
    parameters.iter().any(|parameter| {
        matches!(
            parameter,
            ast::GenericParameter::Const {
                name: parameter_name,
                ..
            } if parameter_name.name == name
        )
    })
}

fn bind_type_argument(environment: &mut GenericEnvironment, name: &str, ty: Type) -> bool {
    if let Some(existing) = environment.types.get(name) {
        *existing == ty
    } else {
        environment.types.insert(name.to_owned(), ty);
        true
    }
}

fn bind_type_pack_argument(
    environment: &mut GenericEnvironment,
    name: &str,
    types: Vec<Type>,
) -> bool {
    if let Some(existing) = environment.type_packs.get(name) {
        existing == &types
    } else {
        environment.type_packs.insert(name.to_owned(), types);
        true
    }
}

fn bind_const_argument(environment: &mut GenericEnvironment, name: &str, value: u64) -> bool {
    if let Some(existing) = environment.constants.get(name) {
        *existing == value
    } else {
        environment.constants.insert(name.to_owned(), value);
        true
    }
}

fn infer_const_pattern(
    pattern: &AstExpression,
    actual: u64,
    parameters: &[ast::GenericParameter],
    environment: &mut GenericEnvironment,
) -> bool {
    if let AstExpression::Path(path) = pattern
        && let Some(name) = single_path_name(path)
        && generic_const_parameter(parameters, name)
    {
        return bind_const_argument(environment, name, actual);
    }
    let mut ignored_diagnostics = Vec::new();
    evaluate_array_length_in(pattern, environment, &mut ignored_diagnostics)
        .is_some_and(|expected| expected == actual)
}

fn type_pattern_is_bound(
    pattern: &ast::TypeName,
    parameters: &[ast::GenericParameter],
    environment: &GenericEnvironment,
) -> bool {
    match &pattern.kind {
        TypeNameKind::Function {
            parameters: parameter_types,
            return_type,
        } => {
            parameter_types
                .iter()
                .all(|parameter| type_pattern_is_bound(parameter, parameters, environment))
                && type_pattern_is_bound(return_type, parameters, environment)
        }
        TypeNameKind::Path(path) => single_path_name(path).is_none_or(|name| {
            !generic_type_parameter(parameters, name) || environment.types.contains_key(name)
        }),
        TypeNameKind::Generic { arguments, .. } => {
            arguments.iter().all(|argument| match argument {
                ast::GenericArgument::Type(ty) => {
                    if let TypeNameKind::Path(path) = &ty.kind
                        && let Some(name) = single_path_name(path)
                        && generic_const_parameter(parameters, name)
                    {
                        environment.constants.contains_key(name)
                    } else {
                        type_pattern_is_bound(ty, parameters, environment)
                    }
                }
                ast::GenericArgument::Const(value) => {
                    const_pattern_is_bound(value, parameters, environment)
                }
                ast::GenericArgument::Pack { pack, template, .. } => {
                    environment.type_packs.contains_key(&pack.name)
                        && template.as_ref().is_none_or(|template| {
                            let mut element_environment = environment.clone();
                            environment
                                .type_packs
                                .get(&pack.name)
                                .and_then(|types| types.first())
                                .is_none_or(|ty| {
                                    element_environment.types.insert(pack.name.clone(), *ty);
                                    type_pattern_is_bound(
                                        template,
                                        parameters,
                                        &element_environment,
                                    )
                                })
                        })
                }
            })
        }
        TypeNameKind::PackExpansion { pack, template } => {
            environment.type_packs.contains_key(&pack.name)
                && template.as_ref().is_none_or(|template| {
                    let mut element_environment = environment.clone();
                    environment
                        .type_packs
                        .get(&pack.name)
                        .and_then(|types| types.first())
                        .is_none_or(|ty| {
                            element_environment.types.insert(pack.name.clone(), *ty);
                            type_pattern_is_bound(template, parameters, &element_environment)
                        })
                })
        }
        TypeNameKind::Tuple(elements) => elements
            .iter()
            .all(|element| type_pattern_is_bound(element, parameters, environment)),
        TypeNameKind::Array { element, length } => {
            type_pattern_is_bound(element, parameters, environment)
                && const_pattern_is_bound(length, parameters, environment)
        }
        TypeNameKind::Slice(element) => type_pattern_is_bound(element, parameters, environment),
        TypeNameKind::Reference { target, .. } | TypeNameKind::RawPointer { target, .. } => {
            type_pattern_is_bound(target, parameters, environment)
        }
        TypeNameKind::Unit => true,
    }
}

fn const_pattern_is_bound(
    expression: &AstExpression,
    parameters: &[ast::GenericParameter],
    environment: &GenericEnvironment,
) -> bool {
    match expression {
        AstExpression::Path(path) => single_path_name(path).is_none_or(|name| {
            !generic_const_parameter(parameters, name) || environment.constants.contains_key(name)
        }),
        AstExpression::Binary(expression) => {
            const_pattern_is_bound(&expression.left, parameters, environment)
                && const_pattern_is_bound(&expression.right, parameters, environment)
        }
        AstExpression::Integer(_) => true,
        _ => false,
    }
}

fn evaluate_array_length_in(
    expression: &AstExpression,
    environment: &GenericEnvironment,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u64> {
    let value = match expression {
        AstExpression::Integer(literal) => u64::try_from(literal.value).ok(),
        AstExpression::Path(path) => {
            single_path_name(path).and_then(|name| environment.constants.get(name).copied())
        }
        AstExpression::Binary(expression) => {
            let left = evaluate_array_length_in(&expression.left, environment, diagnostics)?;
            let right = evaluate_array_length_in(&expression.right, environment, diagnostics)?;
            match expression.operator {
                AstBinaryOperator::Add => left.checked_add(right),
                AstBinaryOperator::Subtract => left.checked_sub(right),
                AstBinaryOperator::Multiply => left.checked_mul(right),
                AstBinaryOperator::Divide => left.checked_div(right),
                AstBinaryOperator::Remainder => left.checked_rem(right),
                AstBinaryOperator::BitAnd => Some(left & right),
                AstBinaryOperator::BitXor => Some(left ^ right),
                AstBinaryOperator::BitOr => Some(left | right),
                AstBinaryOperator::ShiftLeft => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shl(shift)),
                AstBinaryOperator::ShiftRight => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shr(shift)),
                AstBinaryOperator::Equal
                | AstBinaryOperator::NotEqual
                | AstBinaryOperator::Less
                | AstBinaryOperator::LessEqual
                | AstBinaryOperator::Greater
                | AstBinaryOperator::GreaterEqual
                | AstBinaryOperator::And
                | AstBinaryOperator::Or => None,
            }
        }
        _ => None,
    };
    if value.is_none() {
        diagnostics.push(
            Diagnostic::error(
                "E3011",
                "array length must be a non-negative integer constant",
                expression.span(),
            )
            .with_help("use an integer literal, const parameter, or checked const arithmetic"),
        );
    }
    value
}

fn compiletime_value_kind(value: &comptime::Value) -> &'static str {
    match value {
        comptime::Value::Integer(_) => "an integer",
        comptime::Value::Float(_) => "a floating-point value",
        comptime::Value::Boolean(_) => "a boolean",
        comptime::Value::Character(_) => "a character",
        comptime::Value::String(_) => "a string",
        comptime::Value::Unit => "the unit value",
        comptime::Value::Tuple(_) => "a tuple",
        comptime::Value::Array(_) => "an array",
        comptime::Value::Record(_) => "a record",
    }
}

fn report_compiletime_type_mismatch(
    expected: Type,
    actual: &str,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(
        Diagnostic::error(
            "E7011",
            format!("constant declared as `{expected}` produced {actual}"),
            span,
        )
        .with_help("change the declared type or return a compatible compile-time value"),
    );
}

fn invalid_composite_expression(span: Span) -> Expression {
    Expression {
        kind: ExpressionKind::Unit,
        ty: Type::Unit,
        span,
    }
}

fn scoped_storage_diagnostic(span: Span) -> Diagnostic {
    Diagnostic::error(
        "E3013",
        "a scoped view cannot be hidden inside raw owning storage",
        span,
    )
    .with_help("store owned data or keep the scoped value in a directly borrowed aggregate")
}

fn pattern_is_irrefutable(pattern: &hir::Pattern) -> bool {
    match &pattern.kind {
        hir::PatternKind::Wildcard | hir::PatternKind::Binding { .. } => true,
        hir::PatternKind::Tuple(elements) => elements.iter().all(pattern_is_irrefutable),
        hir::PatternKind::Integer(_)
        | hir::PatternKind::Float32(_)
        | hir::PatternKind::Float64(_)
        | hir::PatternKind::Character(_)
        | hir::PatternKind::Boolean(_)
        | hir::PatternKind::Enum { .. } => false,
    }
}

fn match_is_exhaustive(types: &TypeRegistry, ty: Type, arms: &[hir::MatchArm]) -> bool {
    let unguarded: Vec<_> = arms.iter().filter(|arm| arm.guard.is_none()).collect();
    if unguarded
        .iter()
        .any(|arm| pattern_is_irrefutable(&arm.pattern))
    {
        return true;
    }
    match ty {
        Type::Never => true,
        Type::Bool => {
            let mut seen_false = false;
            let mut seen_true = false;
            for arm in unguarded {
                match arm.pattern.kind {
                    hir::PatternKind::Boolean(false) => seen_false = true,
                    hir::PatternKind::Boolean(true) => seen_true = true,
                    _ => {}
                }
            }
            seen_false && seen_true
        }
        Type::Enum(_) => enum_match_is_exhaustive(types, ty, &unguarded),
        _ => false,
    }
}

fn enum_match_is_exhaustive(types: &TypeRegistry, ty: Type, arms: &[&hir::MatchArm]) -> bool {
    let Some(definition) = types.definition(ty) else {
        return false;
    };
    let hir::TypeDefinitionKind::Enum { variants } = &definition.kind else {
        return false;
    };
    let mut covered = vec![false; variants.len()];
    for arm in arms {
        let hir::PatternKind::Enum { variant, fields } = &arm.pattern.kind else {
            continue;
        };
        if fields.iter().all(pattern_is_irrefutable)
            && let Ok(index) = usize::try_from(*variant)
            && let Some(entry) = covered.get_mut(index)
        {
            *entry = true;
        }
    }
    covered.into_iter().all(|entry| entry)
}

fn integer_positive_maximum(ty: Type) -> u128 {
    debug_assert!(ty.is_integer());
    let Some(bits) = ty.integer_bits() else {
        return 0;
    };
    if ty.is_signed_integer() {
        if bits == 128 {
            i128::MAX as u128
        } else {
            (1_u128 << (bits - 1)) - 1
        }
    } else if bits == 128 {
        u128::MAX
    } else {
        (1_u128 << bits) - 1
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "contextual f32 literals deliberately round an already range-checked f64"
)]
fn narrow_f64_to_f32(value: f64) -> f32 {
    value as f32
}

fn integer_minimum_magnitude(ty: Type) -> u128 {
    debug_assert!(ty.is_signed_integer());
    let Some(bits) = ty.integer_bits() else {
        return 0;
    };
    1_u128 << (bits - 1)
}

fn is_equality_type(ty: Type) -> bool {
    ty.is_numeric() || matches!(ty, Type::Bool | Type::Char)
}

fn is_ordered_type(ty: Type) -> bool {
    ty.is_numeric() || ty == Type::Char
}

fn is_valid_cast(source: Type, target: Type) -> bool {
    (source == target && source.has_runtime_value())
        || (source.is_integer() && (target.is_integer() || target.is_float()))
        || (source.is_float()
            && (target.is_float() || target.integer_bits().is_some_and(|bits| bits <= 64)))
        || (source == Type::Char && target.is_integer())
        || (source.is_thin_pointer() && target.is_thin_pointer())
        || (source.is_thin_pointer() && target == Type::Usize)
        || (source == Type::Usize && matches!(target, Type::RawPointer(_) | Type::Function(_)))
}

fn is_builtin_trait(name: &str) -> bool {
    matches!(
        name,
        "Copy"
            | "Clone"
            | "Debug"
            | "Eq"
            | "Ord"
            | "Ordered"
            | "Hash"
            | "Default"
            | "Send"
            | "Sync"
            | "Pod"
    )
}

fn validate_comptime_function(
    function: &ast::Function,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for parameter in &function.parameters {
        validate_comptime_type(&parameter.ty, diagnostics);
    }
    if let Some(return_type) = &function.return_type {
        validate_comptime_type(return_type, diagnostics);
    }
    validate_comptime_block(&function.body, comptime_functions, diagnostics);
}

fn validate_comptime_type(type_name: &ast::TypeName, diagnostics: &mut Vec<Diagnostic>) {
    match &type_name.kind {
        TypeNameKind::Reference { .. }
        | TypeNameKind::RawPointer { .. }
        | TypeNameKind::Slice(_)
        | TypeNameKind::Function { .. } => diagnostics.push(
            Diagnostic::error(
                "E7012",
                "references, pointers, slices, and function values are forbidden in `comptime fn` signatures",
                type_name.span,
            )
            .with_help("pass deterministic owned scalar or aggregate values"),
        ),
        TypeNameKind::Generic { arguments, .. } => {
            for argument in arguments {
                match argument {
                    ast::GenericArgument::Type(type_name) => {
                        validate_comptime_type(type_name, diagnostics);
                    }
                    ast::GenericArgument::Pack { template, .. } => {
                        if let Some(template) = template {
                            validate_comptime_type(template, diagnostics);
                        }
                    }
                    ast::GenericArgument::Const(_) => {}
                }
            }
        }
        TypeNameKind::PackExpansion { template, .. } => {
            if let Some(template) = template {
                validate_comptime_type(template, diagnostics);
            }
        }
        TypeNameKind::Tuple(elements) => {
            for element in elements {
                validate_comptime_type(element, diagnostics);
            }
        }
        TypeNameKind::Array { element, .. } => validate_comptime_type(element, diagnostics),
        TypeNameKind::Path(_) | TypeNameKind::Unit => {}
    }
}

fn validate_comptime_block(
    block: &ast::Block,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for statement in &block.statements {
        match statement {
            AstStatement::Let(statement) => validate_comptime_expression(
                &statement.initializer,
                comptime_functions,
                diagnostics,
            ),
            AstStatement::Expression(statement) => {
                validate_comptime_expression(
                    &statement.expression,
                    comptime_functions,
                    diagnostics,
                );
            }
            AstStatement::Return(statement) => {
                if let Some(value) = &statement.value {
                    validate_comptime_expression(value, comptime_functions, diagnostics);
                }
            }
            AstStatement::While(statement) => {
                validate_comptime_expression(&statement.condition, comptime_functions, diagnostics);
                validate_comptime_block(&statement.body, comptime_functions, diagnostics);
            }
            AstStatement::For(statement) => {
                validate_comptime_expression(&statement.iterable, comptime_functions, diagnostics);
                validate_comptime_block(&statement.body, comptime_functions, diagnostics);
            }
            AstStatement::Break(statement) => {
                if let Some(value) = &statement.value {
                    validate_comptime_expression(value, comptime_functions, diagnostics);
                }
            }
            AstStatement::Defer(statement) => {
                diagnostics.push(
                    Diagnostic::error(
                        "E7012",
                        "`defer` is not available during compile-time evaluation",
                        statement.span,
                    )
                    .with_help("keep compile-time cleanup explicit and pure"),
                );
                validate_comptime_expression(&statement.action, comptime_functions, diagnostics);
            }
            AstStatement::Continue(_) => {}
        }
    }
    if let Some(tail) = &block.tail {
        validate_comptime_expression(tail, comptime_functions, diagnostics);
    }
}

fn validate_comptime_expression(
    expression: &AstExpression,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match expression {
        AstExpression::Integer(_)
        | AstExpression::Float(_)
        | AstExpression::Character(_)
        | AstExpression::String(_)
        | AstExpression::CString(_)
        | AstExpression::Boolean(_)
        | AstExpression::Unit(_)
        | AstExpression::Path(_) => {}
        AstExpression::FormattedString(formatted) => {
            validate_comptime_formatted_string(formatted, comptime_functions, diagnostics);
        }
        AstExpression::Tuple(tuple) => {
            for element in &tuple.elements {
                validate_comptime_expression(element, comptime_functions, diagnostics);
            }
        }
        AstExpression::PackExpansion(expansion) => {
            validate_comptime_expression(&expansion.template, comptime_functions, diagnostics);
        }
        AstExpression::Array(array) => {
            validate_comptime_array(array, comptime_functions, diagnostics);
        }
        AstExpression::Struct(structure) => {
            for field in &structure.fields {
                validate_comptime_expression(&field.value, comptime_functions, diagnostics);
            }
        }
        AstExpression::Unary(unary) => {
            validate_comptime_unary(unary, comptime_functions, diagnostics);
        }
        AstExpression::Binary(binary) => {
            validate_comptime_expression(&binary.left, comptime_functions, diagnostics);
            validate_comptime_expression(&binary.right, comptime_functions, diagnostics);
        }
        AstExpression::Call(call) => {
            validate_comptime_call(call, comptime_functions, diagnostics);
        }
        AstExpression::If(conditional) => {
            validate_comptime_expression(&conditional.condition, comptime_functions, diagnostics);
            validate_comptime_block(&conditional.then_branch, comptime_functions, diagnostics);
            if let Some(alternative) = &conditional.else_branch {
                validate_comptime_expression(alternative, comptime_functions, diagnostics);
            }
        }
        AstExpression::Match(matching) => {
            validate_comptime_expression(&matching.scrutinee, comptime_functions, diagnostics);
            for arm in &matching.arms {
                if let Some(guard) = &arm.guard {
                    validate_comptime_expression(guard, comptime_functions, diagnostics);
                }
                validate_comptime_expression(&arm.body, comptime_functions, diagnostics);
            }
        }
        AstExpression::Loop(looping) => {
            validate_comptime_block(&looping.body, comptime_functions, diagnostics);
        }
        AstExpression::Unsafe(block) => {
            diagnostics.push(
                Diagnostic::error(
                    "E7012",
                    "`unsafe` is forbidden during compile-time evaluation",
                    block.span,
                )
                .with_help("move native or raw-pointer work to runtime code"),
            );
            validate_comptime_block(block, comptime_functions, diagnostics);
        }
        AstExpression::Block(block) => {
            validate_comptime_block(block, comptime_functions, diagnostics);
        }
        AstExpression::Assignment(assignment) => {
            validate_comptime_expression(&assignment.target, comptime_functions, diagnostics);
            validate_comptime_expression(&assignment.value, comptime_functions, diagnostics);
        }
        AstExpression::Cast(cast) => {
            validate_comptime_expression(&cast.value, comptime_functions, diagnostics);
            validate_comptime_type(&cast.target, diagnostics);
        }
        AstExpression::Field(field) => {
            validate_comptime_expression(&field.base, comptime_functions, diagnostics);
        }
        AstExpression::Index(index) => {
            validate_comptime_expression(&index.base, comptime_functions, diagnostics);
            for value in &index.indices {
                validate_comptime_expression(value, comptime_functions, diagnostics);
            }
        }
        AstExpression::Try { value, span } => {
            diagnostics.push(
                Diagnostic::error(
                    "E7012",
                    "`?` is not available during compile-time evaluation",
                    *span,
                )
                .with_help("match the value explicitly before entering `comptime`"),
            );
            validate_comptime_expression(value, comptime_functions, diagnostics);
        }
    }
}

fn validate_comptime_array(
    array: &ast::ArrayExpression,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match &array.kind {
        ast::ArrayExpressionKind::List(elements) => {
            for element in elements {
                validate_comptime_expression(element, comptime_functions, diagnostics);
            }
        }
        ast::ArrayExpressionKind::Repeat { value, length } => {
            validate_comptime_expression(value, comptime_functions, diagnostics);
            validate_comptime_expression(length, comptime_functions, diagnostics);
        }
    }
}

fn validate_comptime_formatted_string(
    formatted: &ast::FormattedStringExpression,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(
        Diagnostic::error(
            "E7012",
            "formatted strings are not available during compile-time evaluation",
            formatted.span,
        )
        .with_help("format runtime text into an allocator-owned String"),
    );
    for fragment in &formatted.fragments {
        if let ast::FormattedStringFragment::Display(expression)
        | ast::FormattedStringFragment::Debug(expression) = fragment
        {
            validate_comptime_expression(expression, comptime_functions, diagnostics);
        }
    }
}

fn validate_comptime_unary(
    unary: &ast::UnaryExpression,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if matches!(
        unary.operator,
        AstUnaryOperator::Borrow | AstUnaryOperator::BorrowMut | AstUnaryOperator::Dereference
    ) {
        diagnostics.push(
            Diagnostic::error(
                "E7012",
                "references and pointer dereferences are forbidden during compile-time evaluation",
                unary.span,
            )
            .with_help("use owned compile-time values"),
        );
    }
    validate_comptime_expression(&unary.operand, comptime_functions, diagnostics);
}

fn validate_comptime_call(
    call: &ast::CallExpression,
    comptime_functions: &BTreeSet<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let allowed = match &call.callee {
        AstExpression::Path(path) => {
            single_path_name(path).is_some_and(|name| {
                comptime_functions.contains(name)
                    || matches!(
                        name,
                        "assert" | "panic" | "size_of" | "align_of" | "fields" | "variants"
                    )
            }) || matches!(
                path.segments.as_slice(),
                [namespace, name]
                    if namespace.name == "meta"
                        && matches!(
                            name.name.as_str(),
                            "name" | "fields" | "variants" | "traits"
                        )
            )
        }
        _ => false,
    };
    if !allowed {
        diagnostics.push(
            Diagnostic::error(
                "E7012",
                "only `comptime fn` and compile-time metadata intrinsics may be called here",
                call.span,
            )
            .with_help(
                "move I/O, allocation, FFI, clocks, randomness, and threads to runtime code",
            ),
        );
    }
    for argument in &call.generic_arguments {
        if let ast::GenericArgument::Const(value) = argument {
            validate_comptime_expression(value, comptime_functions, diagnostics);
        }
    }
    for argument in &call.arguments {
        validate_comptime_expression(argument, comptime_functions, diagnostics);
    }
}

fn has_identifier_attribute(attributes: &[ast::Attribute], name: &str, argument: &str) -> bool {
    attributes.iter().any(|attribute| {
        attribute.name.name == name
            && matches!(
                attribute.arguments.as_slice(),
                [ast::AttributeArgument::Identifier(value)] if value.name == argument
            )
    })
}

fn has_marker_attribute(attributes: &[ast::Attribute], name: &str) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.name.name == name && attribute.arguments.is_empty())
}

fn requested_alignment(attributes: &[ast::Attribute]) -> Option<u32> {
    attributes.iter().find_map(|attribute| {
        if attribute.name.name != "align" {
            return None;
        }
        let [ast::AttributeArgument::Integer(value)] = attribute.arguments.as_slice() else {
            return None;
        };
        u32::try_from(value.value).ok()
    })
}

fn derived_traits(attributes: &[ast::Attribute]) -> Vec<hir::DerivedTrait> {
    let mut requested = Vec::new();
    for attribute in attributes {
        if attribute.name.name != "derive" {
            continue;
        }
        for argument in &attribute.arguments {
            let ast::AttributeArgument::Identifier(name) = argument else {
                continue;
            };
            if let Some(derived) = derived_trait(&name.name)
                && !requested.contains(&derived)
            {
                requested.push(derived);
            }
        }
    }
    if requested.contains(&hir::DerivedTrait::Copy)
        && !requested.contains(&hir::DerivedTrait::Clone)
    {
        requested.push(hir::DerivedTrait::Clone);
    }
    if requested.contains(&hir::DerivedTrait::Pod) && !requested.contains(&hir::DerivedTrait::Copy)
    {
        requested.push(hir::DerivedTrait::Copy);
        if !requested.contains(&hir::DerivedTrait::Clone) {
            requested.push(hir::DerivedTrait::Clone);
        }
    }
    requested
}

fn derived_marker_traits(attributes: &[ast::Attribute]) -> Vec<String> {
    let mut requested = Vec::new();
    for attribute in attributes {
        if attribute.name.name != "derive" {
            continue;
        }
        for argument in &attribute.arguments {
            let ast::AttributeArgument::Identifier(name) = argument else {
                continue;
            };
            if derived_trait(&name.name).is_none() && !requested.contains(&name.name) {
                requested.push(name.name.clone());
            }
        }
    }
    requested
}

fn derived_trait(name: &str) -> Option<hir::DerivedTrait> {
    match name {
        "Copy" => Some(hir::DerivedTrait::Copy),
        "Clone" => Some(hir::DerivedTrait::Clone),
        "Debug" => Some(hir::DerivedTrait::Debug),
        "Eq" => Some(hir::DerivedTrait::Eq),
        "Hash" => Some(hir::DerivedTrait::Hash),
        "Default" => Some(hir::DerivedTrait::Default),
        "Pod" => Some(hir::DerivedTrait::Pod),
        _ => None,
    }
}

fn function_attributes(function: &ast::Function) -> hir::FunctionAttributes {
    hir::FunctionAttributes {
        inline: has_marker_attribute(&function.attributes, "inline"),
        must_use: has_marker_attribute(&function.attributes, "must_use"),
        test: has_marker_attribute(&function.attributes, "test"),
    }
}

fn validate_function_attributes(function: &ast::Function, diagnostics: &mut Vec<Diagnostic>) {
    validate_attribute_names(
        &function.attributes,
        &["inline", "test", "must_use"],
        diagnostics,
    );
    for attribute in &function.attributes {
        if matches!(attribute.name.name.as_str(), "inline" | "test" | "must_use")
            && !attribute.arguments.is_empty()
        {
            diagnostics.push(
                Diagnostic::error(
                    "E7001",
                    format!("`@{}` does not accept arguments", attribute.name.name),
                    attribute.span,
                )
                .with_help(format!("write `@{}`", attribute.name.name)),
            );
        }
    }
    if has_marker_attribute(&function.attributes, "test") {
        if function.is_comptime {
            diagnostics.push(
                Diagnostic::error(
                    "E7002",
                    "`@test` cannot be combined with `comptime fn`",
                    function.span,
                )
                .with_help("make the function a runtime test or remove `@test`"),
            );
        }
        if !function.generic_parameters.is_empty() || !function.parameters.is_empty() {
            diagnostics.push(
                Diagnostic::error(
                    "E7002",
                    "`@test` functions cannot have generic or value parameters",
                    function.span,
                )
                .with_help("move test data into the function body"),
            );
        }
        if function
            .return_type
            .as_ref()
            .is_some_and(|ty| !matches!(ty.kind, TypeNameKind::Unit))
        {
            diagnostics.push(
                Diagnostic::error(
                    "E7002",
                    "`@test` functions must return `()`",
                    function
                        .return_type
                        .as_ref()
                        .map_or(function.span, |ty| ty.span),
                )
                .with_help("remove the return annotation or write `-> ()`"),
            );
        }
    }
    if function.is_comptime && has_marker_attribute(&function.attributes, "inline") {
        diagnostics.push(
            Diagnostic::error(
                "E7002",
                "`@inline` has no meaning on a `comptime fn`",
                function.span,
            )
            .with_help("remove `@inline` from the compile-time function"),
        );
    }
}

fn validate_type_attributes(
    attributes: &[ast::Attribute],
    is_enum: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    validate_attribute_names(
        attributes,
        &["repr", "derive", "align", "must_use"],
        diagnostics,
    );
    for attribute in attributes {
        match attribute.name.name.as_str() {
            "repr" => {
                if is_enum {
                    diagnostics.push(
                        Diagnostic::error(
                            "E7003",
                            "`@repr(C)` is currently valid only on structs",
                            attribute.span,
                        )
                        .with_help(
                            "wrap the enum in a C-representation struct at the FFI boundary",
                        ),
                    );
                }
                if !matches!(
                    attribute.arguments.as_slice(),
                    [ast::AttributeArgument::Identifier(value)] if value.name == "C"
                ) {
                    diagnostics.push(
                        Diagnostic::error(
                            "E7003",
                            "`@repr` expects the single representation `C`",
                            attribute.span,
                        )
                        .with_help("write `@repr(C)`"),
                    );
                }
            }
            "derive" => validate_derive_attribute(attribute, diagnostics),
            "align" => validate_align_attribute(attribute, diagnostics),
            "must_use" if !attribute.arguments.is_empty() => diagnostics.push(
                Diagnostic::error(
                    "E7001",
                    "`@must_use` does not accept arguments",
                    attribute.span,
                )
                .with_help("write `@must_use`"),
            ),
            _ => {}
        }
    }
}

fn validate_attribute_names(
    attributes: &[ast::Attribute],
    allowed: &[&str],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen = HashMap::new();
    for attribute in attributes {
        if !allowed.contains(&attribute.name.name.as_str()) {
            diagnostics.push(
                Diagnostic::error(
                    "E7001",
                    format!("unknown attribute `@{}`", attribute.name.name),
                    attribute.span,
                )
                .with_help(format!("supported attributes here: {}", allowed.join(", "))),
            );
        }
        if seen
            .insert(attribute.name.name.as_str(), attribute.span)
            .is_some()
        {
            diagnostics.push(
                Diagnostic::error(
                    "E7001",
                    format!("attribute `@{}` is repeated", attribute.name.name),
                    attribute.span,
                )
                .with_help("keep one occurrence of each attribute"),
            );
        }
    }
}

fn validate_derive_attribute(attribute: &ast::Attribute, diagnostics: &mut Vec<Diagnostic>) {
    if attribute.arguments.is_empty() {
        diagnostics.push(
            Diagnostic::error(
                "E7004",
                "`@derive` needs at least one trait",
                attribute.span,
            )
            .with_help(
                "choose a built-in structural derive or an imported zero-method marker trait",
            ),
        );
        return;
    }
    let mut seen = HashMap::new();
    for argument in &attribute.arguments {
        let ast::AttributeArgument::Identifier(name) = argument else {
            diagnostics.push(
                Diagnostic::error("E7004", "derive names must be identifiers", argument.span())
                    .with_help(
                        "choose a built-in structural derive or an imported zero-method marker trait",
                    ),
            );
            continue;
        };
        if seen.insert(name.name.as_str(), name.span).is_some() {
            diagnostics.push(
                Diagnostic::error(
                    "E7004",
                    format!("derive `{}` is repeated", name.name),
                    name.span,
                )
                .with_help("list each derive once"),
            );
        }
    }
}

fn validate_align_attribute(attribute: &ast::Attribute, diagnostics: &mut Vec<Diagnostic>) {
    let [ast::AttributeArgument::Integer(value)] = attribute.arguments.as_slice() else {
        diagnostics.push(
            Diagnostic::error(
                "E7005",
                "`@align` expects exactly one integer",
                attribute.span,
            )
            .with_help("write a power of two such as `@align(16)`"),
        );
        return;
    };
    let Ok(alignment) = u32::try_from(value.value) else {
        diagnostics.push(
            Diagnostic::error("E7005", "requested alignment is too large", value.span)
                .with_help("use a power of two no larger than 536870912"),
        );
        return;
    };
    if !alignment.is_power_of_two() || alignment > 536_870_912 {
        diagnostics.push(
            Diagnostic::error(
                "E7005",
                "alignment must be a power of two no larger than 536870912",
                value.span,
            )
            .with_help("use 1, 2, 4, 8, 16, or another supported power of two"),
        );
    }
}

fn receiver_shapes_match(required: Option<&TypeNameKind>, provided: Option<&TypeNameKind>) -> bool {
    match (required, provided) {
        (
            Some(TypeNameKind::Reference {
                mutable: required, ..
            }),
            Some(TypeNameKind::Reference {
                mutable: provided, ..
            }),
        ) => required == provided,
        (Some(TypeNameKind::Reference { .. }), _)
        | (_, Some(TypeNameKind::Reference { .. }))
        | (None, Some(_))
        | (Some(_), None) => false,
        (None, None) | (Some(_), Some(_)) => true,
    }
}

fn type_pattern_key(type_name: &ast::TypeName) -> String {
    match &type_name.kind {
        TypeNameKind::Function {
            parameters,
            return_type,
        } => format!(
            "fn({})->{}",
            parameters
                .iter()
                .map(type_pattern_key)
                .collect::<Vec<_>>()
                .join(","),
            type_pattern_key(return_type)
        ),
        TypeNameKind::Path(path) => path.display(),
        TypeNameKind::Generic { path, arguments } => {
            let arguments = arguments
                .iter()
                .map(|argument| match argument {
                    ast::GenericArgument::Type(ty) => type_pattern_key(ty),
                    ast::GenericArgument::Const(value) => const_expression_key(value),
                    ast::GenericArgument::Pack { pack, template, .. } => {
                        template.as_ref().map_or_else(
                            || format!("...{}", pack.name),
                            |template| format!("...{}=>{}", pack.name, type_pattern_key(template)),
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{}<{arguments}>", path.display())
        }
        TypeNameKind::Unit => "()".to_owned(),
        TypeNameKind::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(type_pattern_key)
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeNameKind::Array { element, length } => {
            format!(
                "[{};{}]",
                type_pattern_key(element),
                const_expression_key(length)
            )
        }
        TypeNameKind::Slice(element) => format!("[{}]", type_pattern_key(element)),
        TypeNameKind::Reference { mutable, target } => format!(
            "&{}{}",
            if *mutable { "mut " } else { "" },
            type_pattern_key(target)
        ),
        TypeNameKind::RawPointer { mutable, target } => format!(
            "*{} {}",
            if *mutable { "mut" } else { "const" },
            type_pattern_key(target)
        ),
        TypeNameKind::PackExpansion { pack, template } => template.as_ref().map_or_else(
            || format!("...{}", pack.name),
            |template| format!("...{}=>{}", pack.name, type_pattern_key(template)),
        ),
    }
}

fn const_expression_key(expression: &AstExpression) -> String {
    match expression {
        AstExpression::Integer(literal) => literal.value.to_string(),
        AstExpression::Path(path) => path.display(),
        AstExpression::Unary(expression) => format!(
            "{:?}{}",
            expression.operator,
            const_expression_key(&expression.operand)
        ),
        AstExpression::Binary(expression) => format!(
            "({} {:?} {})",
            const_expression_key(&expression.left),
            expression.operator,
            const_expression_key(&expression.right)
        ),
        _ => "<expression>".to_owned(),
    }
}

fn single_path_name(path: &ast::Path) -> Option<&str> {
    let [name] = path.segments.as_slice() else {
        return None;
    };
    Some(&name.name)
}

fn assertion_mode(path: &ast::Path) -> Option<AssertionMode> {
    match single_path_name(path)? {
        "assert" => Some(AssertionMode::Always),
        "debug_assert" => Some(AssertionMode::Debug),
        _ => None,
    }
}

fn function_path_name(path: &ast::Path) -> Option<String> {
    match path.segments.as_slice() {
        [name] => Some(name.name.clone()),
        [owner, method] => Some(format!("{}::{}", owner.name, method.name)),
        _ => None,
    }
}

const fn string_format_method(ty: Type) -> Option<&'static str> {
    match ty {
        Type::Str => Some("push_str"),
        Type::Bool => Some("push_bool"),
        Type::Char => Some("push_char"),
        Type::I8 => Some("push_i8"),
        Type::I16 => Some("push_i16"),
        Type::I32 => Some("push_i32"),
        Type::I64 => Some("push_i64"),
        Type::I128 => Some("push_i128"),
        Type::Isize => Some("push_isize"),
        Type::U8 => Some("push_u8"),
        Type::U16 => Some("push_u16"),
        Type::U32 => Some("push_u32"),
        Type::U64 => Some("push_u64"),
        Type::U128 => Some("push_u128"),
        Type::Usize => Some("push_usize"),
        Type::F32 => Some("push_f32"),
        Type::F64 => Some("push_f64"),
        Type::Unit
        | Type::Never
        | Type::CStr
        | Type::Struct(_)
        | Type::Enum(_)
        | Type::Tuple(_)
        | Type::Array(_)
        | Type::Reference(_)
        | Type::RawPointer(_)
        | Type::Slice(_)
        | Type::Function(_) => None,
    }
}

fn type_constructor_name(type_name: &ast::TypeName) -> Option<&str> {
    match &type_name.kind {
        TypeNameKind::Path(path) | TypeNameKind::Generic { path, .. } => single_path_name(path),
        TypeNameKind::Function { .. }
        | TypeNameKind::Unit
        | TypeNameKind::Tuple(_)
        | TypeNameKind::Array { .. }
        | TypeNameKind::Slice(_)
        | TypeNameKind::Reference { .. }
        | TypeNameKind::RawPointer { .. }
        | TypeNameKind::PackExpansion { .. } => None,
    }
}

fn symbol_module_identity(name: &str) -> Option<&str> {
    let (module, _) = name.strip_prefix("__module_")?.split_once('$')?;
    Some(module)
}

fn assignment_root_path(expression: &AstExpression) -> Option<&ast::Path> {
    match expression {
        AstExpression::Path(path) => Some(path),
        AstExpression::Field(field) => assignment_root_path(&field.base),
        AstExpression::Index(index) => assignment_root_path(&index.base),
        AstExpression::Call(call) => match &call.callee {
            AstExpression::Field(field) => assignment_root_path(&field.base),
            _ => None,
        },
        _ => None,
    }
}

fn scoped_return_root(expression: &AstExpression) -> Option<&ast::Path> {
    match expression {
        AstExpression::Unary(unary)
            if matches!(
                unary.operator,
                AstUnaryOperator::Borrow | AstUnaryOperator::BorrowMut
            ) =>
        {
            view_source_root_path(&unary.operand)
        }
        AstExpression::Path(path) => Some(path),
        AstExpression::Field(field) => view_source_root_path(&field.base),
        AstExpression::Index(index) => view_source_root_path(&index.base),
        AstExpression::Call(call) => match &call.callee {
            AstExpression::Field(field) => view_source_root_path(&field.base),
            AstExpression::Path(_) => call.arguments.first().and_then(scoped_return_root),
            _ => None,
        },
        AstExpression::Cast(cast) => scoped_return_root(&cast.value),
        AstExpression::Unsafe(block) | AstExpression::Block(block) => {
            block.tail.as_deref().and_then(scoped_return_root)
        }
        AstExpression::Try { value, .. } => scoped_return_root(value),
        AstExpression::Match(matching) => scoped_return_root(&matching.scrutinee),
        AstExpression::Struct(structure) => structure
            .fields
            .iter()
            .find_map(|field| scoped_return_root(&field.value)),
        AstExpression::Tuple(tuple) => tuple.elements.iter().find_map(scoped_return_root),
        AstExpression::Array(array) => match &array.kind {
            ast::ArrayExpressionKind::List(elements) => {
                elements.iter().find_map(scoped_return_root)
            }
            ast::ArrayExpressionKind::Repeat { value, length } => {
                scoped_return_root(value).or_else(|| scoped_return_root(length))
            }
        },
        _ => None,
    }
}

fn initializer_stores_borrow(expression: &AstExpression) -> bool {
    match expression {
        AstExpression::Unary(unary) => matches!(
            unary.operator,
            AstUnaryOperator::Borrow | AstUnaryOperator::BorrowMut
        ),
        AstExpression::Struct(structure) => structure
            .fields
            .iter()
            .any(|field| initializer_stores_borrow(&field.value)),
        AstExpression::Tuple(tuple) => tuple.elements.iter().any(initializer_stores_borrow),
        AstExpression::PackExpansion(expansion) => initializer_stores_borrow(&expansion.template),
        AstExpression::Array(array) => match &array.kind {
            ast::ArrayExpressionKind::List(elements) => {
                elements.iter().any(initializer_stores_borrow)
            }
            ast::ArrayExpressionKind::Repeat { value, length } => {
                initializer_stores_borrow(value) || initializer_stores_borrow(length)
            }
        },
        _ => false,
    }
}

fn view_source_root_path(expression: &AstExpression) -> Option<&ast::Path> {
    match expression {
        AstExpression::Path(path) => Some(path),
        AstExpression::Field(field) => view_source_root_path(&field.base),
        AstExpression::Index(index) => view_source_root_path(&index.base),
        AstExpression::Call(call) => match &call.callee {
            AstExpression::Field(field) => view_source_root_path(&field.base),
            _ => None,
        },
        AstExpression::Cast(cast) => view_source_root_path(&cast.value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use reimer_diagnostics::Span;
    use reimer_hir::{TypeDefinition, TypeDefinitionKind, TypeField, TypeRepresentation};
    use reimer_lexer::lex;
    use reimer_parser::parse;
    use reimer_types::{Type, TypeId};

    use super::{STANDARD_STRING_TYPE, TypeRegistry, resolve, resolve_library};

    fn resolve_fixture(
        source: &str,
    ) -> Result<reimer_hir::Program, Vec<reimer_diagnostics::Diagnostic>> {
        let tokens = lex(source).expect("fixture should lex");
        let program = parse(&tokens).expect("fixture should parse");
        resolve(&program)
    }

    #[test]
    fn resolve_should_type_check_functions_bindings_and_control_flow() {
        let source = "fn add(left: i32, right: i32) -> i32 { left + right }
            fn main() -> i32 {
                let mut value = add(1, 2);
                while value < 5 { value += 1; }
                if value == 5 { 42 } else { 0 }
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions[1].body.ty, Type::I32);
    }

    #[test]
    fn resolve_library_should_not_require_main() {
        let tokens = lex("pub fn answer() -> i32 { 42 }").expect("fixture should lex");
        let syntax = parse(&tokens).expect("fixture should parse");

        let program = resolve_library(&syntax).expect("library should resolve");

        assert!(program.entry.is_none());
    }

    #[test]
    fn resolve_should_reject_assignment_to_immutable_binding() {
        let source = "fn main() -> i32 { let value = 1; value = 2; return value; }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3107")
        );
    }

    #[test]
    fn resolve_should_collect_typed_static_initializers() {
        let source = "static ANSWER: i32 = 40 + 2;
            static mut COUNTER: i32 = 0;
            fn main() -> i32 {
                unsafe { COUNTER = ANSWER; COUNTER }
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.statics.len(), 2);
        assert!(!program.statics[0].mutable);
        assert!(program.statics[1].mutable);
        assert!(matches!(
            program.statics[0].initializer.kind,
            reimer_hir::ExpressionKind::Integer(42)
        ));
    }

    #[test]
    fn resolve_should_allow_references_rooted_in_immutable_statics_to_escape() {
        let source = "static ANSWER: i32 = 42; fn answer_address() -> &i32 { &ANSWER } fn main() -> i32 { *answer_address() }";

        resolve_fixture(source).expect("stable static reference should resolve");
    }

    #[test]
    fn resolve_should_require_unsafe_for_mutable_static_access() {
        let source = "static mut COUNTER: i32 = 0; fn main() -> i32 { COUNTER = 1; COUNTER }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3156")
        );
    }

    #[test]
    fn resolve_should_reject_assignment_to_immutable_static() {
        let source = "static ANSWER: i32 = 42; fn main() -> i32 { ANSWER = 0; ANSWER }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3107")
        );
    }

    #[test]
    fn resolve_should_reject_scoped_static_storage() {
        let source = "static MESSAGE: str = \"hello\"; fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3155")
        );
    }

    #[test]
    fn resolve_should_reject_moving_non_copy_values_from_statics() {
        let source = "struct Holder { value: i32 }
            static VALUE: Holder = Holder { value: 42 };
            fn main() -> Holder { VALUE }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3144")
        );
    }

    #[test]
    fn resolve_should_reject_wrong_function_argument_type() {
        let source = "fn identity(value: i32) -> i32 { value } fn main() -> i32 { identity(true) }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3103")
        );
    }

    #[test]
    fn resolve_should_accept_unit_function_without_return_annotation() {
        let source = "fn visit() { let done = true; } fn main() -> i32 { visit(); 0 }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions[0].return_type, Type::Unit);
    }

    #[test]
    fn resolve_should_reject_qualified_call_until_module_resolution() {
        let source = "fn main() -> i32 { x::y::z(); 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3106")
        );
    }

    #[test]
    fn resolve_should_accept_minimum_i32_literal() {
        let program =
            resolve_fixture("fn main() -> i32 { -2147483648 }").expect("fixture should resolve");

        let tail = program.functions[0]
            .body
            .tail
            .as_ref()
            .expect("fixture has a tail expression");

        assert!(matches!(
            tail.kind,
            reimer_hir::ExpressionKind::Integer(value) if value == 1_u128 << 31
        ));
    }

    #[test]
    fn resolve_should_respect_local_shadowing_at_call_sites() {
        let source = "fn answer() -> i32 { 42 }
            fn main() -> i32 { let answer = 1; answer(); 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("not callable"))
        );
    }

    #[test]
    fn resolve_should_contextually_type_scalar_literals_and_casts() {
        let source = "fn widen(value: u8) -> u64 { value as u64 }
            fn main() -> i32 {
                let byte: u8 = 21;
                let signed: i128 = -170141183460469231731687303715884105728;
                let ratio: f32 = 1.5;
                let scalar: char = 'A';
                if (widen(byte) << 1) == 42 && ratio > 1.0 && scalar as u32 == 65 {
                    signed as i32
                } else {
                    0
                }
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions[0].parameters[0].ty, Type::U8);
        assert_eq!(program.functions[0].return_type, Type::U64);
    }

    #[test]
    fn resolve_should_type_integer_addition_modes_for_every_integer_type() {
        let mut source = String::new();
        for ty in [
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        ] {
            write!(
                source,
                "fn exercise_{ty}(value: {ty}) {{
                    value.wrapping_add(1);
                    value.checked_add(1);
                    value.saturating_add(1);
                }}"
            )
            .expect("writing a String should not fail");
        }
        source.push_str("fn main() -> i32 { 0 }");

        let program = resolve_fixture(&source).expect("integer methods should resolve");

        assert_eq!(program.functions.len(), 13);
    }

    #[test]
    fn resolve_should_reject_integer_addition_methods_on_other_types() {
        let diagnostics = resolve_fixture("fn main() -> i32 { true.checked_add(1); 0 }")
            .expect_err("boolean receiver should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E6002" && diagnostic.message.contains("non-integer")
        }));
    }

    #[test]
    fn resolve_should_reject_wrong_integer_addition_method_arity() {
        let diagnostics =
            resolve_fixture("fn main() -> i32 { let value: u8 = 1; value.wrapping_add(); 0 }")
                .expect_err("missing argument should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E3105" && diagnostic.message.contains("wrapping_add")
        }));
    }

    #[test]
    fn resolve_should_type_recoverable_slice_access() {
        let source = "
            fn read(values: &[i32], index: usize) -> Option<&i32> {
                values.get(index)
            }
            fn update(values: &mut [i32], index: usize) -> Option<&mut i32> {
                values.get_mut(index)
            }
            fn main() -> i32 {
                let mut values: [i32; 2] = [20, 22];
                {
                    let slice: &mut [i32] = &mut values;
                    match update(slice, 1) {
                        Some(value) => *value,
                        None => 0,
                    };
                }
                let slice: &[i32] = &values;
                match read(slice, 0) {
                    Some(value) => *value,
                    None => 0,
                }
            }";

        resolve_fixture(source).expect("recoverable slice access should resolve");
    }

    #[test]
    fn resolve_should_reject_get_mut_on_an_immutable_slice() {
        let source = "
            fn invalid(values: &[i32]) -> Option<&mut i32> {
                values.get_mut(0)
            }
            fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("immutable slice should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E3107" && diagnostic.message.contains("mutable slice")
        }));
    }

    #[test]
    fn resolve_should_reject_wrong_slice_access_arity() {
        let source = "
            fn invalid(values: &[i32]) {
                values.get();
            }
            fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("missing slice index should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E3105" && diagnostic.message.contains("slice method `get`")
        }));
    }

    #[test]
    fn resolve_should_type_utf8_byte_views_and_character_iteration() {
        let source = "
            fn encoded(text: str) -> &[u8] {
                text.bytes()
            }
            fn main() -> i32 {
                let text: str = \"Aé🦀\";
                let bytes = encoded(text);
                let mut characters = text.chars();
                let first = match characters.next() {
                    Some(value) => value,
                    None => 'x',
                };
                let second = match characters.next() {
                    Some(value) => value,
                    None => 'x',
                };
                let mut count: usize = 0;
                for _ in text.chars() {
                    count += 1;
                }
                if bytes[0] == 65 && first == 'A' && second == 'é' && count == 3 {
                    42
                } else {
                    0
                }
            }";

        resolve_fixture(source).expect("UTF-8 iteration should resolve");
    }

    #[test]
    fn resolve_should_require_a_mutable_character_iterator() {
        let source = "
            fn main() -> i32 {
                let text: str = \"A\";
                let characters = text.chars();
                characters.next();
                0
            }";

        let diagnostics = resolve_fixture(source).expect_err("immutable iterator should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E3107" && diagnostic.message.contains("characters")
        }));
    }

    #[test]
    fn resolve_should_reject_out_of_range_contextual_integer() {
        let diagnostics = resolve_fixture("fn main() -> i32 { let byte: u8 = 256; 0 }")
            .expect_err("fixture should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E3104" && diagnostic.message.contains("`u8`")
        }));
    }

    #[test]
    fn resolve_should_reject_implicit_numeric_conversion() {
        let source = "fn main() -> i32 { let small: u8 = 1; let wide: u64 = small; 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3103")
        );
    }

    #[test]
    fn resolve_should_accept_explicit_float_to_integer_casts() {
        resolve_fixture(
            "fn main() -> i32 {
                let byte: u8 = 255.9 as u8;
                let signed: i32 = -1.9 as i32;
                byte as i32 + signed
            }",
        )
        .expect("explicit saturating float-to-integer casts should resolve");
    }

    #[test]
    fn resolve_should_type_structs_tuples_arrays_fields_and_indices() {
        let source = "struct Pair { left: i32, right: i32 }
            fn sum(pair: Pair) -> i32 { pair.left + pair.right }
            fn main() -> i32 {
                let pair = Pair { right: 22, left: 20 };
                let values: [i32; 2] = [sum(pair), 0];
                let result: (i32, bool) = (values[0], true);
                result.0
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.types.len(), 3);
    }

    #[test]
    fn resolve_should_type_repeated_array_initializers() {
        let source = "fn filled<T: Copy, const N: usize>(value: T) -> [T; N] {
            [value; N]
        }
        fn main() -> i32 {
            let values: [i32; 4] = filled<i32, 4>(7);
            values[0] + values[3]
        }";

        let program = resolve_fixture(source).expect("repeated array should resolve");
        assert!(program.functions.iter().any(|function| {
            matches!(
                function
                    .body
                    .tail
                    .as_deref()
                    .map(|expression| &expression.kind),
                Some(reimer_hir::ExpressionKind::ArrayRepeat { length: 4, .. })
            )
        }));
    }

    #[test]
    fn resolve_should_require_copy_for_repeated_array_elements() {
        let source = "struct Owner { value: i32 }
            fn main() -> i32 {
                let values: [Owner; 2] = [Owner { value: 21 }; 2];
                values[0].value
            }";

        let diagnostics = resolve_fixture(source).expect_err("non-Copy repeat should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3164")
        );
    }

    #[test]
    fn resolve_should_monomorphize_generic_structs_and_const_lengths() {
        let source = "
            struct Buffer<T, const N: usize> { values: [T; N] }
            fn main() -> i32 {
                let buffer: Buffer<i32, 2> = Buffer { values: [20, 22] };
                buffer.values[0] + buffer.values[1]
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert!(program.types.iter().any(|definition| {
            definition
                .name
                .as_deref()
                .is_some_and(|name| name == "Buffer<i32, 2>")
        }));
    }

    #[test]
    fn resolve_should_monomorphize_generic_functions_by_inference() {
        let source = "
            fn identity<T>(value: T) -> T { value }
            fn first<T, const N: usize>(values: [T; N]) -> T { values[0] }
            fn main() -> i32 { identity(first([42, 7])) }
        ";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions.len(), 3);
        assert!(
            program
                .functions
                .iter()
                .filter(|function| function.name.contains("$instance$"))
                .count()
                == 2
        );
    }

    #[test]
    fn resolve_should_expand_variadic_type_packs_and_mapped_tuple_types() {
        let source = "
            struct Slot<T> { value: T }
            struct Registry<...Types> { slots: (...Types => Slot<Types>) }
            fn forward<...Types>(values: (...Types)) -> (...Types) { values }
            fn main() -> i32 {
                let registry: Registry<i32, bool> = Registry {
                    slots: (Slot { value: 42 }, Slot { value: true }),
                };
                let values: (i32, bool) = forward((registry.slots.0.value, true));
                values.0
            }
        ";

        let program = resolve_fixture(source).expect("variadic fixture should resolve");

        assert!(program.types.iter().any(|definition| {
            definition
                .name
                .as_deref()
                .is_some_and(|name| name == "Registry<i32, bool>")
        }));
    }

    #[test]
    fn resolve_should_expand_variadic_value_templates() {
        let source = "
            struct Marker<T> { value: i32 }
            fn make_marker<T>() -> Marker<T> { Marker { value: 21 } }
            fn markers<...Types>() -> (...Types => Marker<Types>) {
                (...Types => make_marker<Types>(),)
            }
            fn main() -> i32 {
                let values: (Marker<i32>, Marker<bool>) = markers<i32, bool>();
                values.0.value + values.1.value
            }
        ";

        resolve_fixture(source).expect("value pack fixture should resolve");
    }

    #[test]
    fn resolve_should_reject_non_final_type_packs() {
        let diagnostics = resolve_fixture(
            "struct Invalid<...Types, Tail> { value: Tail }
             fn main() -> i32 { 0 }",
        )
        .expect_err("non-final pack should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E6020" && diagnostic.message.contains("final")
        }));
    }

    #[test]
    fn resolve_should_reject_ambiguous_type_addressable_tuples() {
        let diagnostics = resolve_fixture(
            "fn main() -> i32 {
                 let values: (i32, i32) = (20, 22);
                 values.assert_unique_types();
                 0
             }",
        )
        .expect_err("duplicate tuple types should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E6022" && diagnostic.message.contains("more than once")
        }));
    }

    #[test]
    fn resolve_should_monomorphize_generic_inherent_methods() {
        let source = "
            struct Holder<T> { value: T }
            impl<T> Holder<T> {
                fn new(value: T) -> Holder<T> { Holder { value: value } }
                fn get(&self) -> T { self.value }
            }
            fn main() -> i32 {
                let holder: Holder<i32> = Holder::new(42);
                holder.get()
            }
        ";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions.len(), 3);
        assert!(
            program
                .functions
                .iter()
                .any(|function| function.name.starts_with("Holder::get$instance$"))
        );
    }

    #[test]
    fn resolve_should_validate_trait_bounds_and_dispatch_statically() {
        let source = "
            trait Measure {
                fn measure(&self) -> i32;
            }
            struct Counter { value: i32 }
            impl Measure for Counter {
                fn measure(&self) -> i32 { self.value }
            }
            fn read<T: Measure>(value: T) -> i32 { value.measure() }
            fn main() -> i32 { read(Counter { value: 42 }) }
        ";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions.len(), 3);
        assert!(
            program
                .functions
                .iter()
                .any(|function| function.name.starts_with("read$instance$"))
        );
    }

    #[test]
    fn resolve_should_find_generic_methods_through_borrowed_receivers() {
        let source = "
            struct Holder<T> { value: T }
            impl<T> Holder<T> {
                fn new(value: T) -> Holder<T> { Holder { value: value } }
                fn get(&self) -> T { self.value }
                fn set(&mut self, value: T) { self.value = value; }
            }
            fn read(holder: &Holder<i32>) -> i32 { holder.get() }
            fn update(holder: &mut Holder<i32>) {
                let previous = holder.get();
                holder.set(previous + 1);
            }
            fn main() -> i32 {
                let mut holder: Holder<i32> = Holder::new(41);
                update(&mut holder);
                read(&holder)
            }
        ";

        let program = resolve_fixture(source).expect("borrowed generic receiver should resolve");

        assert!(
            program
                .functions
                .iter()
                .any(|function| function.name.starts_with("Holder::get$instance$"))
        );
    }

    #[test]
    fn resolve_should_accept_library_defined_marker_derives() {
        let source = "
            trait Component: Copy + Send + Sync {}
            @derive(Copy, Component)
            struct Position { x: f32, y: f32 }
            fn require_component<T: Component>(value: T) -> T { value }
            fn main() -> i32 {
                let position = Position { x: 10.0, y: 20.0 };
                let checked = require_component(position);
                if position.x == checked.x { 42 } else { 0 }
            }
        ";

        let program = resolve_fixture(source).expect("marker derive should satisfy the bound");
        let position = program
            .types
            .iter()
            .find(|definition| definition.name.as_deref() == Some("Position"))
            .expect("fixture should define Position");

        assert_eq!(position.marker_traits, ["Component"]);
    }

    #[test]
    fn resolve_should_derive_pod_for_padding_free_c_structs() {
        let source = "
            @repr(C)
            @derive(Pod)
            struct Vertex { x: f32, y: f32 }
            fn require_pod<T: Pod>(value: T) -> T { value }
            fn main() -> i32 {
                let vertex = require_pod(Vertex { x: 20.0, y: 22.0 });
                if vertex.x == 20.0 && vertex.y == 22.0 { 0 } else { 1 }
            }
        ";

        let program = resolve_fixture(source).expect("padding-free C data should derive Pod");
        let vertex = program
            .types
            .iter()
            .find(|definition| definition.name.as_deref() == Some("Vertex"))
            .expect("fixture should define Vertex");

        assert!(vertex.derives.contains(&reimer_hir::DerivedTrait::Pod));
        assert!(vertex.derives.contains(&reimer_hir::DerivedTrait::Copy));
    }

    #[test]
    fn resolve_should_reject_pod_when_layout_contains_padding() {
        let source = "
            @repr(C)
            @derive(Pod)
            struct Padded { small: u8, wide: u32 }
            fn main() -> i32 { 0 }
        ";

        let diagnostics = resolve_fixture(source).expect_err("padding must reject Pod");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E7006" && diagnostic.message.contains("Pod")
        }));
    }

    #[test]
    fn resolve_should_reject_unknown_marker_derives() {
        let source = "
            @derive(Missing)
            struct Position { x: f32 }
            fn main() -> i32 { 42 }
        ";

        let diagnostics = resolve_fixture(source).expect_err("unknown derive should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E7004" && diagnostic.message.contains("unknown derive `Missing`")
        }));
    }

    #[test]
    fn resolve_should_reject_deriving_traits_with_methods() {
        let source = "
            trait Component { fn id(&self) -> u64; }
            @derive(Component)
            struct Position { x: f32 }
            fn main() -> i32 { 42 }
        ";

        let diagnostics =
            resolve_fixture(source).expect_err("behavioral traits must be implemented explicitly");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E7004"
                && diagnostic
                    .message
                    .contains("`Component` is not a marker trait")
        }));
    }

    #[test]
    fn resolve_should_validate_marker_derive_supertraits() {
        let source = "
            trait Component: Copy {}
            @derive(Component)
            struct Position { x: f32 }
            fn main() -> i32 { 42 }
        ";

        let diagnostics =
            resolve_fixture(source).expect_err("missing marker requirements should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E7006"
                && diagnostic
                    .message
                    .contains("cannot derive `Component` for `Position`")
        }));
    }

    #[test]
    fn resolve_should_reject_unsatisfied_trait_bound() {
        let source = "
            trait Measure { fn measure(&self) -> i32; }
            fn read<T: Measure>(value: T) -> i32 { 42 }
            fn main() -> i32 { read(true) }
        ";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E6014"
                && diagnostic.message.contains("does not satisfy trait bound")
        }));
    }

    #[test]
    fn resolve_should_apply_explicit_copy_marker_to_move_analysis() {
        let source = "
            struct Pair { left: i32, right: i32 }
            impl Copy for Pair {}
            fn duplicate<T: Copy>(value: T) -> T {
                let other = value;
                value
            }
            fn main() -> i32 {
                let pair = duplicate(Pair { left: 42, right: 7 });
                pair.left
            }
        ";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions.len(), 2);
    }

    #[test]
    fn resolve_should_reject_trait_method_return_mismatch() {
        let source = "
            trait Measure { fn measure(&self) -> bool; }
            struct Counter { value: i32 }
            impl Measure for Counter {
                fn measure(&self) -> i32 { self.value }
            }
            fn main() -> i32 { 42 }
        ";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E6013" && diagnostic.message.contains("return type")
        }));
    }

    #[test]
    fn resolve_should_type_all_enum_variant_forms() {
        let source = "enum Value {
                Empty,
                Number(i32),
                Named { value: i32 },
            }
            fn make_number() -> Value { Value::Number(42) }
            fn main() -> i32 {
                let empty = Value::Empty;
                let number = make_number();
                let named = Value::Named { value: 42 };
                42
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert!(matches!(program.functions[0].return_type, Type::Enum(_)));
    }

    #[test]
    fn resolve_should_report_missing_and_unknown_struct_fields() {
        let source = "struct Pair { left: i32, right: i32 } fn main() -> i32 {
                let pair = Pair { left: 20, other: 22 };
                0
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| { matches!(diagnostic.code, "E3121" | "E3122") })
        );
    }

    #[test]
    fn resolve_should_accept_mutable_fields_and_array_elements() {
        let source = "struct Pair { left: i32, right: i32 }
            fn main() -> i32 {
                let mut pair = Pair { left: 18, right: 0 };
                let mut values: [i32; 2] = [11, 0];
                pair.left += 2;
                values[0] *= 2;
                pair.right = values[0];
                pair.left + pair.right
            }";

        resolve_fixture(source).expect("fixture should resolve");
    }

    #[test]
    fn resolve_should_reject_field_assignment_through_immutable_binding() {
        let source = "struct Pair { left: i32, right: i32 }
            fn main() -> i32 {
                let pair = Pair { left: 20, right: 22 };
                pair.left = 0;
                pair.left + pair.right
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3107")
        );
    }

    #[test]
    fn resolve_should_reject_recursive_types_stored_by_value() {
        let source = "struct First { next: Second }
            struct Second { next: First }
            fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3012")
        );
    }

    #[test]
    fn resolve_should_type_match_loop_for_and_pattern_bindings() {
        let source = "enum Value { Empty, Pair(i32, i32) }
            fn main() -> i32 {
                let values: [i32; 2] = [20, 22];
                let mut sum = 0;
                for value in values { sum += value; }
                let selected = Value::Pair(sum, 0);
                let result = match selected {
                    Value::Empty => 0,
                    Value::Pair(left, right) if right != 0 => left + right,
                    Value::Pair(left, _) => left,
                };
                loop { break result; }
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions[0].body.ty, Type::I32);
    }

    #[test]
    fn resolve_should_require_exhaustive_matches() {
        let source = "enum Value { Empty, Number(i32) }
            fn main() -> i32 {
                match Value::Empty {
                    Value::Empty => 42,
                }
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3133")
        );
    }

    #[test]
    fn resolve_should_reject_refutable_for_patterns() {
        let source = "fn main() -> i32 {
            for 1 in [1, 2] { }
            42
        }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3131")
        );
    }

    #[test]
    fn resolve_should_type_references_slices_str_and_raw_pointers() {
        let source = "fn adjust(value: &mut i32) { *value += 2; }
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
            }";

        resolve_fixture(source).expect("fixture should resolve");
    }

    #[test]
    fn resolve_should_reject_raw_dereference_outside_unsafe() {
        let source = "fn main() -> i32 {
            let mut value = 42;
            let raw: *mut i32 = &mut value as *mut i32;
            *raw
        }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3136")
        );
    }

    #[test]
    fn resolve_should_reject_escaping_local_references() {
        let source = "fn invalid() -> &i32 {
                let value = 42;
                &value
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3137")
        );
    }

    #[test]
    fn resolve_should_track_scoped_aggregate_lifetimes() {
        let source = "
            struct View { data: &i32 }
            fn view(value: &i32) -> View { View { data: value } }
            fn main() -> i32 {
                let value = 42;
                let borrowed = view(&value);
                *borrowed.data
            }";

        resolve_fixture(source).expect("a scoped aggregate rooted in a parameter should resolve");
    }

    #[test]
    fn resolve_should_keep_scoped_roots_through_match_payload_bindings() {
        let source = "
            fn selected(value: &i32) -> Option<&i32> {
                match Some(value) {
                    Some(item) => Some(item),
                    None => None,
                }
            }
            fn main() -> i32 {
                let value = 42;
                match selected(&value) {
                    Some(item) => *item,
                    None => 0,
                }
            }";

        resolve_fixture(source).expect("match payload should retain the owner's scoped root");
    }

    #[test]
    fn resolve_should_track_scoped_returns_through_locals() {
        let source = "
            struct View { data: &i32 }
            struct Wrapper { view: View }
            fn view(value: &i32) -> View { View { data: value } }
            fn wrap(value: &i32) -> Wrapper {
                let borrowed = view(value);
                Wrapper { view: borrowed }
            }
            fn main() -> i32 {
                let value = 42;
                let wrapped = wrap(&value);
                *wrapped.view.data
            }";

        resolve_fixture(source).expect("a scoped local should retain its parameter root");
    }

    #[test]
    fn resolve_should_allow_chained_by_value_methods_on_temporaries() {
        let source = "
            struct Builder { value: i32 }
            impl Builder {
                fn new() -> Builder { Builder { value: 0 } }
                fn with_value(self, value: i32) -> Builder {
                    Builder { value: value }
                }
            }
            fn main() -> i32 {
                Builder::new().with_value(42).value
            }";

        resolve_fixture(source).expect("by-value builder methods should accept temporaries");
    }

    #[test]
    fn resolve_should_reject_local_scoped_returns_through_locals() {
        let source = "
            struct View { data: &i32 }
            fn view(value: &i32) -> View { View { data: value } }
            fn invalid() -> View {
                let value = 42;
                let borrowed = view(&value);
                borrowed
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3137")
        );
    }

    #[test]
    fn resolve_should_reject_scoped_aggregate_escape() {
        let source = "
            struct View { data: &i32 }
            fn invalid() -> View {
                let value = 42;
                View { data: &value }
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3137")
        );
    }

    #[test]
    fn resolve_should_keep_owner_borrowed_by_scoped_aggregate() {
        let source = "
            struct View { data: &i32 }
            fn main() -> i32 {
                let mut value = 42;
                let borrowed = View { data: &value };
                value = 0;
                *borrowed.data
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3107" || diagnostic.code == "E3138")
        );
    }

    #[test]
    fn resolve_should_reject_scoped_values_hidden_by_raw_generic_storage() {
        let source = "
            struct View { data: &i32 }
            struct RawOwner<T> { data: *mut T }
            fn consume(value: RawOwner<View>) {}
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3013")
        );
    }

    #[test]
    fn resolve_should_keep_method_owner_live_for_returned_view() {
        let source = "
            struct Owner { value: i32 }
            struct View { data: &i32 }
            impl Owner {
                fn view(&self) -> View { View { data: &self.value } }
                fn deinit(self) {}
            }
            fn main() -> i32 {
                let owner = Owner { value: 42 };
                let borrowed = owner.view();
                owner.deinit();
                *borrowed.data
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3138")
        );
    }

    #[test]
    fn resolve_should_reject_conflicting_scoped_borrows() {
        let source = "fn main() -> i32 {
            let mut value = 42;
            let shared = &value;
            let exclusive = &mut value;
            *shared
        }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3138")
        );
    }

    #[test]
    fn resolve_should_type_option_result_and_try() {
        let source = "fn maybe(flag: bool) -> Option<i32> {
                if flag { Some(42) } else { None }
            }
            fn forward(flag: bool) -> Option<i32> {
                let value = maybe(flag)?;
                Some(value)
            }
            fn fallible(flag: bool) -> Result<Option<i32>, i32> {
                if flag { Ok(Some(42)) } else { Err(7) }
            }
            fn main() -> i32 {
                match forward(true) {
                    Some(value) => value,
                    None => 0,
                }
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions.len(), 4);
        assert!(matches!(program.functions[0].return_type, Type::Enum(_)));
    }

    #[test]
    fn resolve_should_reject_invalid_try_contexts() {
        let source = "fn not_fallible() -> i32 { 42? }
            fn mismatched(value: Option<i32>) -> Result<i32, i32> {
                value?
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "E3140")
                .count()
                >= 2
        );
    }

    #[test]
    fn resolve_should_type_defer_and_panic() {
        let source = "fn record(value: &mut i32, digit: i32) {
                *value = *value * 10 + digit;
            }
            fn cleanup(value: &mut i32, flag: bool) -> Option<i32> {
                defer record(value, 1);
                if flag { Some(42) } else { None?; Some(0) }
            }
            fn choose(flag: bool) -> i32 {
                if flag { 42 } else { panic(\"unreachable\") }
            }
            fn main() -> i32 { choose(true) }";

        resolve_fixture(source).expect("fixture should resolve");
    }

    #[test]
    fn resolve_should_type_runtime_and_debug_assertions() {
        let source = "fn main() -> i32 {
                assert(true);
                assert(20 + 22 == 42, \"arithmetic invariant failed\");
                debug_assert(true, \"debug invariant failed\");
                42
            }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert!(matches!(
            program.functions[0].body.statements[0],
            reimer_hir::Statement::Expression(reimer_hir::Expression {
                kind: reimer_hir::ExpressionKind::Assert {
                    mode: reimer_hir::AssertionMode::Always,
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            program.functions[0].body.statements[2],
            reimer_hir::Statement::Expression(reimer_hir::Expression {
                kind: reimer_hir::ExpressionKind::Assert {
                    mode: reimer_hir::AssertionMode::Debug,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn resolve_should_reject_invalid_assertion_arguments() {
        let source = "fn main() -> i32 {
                assert();
                assert(42, \"not a boolean\");
                debug_assert(true, 42);
                0
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E6004" && diagnostic.message.contains("optional message")
        }));
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("assertion condition"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("assertion message"))
        );
    }

    #[test]
    fn resolve_should_allow_borrowing_a_resource_after_registering_its_cleanup() {
        let source = "
            struct Resource { value: i32 }
            fn destroy(resource: Resource) {}
            fn inspect(resource: &Resource) -> i32 { (*resource).value }
            fn main() -> i32 {
                let resource = Resource { value: 42 };
                defer destroy(resource);
                inspect(&resource)
            }";

        let result = resolve_fixture(source);

        assert!(
            result.is_ok(),
            "deferred cleanup should reserve rather than move"
        );
    }

    #[test]
    fn resolve_should_reject_moving_a_resource_reserved_for_cleanup() {
        let source = "
            struct Resource { value: i32 }
            fn destroy(resource: Resource) {}
            fn main() -> i32 {
                let resource = Resource { value: 42 };
                defer destroy(resource);
                let moved = resource;
                moved.value
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3147")
        );
    }

    #[test]
    fn resolve_should_reject_two_deferred_consumers_of_one_resource() {
        let source = "
            struct Resource { value: i32 }
            fn destroy(resource: Resource) {}
            fn main() -> i32 {
                let resource = Resource { value: 42 };
                defer destroy(resource);
                defer destroy(resource);
                42
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3147")
        );
    }

    #[test]
    fn resolve_should_keep_string_view_intrinsics_private_to_standard_wrappers() {
        let source = r#"
            fn main() -> i32 {
                __string_length("private") as i32
            }"#;

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3148")
        );
    }

    #[test]
    fn resolve_should_register_c_abi_function_declarations() {
        let source = "
            extern \"C\" fn native_abs(value: i32) -> i32;
            fn main() -> i32 { unsafe { native_abs(-42) } }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.extern_functions.len(), 1);
    }

    #[test]
    fn resolve_should_allow_type_aliases_to_reference_later_aliases() {
        let source = "
            pub type TaskKind = NativeInt;
            @repr(C) pub struct Task { pub kind: TaskKind }
            pub type NativeInt = c_int;
            fn main() -> i32 { 42 }";

        let program = resolve_fixture(source).expect("fixture should resolve");
        let task = program
            .types
            .iter()
            .find(|definition| definition.name.as_deref() == Some("Task"))
            .expect("task type should exist");
        let reimer_hir::TypeDefinitionKind::Struct { fields } = &task.kind else {
            panic!("task should remain a struct");
        };
        assert_eq!(fields[0].ty, Type::I32);
    }

    #[test]
    fn resolve_should_erase_target_correct_c_type_aliases() {
        let source = r#"
            pub type CInt = c_int;
            pub type CLong = c_long;
            extern "C" fn inspect(value: CInt, offset: CLong) -> CInt;
            fn main() -> i32 { 42 }
        "#;

        let program = resolve_fixture(source).expect("fixture should resolve");
        let function = &program.extern_functions[0];
        let expected_long = if size_of::<std::ffi::c_long>() == size_of::<i32>() {
            Type::I32
        } else {
            Type::I64
        };

        assert_eq!(function.parameters[0].ty, Type::I32);
        assert_eq!(function.parameters[1].ty, expected_long);
        assert_eq!(function.return_type, Type::I32);
    }

    #[test]
    fn resolve_should_type_c_string_literals_for_ffi_calls() {
        let source = r#"
            extern "C" fn string_length(value: cstr) -> usize;
            fn main() -> i32 {
                unsafe { string_length(c"Reimer") as i32 }
            }"#;

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions[0].body.ty, Type::I32);
    }

    #[test]
    fn resolve_should_reject_escaping_c_string_literals() {
        let source = r#"fn title() -> cstr { c"temporary" } fn main() -> i32 { 42 }"#;

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3137")
        );
    }

    #[test]
    fn resolve_should_require_unsafe_for_native_calls() {
        let source = "
            extern \"C\" fn native_abs(value: i32) -> i32;
            fn main() -> i32 { native_abs(-42) }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E5002")
        );
    }

    #[test]
    fn resolve_should_reject_non_abi_safe_external_parameters() {
        let source = "
            extern \"C\" fn native_log(value: str);
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E5001")
        );
    }

    #[test]
    fn resolve_should_accept_abi_safe_c_structs_by_value() {
        let source = "
            @repr(C)
            struct Identifier { bytes: [u8; 16] }
            @repr(C)
            struct Request { identifier: Identifier, count: u32 }
            extern \"C\" {
                fn native_read(request: Request) -> Identifier;
            }
            fn main() -> i32 {
                let request = Request {
                    identifier: Identifier {
                        bytes: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    },
                    count: 1,
                };
                let identifier = unsafe { native_read(request) };
                identifier.bytes[0] as i32
            }";

        resolve_fixture(source).expect("ABI-safe C structs should resolve by value");
    }

    #[test]
    fn resolve_should_reject_native_structs_by_value_at_ffi_boundaries() {
        let source = "
            struct Native { value: i32 }
            extern \"C\" fn native_read(value: Native) -> Native;
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("native struct should fail");

        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "E5001")
                .count(),
            2
        );
    }

    #[test]
    fn resolve_should_accept_abi_safe_callbacks_in_external_signatures() {
        let source = "
            extern \"C\" fn native_apply(
                callback: fn(i32) -> i32,
                value: i32,
            ) -> i32;
            fn increment(value: i32) -> i32 { value + 1 }
            fn main() -> i32 {
                unsafe { native_apply(increment, 41) }
            }";

        resolve_fixture(source).expect("ABI-safe callback should resolve");
    }

    #[test]
    fn resolve_should_reject_non_abi_safe_callback_parameters() {
        let source = "
            extern \"C\" fn install(callback: fn(str) -> ());
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E5001")
        );
    }

    #[test]
    fn resolve_should_require_unsafe_for_address_to_function_casts() {
        let source = "
            fn main() -> i32 {
                let address = 1 as usize;
                let callback = address as fn(i32) -> i32;
                callback(41)
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E5002")
        );
    }

    #[test]
    fn resolve_should_accept_unsafe_loaded_function_casts() {
        let source = "
            extern \"C\" fn load(name: cstr) -> fn() -> ();
            fn main() -> i32 {
                let loaded = unsafe { load(c\"increment\") };
                let callback = unsafe { loaded as fn(i32) -> i32 };
                callback(41)
            }";

        resolve_fixture(source).expect("unsafe function cast should resolve");
    }

    #[test]
    fn resolve_should_accept_unsafe_raw_pointer_to_function_casts() {
        let source = "
            type Callback = fn(i32) -> i32;
            fn load(address: *const u8) -> Callback {
                unsafe { address as Callback }
            }
            fn main() -> i32 { 42 }";

        resolve_fixture(source).expect("unsafe raw pointer cast should resolve");
    }

    #[test]
    fn scoped_return_should_follow_owner_through_raw_pointer_casts() {
        let source = "
            struct Text { data: *const c_char }
            impl Text {
                fn as_cstr(&self) -> cstr { self.data as cstr }
            }
            extern \"C\" fn native_title(owner: *const u8) -> *const c_char;
            struct Window { raw: *const u8 }
            impl Window {
                fn title(&self) -> cstr {
                    unsafe { native_title(self.raw) } as cstr
                }
            }
            fn main() -> i32 { 42 }";

        resolve_fixture(source).expect("returned views should remain rooted in their owner");
    }

    #[test]
    fn invalid_cast_diagnostic_should_render_structural_type_names() {
        let source = "
            fn main() -> i32 {
                let callback = unsafe { true as fn(i32) -> i32 };
                callback(41)
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E3113")
            .expect("invalid cast diagnostic should be emitted");

        assert_eq!(diagnostic.message, "cannot cast `bool` to `fn(i32) -> i32`");
    }

    #[test]
    fn resolve_should_preserve_ffi_layout_and_link_metadata() {
        let source = "
            @repr(C)
            pub struct Vector2 { pub x: f32, pub y: f32 }
            @link(\"raylib\")
            extern \"C\" {
                fn draw(value: *const Vector2, title: cstr) -> bool;
            }
            fn main() -> i32 { 42 }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(
            program.types[0].representation,
            reimer_hir::TypeRepresentation::C
        );
        assert_eq!(program.extern_functions[0].link.as_deref(), Some("raylib"));
    }

    #[test]
    fn resolve_should_reject_pointers_to_native_layout_structs_at_ffi_boundary() {
        let source = "
            struct Vector2 { x: f32, y: f32 }
            extern \"C\" fn draw(value: *const Vector2);
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E5001")
        );
    }

    #[test]
    fn resolve_should_type_inherent_associated_and_receiver_calls() {
        let source = include_str!("../../../examples/m6_methods.reim");

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert_eq!(program.functions.len(), 4);
        assert!(
            program
                .functions
                .iter()
                .any(|function| function.name == "Counter::add")
        );
    }

    #[test]
    fn resolve_should_reject_control_transfer_from_defer() {
        let source = "fn invalid() -> Option<i32> {
                defer { return Some(0); }
                defer None?;
                Some(42)
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "E3142")
                .count()
                >= 2
        );
    }

    #[test]
    fn resolve_should_reject_use_after_implicit_move() {
        let source = "struct Resource { id: i32 }
            fn consume(resource: Resource) -> i32 { resource.id }
            fn main() -> i32 {
                let first = Resource { id: 20 };
                let second = first;
                let invalid = consume(first);
                consume(second)
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3143")
        );
    }

    #[test]
    fn resolve_should_track_moves_of_independent_struct_fields() {
        let source = "struct Resource { id: i32 }
            struct Pair { first: Resource, second: Resource }
            fn consume(resource: Resource) -> i32 { resource.id }
            fn main() -> i32 {
                let pair = Pair {
                    first: Resource { id: 20 },
                    second: Resource { id: 22 },
                };
                let first = pair.first;
                consume(pair.second) + consume(first)
            }";

        resolve_fixture(source).expect("disjoint fields should move independently");
    }

    #[test]
    fn resolve_should_reject_reusing_a_moved_struct_field() {
        let source = "struct Resource { id: i32 }
            struct Pair { first: Resource, second: Resource }
            fn consume(resource: Resource) -> i32 { resource.id }
            fn main() -> i32 {
                let pair = Pair {
                    first: Resource { id: 20 },
                    second: Resource { id: 22 },
                };
                let first = pair.first;
                consume(pair.first) + consume(first)
            }";

        let diagnostics = resolve_fixture(source).expect_err("a moved field should be unavailable");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3143")
        );
    }

    #[test]
    fn resolve_should_allow_reinitializing_a_moved_struct_field() {
        let source = "struct Resource { id: i32 }
            struct Pair { first: Resource, second: Resource }
            fn consume(resource: Resource) -> i32 { resource.id }
            fn main() -> i32 {
                let mut pair = Pair {
                    first: Resource { id: 20 },
                    second: Resource { id: 22 },
                };
                let first = pair.first;
                pair.first = Resource { id: 21 };
                consume(pair.first) + consume(pair.second) + consume(first)
            }";

        resolve_fixture(source).expect("an assigned field should become available again");
    }

    #[test]
    fn resolve_should_allow_copy_reuse_reinitialization_and_mutable_reborrows() {
        let source = "struct Resource { id: i32 }
            fn consume(resource: Resource) -> i32 { resource.id }
            fn increment(value: &mut i32) { *value += 1; }
            fn main() -> i32 {
                let mut resource = Resource { id: 19 };
                let moved = resource;
                resource = Resource { id: 21 };
                let tuple = (20, 22);
                let tuple_copy = tuple;
                let mut scalar = 18;
                let view = &mut scalar;
                increment(view);
                increment(view);
                consume(moved) + consume(resource) + tuple.0 + tuple_copy.1
            }";

        resolve_fixture(source).expect("fixture should resolve");
    }

    #[test]
    fn resolve_should_automatically_reborrow_mutable_method_receivers() {
        let source = "
            struct Counter { value: i32 }
            impl Counter {
                fn increment(&mut self) { self.value += 1; }
            }
            fn increment_twice(counter: &mut Counter) {
                counter.increment();
                counter.increment();
            }
            fn main() -> i32 {
                let mut counter = Counter { value: 40 };
                increment_twice(&mut counter);
                counter.value
            }";

        resolve_fixture(source).expect("mutable method receiver should be reborrowed");
    }

    #[test]
    fn resolve_should_immutably_reborrow_mutable_method_receivers() {
        let source = "
            struct Counter { value: i32 }
            impl Counter {
                fn read(&self) -> i32 { self.value }
                fn increment(&mut self) { self.value += self.read(); }
            }
            fn read_then_increment(counter: &mut Counter) -> i32 {
                let previous = counter.read();
                counter.increment();
                previous + counter.read()
            }
            fn main() -> i32 {
                let mut counter = Counter { value: 14 };
                read_then_increment(&mut counter)
            }";

        resolve_fixture(source).expect("immutable access through a mutable view should reborrow");
    }

    #[test]
    fn resolve_should_keep_raw_string_construction_private() {
        let source = "fn invalid() -> str {
                __str_from_parts(0 as *const u8, 0)
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3149")
        );
    }

    #[test]
    fn resolve_should_keep_native_element_stride_private() {
        let source = "fn invalid() -> usize {
                __pointee_stride(0 as usize as *const i32)
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3150")
        );
    }

    #[test]
    fn resolve_should_keep_structural_hashing_private() {
        let source = "fn invalid() -> u64 {
                __hash_value(42, 7)
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3150")
        );
    }

    #[test]
    fn resolve_should_keep_raw_slice_construction_private() {
        let source = "fn invalid(data: *const i32) -> &[i32] {
                __slice_from_parts(data, 1)
            }
            fn main() -> i32 { 42 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E3150")
        );
    }

    #[test]
    fn resolve_should_type_function_values_and_indirect_calls() {
        let source = "
            fn add(left: i32, right: i32) -> i32 { left + right }
            fn apply(callback: fn(i32, i32) -> i32) -> i32 {
                callback(20, 22)
            }
            fn main() -> i32 { apply(add) }";

        let program = resolve_fixture(source).expect("fixture should resolve");

        assert!(matches!(
            program.functions[1].parameters[0].ty,
            Type::Function(_)
        ));
    }

    #[test]
    fn resolve_should_derive_send_for_safe_aggregate_fields() {
        let source = "
            struct Work { value: i32 }
            fn transfer<T: Send>(value: T) -> T { value }
            fn main() -> i32 { transfer(Work { value: 42 }).value }";

        resolve_fixture(source).expect("fixture should resolve");
    }

    #[test]
    fn type_registry_should_treat_owned_strings_as_send_and_sync() {
        let span = Span::empty(0);
        let pointer = Type::RawPointer(TypeId(0));
        let string = Type::Struct(TypeId(1));
        let types = TypeRegistry {
            definitions: vec![
                TypeDefinition {
                    id: TypeId(0),
                    name: None,
                    documentation: None,
                    kind: TypeDefinitionKind::RawPointer {
                        target: Type::U8,
                        mutable: true,
                    },
                    representation: TypeRepresentation::Native,
                    alignment: None,
                    derives: Vec::new(),
                    marker_traits: Vec::new(),
                    must_use: false,
                    span,
                },
                TypeDefinition {
                    id: TypeId(1),
                    name: Some(STANDARD_STRING_TYPE.to_owned()),
                    documentation: None,
                    kind: TypeDefinitionKind::Struct {
                        fields: vec![TypeField {
                            name: "data".to_owned(),
                            is_public: false,
                            ty: pointer,
                            span,
                        }],
                    },
                    representation: TypeRepresentation::Native,
                    alignment: None,
                    derives: Vec::new(),
                    marker_traits: Vec::new(),
                    must_use: false,
                    span,
                },
            ],
            ..TypeRegistry::default()
        };

        assert!(types.satisfies_trait(string, "Send"));
        assert!(types.satisfies_trait(string, "Sync"));
        assert!(!types.satisfies_trait(pointer, "Send"));
    }

    #[test]
    fn resolve_should_never_derive_send_for_raw_pointers() {
        let source = "
            fn transfer<T: Send>(value: T) -> T { value }
            fn main() -> i32 {
                let pointer = 0 as usize as *const i32;
                let moved = transfer(pointer);
                42
            }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("Send"))
        );
    }

    #[test]
    fn resolve_should_evaluate_m10_constants_attributes_and_reflection() {
        let source = include_str!("../../../examples/m10_comptime.reim");

        let program = resolve_fixture(source).expect("M10 fixture should resolve");
        let header = program
            .types
            .iter()
            .find(|definition| definition.name.as_deref() == Some("Header"))
            .expect("fixture should define Header");

        assert_eq!(header.alignment, Some(16));
        assert_eq!(header.derives.len(), 6);
        assert_eq!(program.tests.len(), 1);
    }

    #[test]
    fn resolve_should_use_comptime_results_in_const_generics_and_runtime_code() {
        let source = "
            comptime fn doubled(value: usize) -> usize { value * 2 }
            const LENGTH: usize = doubled(2);
            fn identity<T>(value: T) -> T { value }
            fn main() -> i32 {
                let values: [i32; LENGTH] = [10, 10, 10, 12];
                identity<i32>(values[0] + values[1] + values[2] + values[3])
            }";

        resolve_fixture(source).expect("compile-time and explicit generic fixture should resolve");
    }

    #[test]
    fn resolve_should_bind_explicit_method_generics_after_impl_generics() {
        let source = "
            struct Holder<T> { value: T }
            impl<T> Holder<T> {
                fn replace<U>(&self, replacement: U) -> U { replacement }
            }
            fn main() -> i32 {
                let holder: Holder<bool> = Holder { value: true };
                holder.replace<i32>(42)
            }";

        resolve_fixture(source).expect("explicit method generic fixture should resolve");
    }

    #[test]
    fn resolve_should_reject_runtime_calls_during_comptime_evaluation() {
        let source = "
            fn runtime_value() -> usize { 42 }
            comptime fn invalid() -> usize { runtime_value() }
            const VALUE: usize = invalid();
            fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E7012")
        );
    }

    #[test]
    fn resolve_should_report_a_failing_top_level_comptime_assertion() {
        let source = "
            comptime { assert(size_of<i32>() == 8); }
            fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E7013")
        );
    }

    #[test]
    fn resolve_should_use_custom_comptime_assertion_messages() {
        let source = "
            comptime {
                debug_assert(true, \"checked during compile-time evaluation\");
                assert(false, \"header layout changed\");
            }
            fn main() -> i32 { 0 }";

        let diagnostics = resolve_fixture(source).expect_err("fixture should fail");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E7013" && diagnostic.message == "header layout changed"
        }));
    }
}
