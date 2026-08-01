//! Parses a target-preprocessed SDL header into a small, deterministic ABI model.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use lang_c::ast::{
    ArraySize, BinaryOperator, Constant, Declaration, DeclarationSpecifier, Declarator,
    DeclaratorKind, DerivedDeclarator, Ellipsis, EnumType, Expression, ExternalDeclaration,
    IntegerBase, SpecifierQualifier, StorageClassSpecifier, StructDeclaration, StructKind,
    TypeQualifier, TypeSpecifier, UnaryOperator,
};
use lang_c::driver::{Config, Flavor};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderModel {
    pub(crate) aliases: BTreeMap<String, CType>,
    pub(crate) callbacks: BTreeMap<String, CType>,
    pub(crate) records: BTreeMap<String, Record>,
    pub(crate) constants: Vec<EnumConstant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) fields: Option<Vec<Field>>,
    pub(crate) blocker: Option<RecordBlocker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) ty: CType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordBlocker {
    Union,
    AnonymousField,
    BitField,
    FlexibleArray,
    UnsupportedType,
}

impl RecordBlocker {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Union => "union",
            Self::AnonymousField => "anonymous_field",
            Self::BitField => "bit_field",
            Self::FlexibleArray => "flexible_array",
            Self::UnsupportedType => "unsupported_type",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumConstant {
    pub(crate) name: String,
    pub(crate) ty: String,
    pub(crate) value: i128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MacroConstant {
    pub(crate) name: String,
    pub(crate) ty: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MacroModel {
    pub(crate) discovered: usize,
    pub(crate) constants: Vec<MacroConstant>,
    pub(crate) excluded: Vec<MacroExclusion>,
    pub(crate) blocked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MacroExclusion {
    pub(crate) name: String,
    pub(crate) reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CType {
    Unit,
    Bool,
    Char,
    SignedChar,
    UnsignedChar,
    Short,
    UnsignedShort,
    Int,
    UnsignedInt,
    Long,
    UnsignedLong,
    LongLong,
    UnsignedLongLong,
    Float,
    Double,
    Named(String),
    Pointer {
        target: Box<CType>,
        mutable: bool,
    },
    Array {
        element: Box<CType>,
        length: usize,
    },
    Function {
        parameters: Vec<CType>,
        result: Box<CType>,
    },
}

impl CType {
    pub(crate) fn render(&self) -> String {
        match self {
            Self::Unit => "()".to_owned(),
            Self::Bool => "bool".to_owned(),
            Self::Char => "c::Char".to_owned(),
            Self::SignedChar => "c::SignedChar".to_owned(),
            Self::UnsignedChar => "c::UnsignedChar".to_owned(),
            Self::Short => "c::Short".to_owned(),
            Self::UnsignedShort => "c::UnsignedShort".to_owned(),
            Self::Int => "c::Int".to_owned(),
            Self::UnsignedInt => "c::UnsignedInt".to_owned(),
            Self::Long => "c::Long".to_owned(),
            Self::UnsignedLong => "c::UnsignedLong".to_owned(),
            Self::LongLong => "c::LongLong".to_owned(),
            Self::UnsignedLongLong => "c::UnsignedLongLong".to_owned(),
            Self::Float => "f32".to_owned(),
            Self::Double => "f64".to_owned(),
            Self::Named(name) => render_named_type(name).to_owned(),
            Self::Pointer { target, mutable } => {
                let qualifier = if *mutable { "mut" } else { "const" };
                format!("*{qualifier} {}", target.render())
            }
            Self::Array { element, length } => format!("[{}; {length}]", element.render()),
            Self::Function { parameters, result } => {
                let parameters = parameters
                    .iter()
                    .map(CType::render)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("fn({parameters}) -> {}", result.render())
            }
        }
    }
}

fn render_named_type(name: &str) -> &str {
    match name {
        "Sint8" | "int8_t" => "i8",
        "Uint8" | "uint8_t" => "u8",
        "Sint16" | "int16_t" => "i16",
        "Uint16" | "uint16_t" | "wchar_t" => "u16",
        "Sint32" | "int32_t" => "i32",
        "Uint32" | "uint32_t" => "u32",
        "Sint64" | "int64_t" => "i64",
        "Uint64" | "uint64_t" => "u64",
        "intptr_t" => "isize",
        "uintptr_t" | "size_t" => "usize",
        _ => name,
    }
}

#[derive(Debug)]
pub(crate) enum HeaderError {
    Syntax(lang_c::driver::SyntaxError),
    InvalidDeclaration(String),
    InvalidEnumExpression(String),
}

impl fmt::Display for HeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(source) => write!(
                formatter,
                "could not parse preprocessed SDL headers: {source}"
            ),
            Self::InvalidDeclaration(detail) => {
                write!(formatter, "invalid SDL declaration: {detail}")
            }
            Self::InvalidEnumExpression(name) => {
                write!(formatter, "could not evaluate SDL enum constant `{name}`")
            }
        }
    }
}

impl Error for HeaderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

pub(crate) fn parse_header(source: &str) -> Result<HeaderModel, HeaderError> {
    let filtered = filter_sdl_preprocessor_output(source);
    let config = Config {
        cpp_command: String::new(),
        cpp_options: Vec::new(),
        flavor: Flavor::ClangC11,
    };
    let parsed =
        lang_c::driver::parse_preprocessed(&config, filtered).map_err(HeaderError::Syntax)?;
    let mut model = HeaderModel {
        aliases: BTreeMap::new(),
        callbacks: BTreeMap::new(),
        records: BTreeMap::new(),
        constants: Vec::new(),
    };
    let mut enum_values = BTreeMap::new();

    for external in &parsed.unit.0 {
        let ExternalDeclaration::Declaration(declaration) = &external.node else {
            continue;
        };
        if !is_typedef(&declaration.node) {
            continue;
        }
        collect_typedef(&declaration.node, &mut model, &mut enum_values)?;
    }
    Ok(model)
}

pub(crate) fn render_macro_probe(source: &str) -> String {
    let names = collect_macro_names(source);
    let mut output = String::from(
        "#include <SDL3/SDL.h>\n\
         #define SDL_BINDING_STRINGIFY_INNER(...) #__VA_ARGS__\n\
         #define SDL_BINDING_STRINGIFY(...) SDL_BINDING_STRINGIFY_INNER(__VA_ARGS__)\n",
    );
    for name in names {
        output.push_str("SDL_BINDING_ENTRY(\"");
        output.push_str(&name);
        output.push_str("\", SDL_BINDING_STRINGIFY(");
        output.push_str(&name);
        output.push_str("))\n");
    }
    output
}

pub(crate) fn parse_macro_expansions(source: &str, header: &HeaderModel) -> MacroModel {
    let enum_values = header
        .constants
        .iter()
        .map(|constant| (constant.name.clone(), constant.value))
        .collect::<BTreeMap<_, _>>();
    let enum_types = header
        .constants
        .iter()
        .map(|constant| (constant.name.clone(), constant.ty.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut entries = Vec::new();
    for line in source.lines() {
        let Some((name, expansion)) = parse_macro_entry(line) else {
            continue;
        };
        if enum_values.contains_key(&name) {
            continue;
        }
        entries.push((name, unescape_stringified(&expansion)));
    }
    let discovered = entries.len();
    let mut constants = Vec::new();
    let mut excluded = Vec::new();
    let mut known_values = enum_values;
    let mut known_types = enum_types;
    let typedef_prelude = macro_typedef_prelude(header);

    let mut pending = Vec::new();
    for (name, expansion) in entries {
        if let Some(reason) = macro_exclusion_reason(&name, &expansion) {
            excluded.push(MacroExclusion { name, reason });
            continue;
        }
        pending.push((name, expansion));
    }

    loop {
        let previous = pending.len();
        let mut deferred = Vec::new();
        for (name, expansion) in pending {
            let translated = translate_macro(
                &name,
                &expansion,
                &typedef_prelude,
                &known_values,
                &known_types,
                &header.aliases,
            );
            let Some((ty, value, numeric_value)) = translated else {
                deferred.push((name, expansion));
                continue;
            };
            if let Some(numeric_value) = numeric_value {
                known_values.insert(name.clone(), numeric_value);
            }
            known_types.insert(name.clone(), ty.clone());
            constants.push(MacroConstant { name, ty, value });
        }
        if deferred.len() == previous {
            pending = deferred;
            break;
        }
        pending = deferred;
    }
    constants.sort_by(|left, right| left.name.cmp(&right.name));
    excluded.sort_by(|left, right| left.name.cmp(&right.name));
    let blocked = pending.into_iter().map(|(name, _)| name).collect();
    MacroModel {
        discovered,
        constants,
        excluded,
        blocked,
    }
}

fn macro_exclusion_reason(name: &str, expansion: &str) -> Option<&'static str> {
    if expansion.contains("_renamed_") {
        return Some("renamed_compatibility_alias");
    }
    if expansion.contains("_deprecated_") || expansion.contains("_removed_") {
        return Some("deprecated_compatibility_alias");
    }
    if expansion == name
        || expansion.starts_with("__declspec")
        || expansion.starts_with("__forceinline")
        || expansion.starts_with("__inline")
        || expansion.starts_with("__restrict")
        || expansion.starts_with("do ")
        || expansion.contains("__FUNCTION__")
    {
        return Some("non_constant_preprocessor_utility");
    }
    None
}

fn collect_macro_names(source: &str) -> Vec<String> {
    let mut names = BTreeSet::new();
    let mut inside_sdl = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#line ") {
            inside_sdl = trimmed.contains("include\\\\SDL3") || trimmed.contains("include/SDL3");
            continue;
        }
        if !inside_sdl {
            continue;
        }
        let Some(body) = trimmed.strip_prefix("#define ") else {
            continue;
        };
        let Some(end) = body.find(char::is_whitespace) else {
            continue;
        };
        let name = &body[..end];
        if name.contains('(') || !is_public_macro_name(name) {
            continue;
        }
        if body[end..].trim().is_empty() {
            continue;
        }
        names.insert(name.to_owned());
    }
    names.into_iter().collect()
}

fn is_public_macro_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("SDL_") else {
        return false;
    };
    !suffix.is_empty()
        && suffix.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn parse_macro_entry(line: &str) -> Option<(String, String)> {
    let body = line.trim().strip_prefix("SDL_BINDING_ENTRY(")?;
    let body = body.strip_suffix(')')?;
    let name_start = body.find('"')?;
    let name_end = find_closing_quote(body, name_start + 1)?;
    let name = unescape_c_string(&body[name_start + 1..name_end])?;
    let value_start = body[name_end + 1..].find('"')? + name_end + 1;
    let value_end = find_closing_quote(body, value_start + 1)?;
    let value = body[value_start..=value_end].to_owned();
    Some((name, value))
}

fn find_closing_quote(source: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (relative, character) in source[start..].char_indices() {
        if character == '"' && !escaped {
            return Some(start + relative);
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    None
}

fn unescape_stringified(source: &str) -> String {
    source
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .and_then(unescape_c_string)
        .unwrap_or_default()
}

fn unescape_c_string(source: &str) -> Option<String> {
    let mut output = String::new();
    let mut characters = source.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let escaped = characters.next()?;
        match escaped {
            '\\' => output.push('\\'),
            '"' => output.push('"'),
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            _ => {
                output.push('\\');
                output.push(escaped);
            }
        }
    }
    Some(output)
}

fn macro_typedef_prelude(header: &HeaderModel) -> String {
    let mut output = String::from(
        "typedef signed char Sint8; typedef unsigned char Uint8;\n\
         typedef signed short Sint16; typedef unsigned short Uint16;\n\
         typedef signed int Sint32; typedef unsigned int Uint32;\n\
         typedef signed long long Sint64; typedef unsigned long long Uint64;\n\
         typedef unsigned long long size_t;\n",
    );
    for name in header
        .aliases
        .keys()
        .chain(header.records.keys())
        .filter(|name| name.starts_with("SDL_"))
    {
        output.push_str("typedef long long ");
        output.push_str(name);
        output.push_str(";\n");
    }
    output
}

fn translate_macro(
    name: &str,
    expansion: &str,
    typedef_prelude: &str,
    known_values: &BTreeMap<String, i128>,
    known_types: &BTreeMap<String, String>,
    aliases: &BTreeMap<String, CType>,
) -> Option<(String, String, Option<i128>)> {
    let expansion = expansion.trim();
    if expansion.is_empty() || expansion == name {
        return None;
    }
    if expansion.starts_with('"') {
        let value = translate_string_tokens(expansion)?;
        return Some(("cstr".to_owned(), value, None));
    }
    if let Some((ty, value)) = translate_float_literal(expansion) {
        return Some((ty, value, None));
    }

    let normalized_expansion = expansion.replace("ui64", "ULL").replace("i64", "LL");
    let mut source = typedef_prelude.to_owned();
    source.push_str("enum SDL_BindingValue { SDL_BINDING_VALUE = ");
    source.push_str(&normalized_expansion);
    source.push_str(" };\n");
    let config = Config {
        cpp_command: String::new(),
        cpp_options: Vec::new(),
        flavor: Flavor::ClangC11,
    };
    let parsed = lang_c::driver::parse_preprocessed(&config, source).ok()?;
    let expression = parsed.unit.0.iter().rev().find_map(|external| {
        let ExternalDeclaration::Declaration(declaration) = &external.node else {
            return None;
        };
        declaration.node.specifiers.iter().find_map(|specifier| {
            let DeclarationSpecifier::TypeSpecifier(ty) = &specifier.node else {
                return None;
            };
            let TypeSpecifier::Enum(enumeration) = &ty.node else {
                return None;
            };
            enumeration
                .node
                .enumerators
                .first()?
                .node
                .expression
                .as_deref()
        })
    })?;
    let value = evaluate_integer(&expression.node, known_values)?;
    let ty = infer_macro_integer_type(&normalized_expansion, value, known_types);
    let (rendered, normalized) = normalize_integer_literal(value, &ty, aliases)?;
    Some((ty, rendered, Some(normalized)))
}

fn translate_string_tokens(expansion: &str) -> Option<String> {
    let mut cursor = expansion.trim();
    let mut combined = String::new();
    while !cursor.is_empty() {
        cursor = cursor.trim_start();
        let prefix_length = if cursor.starts_with("u8\"") {
            2
        } else {
            usize::from(
                cursor.starts_with(['L', 'u', 'U']) && cursor.as_bytes().get(1) == Some(&b'"'),
            )
        };
        let quoted = &cursor[prefix_length..];
        if !quoted.starts_with('"') {
            return None;
        }
        let end = find_closing_quote(quoted, 1)?;
        combined.push_str(&quoted[1..end]);
        cursor = &quoted[end + 1..];
    }
    Some(format!("c\"{combined}\""))
}

fn translate_float_literal(expansion: &str) -> Option<(String, String)> {
    let trimmed = expansion.trim_matches(|character| character == '(' || character == ')');
    if !trimmed.contains(['.', 'e', 'E', 'p', 'P']) {
        return None;
    }
    let is_float = trimmed.ends_with(['f', 'F']);
    let number = trimmed.trim_end_matches(['f', 'F', 'l', 'L']);
    if number.parse::<f64>().is_err() {
        return None;
    }
    Some((
        if is_float { "f32" } else { "f64" }.to_owned(),
        number.to_owned(),
    ))
}

fn infer_macro_integer_type(
    expansion: &str,
    value: i128,
    known_types: &BTreeMap<String, String>,
) -> String {
    for (name, ty) in known_types {
        if expansion.trim_matches(['(', ')', ' ']) == name {
            return ty.clone();
        }
    }
    if let Some(cast) = leading_cast_name(expansion) {
        return match cast {
            "Sint8" => "i8".to_owned(),
            "Uint8" => "u8".to_owned(),
            "Sint16" => "i16".to_owned(),
            "Uint16" => "u16".to_owned(),
            "Sint32" => "i32".to_owned(),
            "Uint32" => "u32".to_owned(),
            "Sint64" => "i64".to_owned(),
            "Uint64" => "u64".to_owned(),
            "size_t" => "usize".to_owned(),
            other if other.starts_with("SDL_") => other.to_owned(),
            _ => default_integer_type(expansion, value),
        };
    }
    default_integer_type(expansion, value)
}

fn leading_cast_name(expansion: &str) -> Option<&str> {
    let trimmed = expansion.trim_start_matches('(').trim_start();
    let end = trimmed.find(')')?;
    let candidate = trimmed[..end].trim();
    (candidate == "size_t"
        || candidate.starts_with("SDL_")
        || candidate.starts_with("Uint")
        || candidate.starts_with("Sint"))
    .then_some(candidate)
}

fn default_integer_type(expansion: &str, value: i128) -> String {
    let uppercase = expansion.to_ascii_uppercase();
    if uppercase.contains("ULL") || uppercase.contains("UINT64") || value > i128::from(u32::MAX) {
        "u64".to_owned()
    } else if uppercase.contains("LL") || value < i128::from(i32::MIN) {
        "i64".to_owned()
    } else if uppercase
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| {
            token.ends_with('U') && token.chars().any(|character| character.is_ascii_digit())
        })
        || value > i128::from(i32::MAX)
    {
        "u32".to_owned()
    } else {
        "i32".to_owned()
    }
}

fn normalize_integer_literal(
    value: i128,
    ty: &str,
    aliases: &BTreeMap<String, CType>,
) -> Option<(String, i128)> {
    if value >= 0 || integer_is_signed(ty, aliases, &mut BTreeSet::new()) != Some(false) {
        return Some((value.to_string(), value));
    }
    let bits = integer_width(ty, aliases, &mut BTreeSet::new())?;
    let modulus = 1_i128.checked_shl(bits)?;
    let normalized = value.rem_euclid(modulus);
    Some((normalized.to_string(), normalized))
}

pub(crate) fn render_integer_literal(
    value: i128,
    ty: &str,
    aliases: &BTreeMap<String, CType>,
) -> Option<String> {
    normalize_integer_literal(value, ty, aliases).map(|(rendered, _)| rendered)
}

fn integer_is_signed(
    ty: &str,
    aliases: &BTreeMap<String, CType>,
    visited: &mut BTreeSet<String>,
) -> Option<bool> {
    match ty {
        "i8" | "i16" | "i32" | "i64" | "isize" => Some(true),
        "u8" | "u16" | "u32" | "u64" | "usize" => Some(false),
        name => integer_property_from_alias(name, aliases, visited, integer_type_is_signed),
    }
}

fn integer_width(
    ty: &str,
    aliases: &BTreeMap<String, CType>,
    visited: &mut BTreeSet<String>,
) -> Option<u32> {
    match ty {
        "i8" | "u8" => Some(8),
        "i16" | "u16" => Some(16),
        "i32" | "u32" => Some(32),
        "i64" | "u64" | "isize" | "usize" => Some(64),
        name => integer_property_from_alias(name, aliases, visited, integer_type_width),
    }
}

fn integer_property_from_alias<T>(
    name: &str,
    aliases: &BTreeMap<String, CType>,
    visited: &mut BTreeSet<String>,
    property: fn(&CType) -> Option<T>,
) -> Option<T> {
    if !visited.insert(name.to_owned()) {
        return None;
    }
    let ty = aliases.get(name)?;
    if let CType::Named(target) = ty {
        if let Some(value) = property(ty) {
            return Some(value);
        }
        return integer_property_from_alias(target, aliases, visited, property);
    }
    property(ty)
}

fn integer_type_is_signed(ty: &CType) -> Option<bool> {
    match ty {
        CType::Char
        | CType::SignedChar
        | CType::Short
        | CType::Int
        | CType::Long
        | CType::LongLong => Some(true),
        CType::Bool
        | CType::UnsignedChar
        | CType::UnsignedShort
        | CType::UnsignedInt
        | CType::UnsignedLong
        | CType::UnsignedLongLong => Some(false),
        CType::Named(name) => match render_named_type(name) {
            "i8" | "i16" | "i32" | "i64" | "isize" => Some(true),
            "u8" | "u16" | "u32" | "u64" | "usize" => Some(false),
            _ => None,
        },
        CType::Unit
        | CType::Float
        | CType::Double
        | CType::Pointer { .. }
        | CType::Array { .. }
        | CType::Function { .. } => None,
    }
}

fn integer_type_width(ty: &CType) -> Option<u32> {
    match ty {
        CType::Bool | CType::Char | CType::SignedChar | CType::UnsignedChar => Some(8),
        CType::Short | CType::UnsignedShort => Some(16),
        CType::Int | CType::UnsignedInt | CType::Long | CType::UnsignedLong => Some(32),
        CType::LongLong | CType::UnsignedLongLong => Some(64),
        CType::Named(name) => match render_named_type(name) {
            "i8" | "u8" => Some(8),
            "i16" | "u16" => Some(16),
            "i32" | "u32" => Some(32),
            "i64" | "u64" | "isize" | "usize" => Some(64),
            _ => None,
        },
        CType::Unit
        | CType::Float
        | CType::Double
        | CType::Pointer { .. }
        | CType::Array { .. }
        | CType::Function { .. } => None,
    }
}

fn filter_sdl_preprocessor_output(source: &str) -> String {
    let mut output = String::from(
        "typedef signed char int8_t;\n\
         typedef unsigned char uint8_t;\n\
         typedef signed short int16_t;\n\
         typedef unsigned short uint16_t;\n\
         typedef signed int int32_t;\n\
         typedef unsigned int uint32_t;\n\
         typedef signed long long int64_t;\n\
         typedef unsigned long long uint64_t;\n\
         typedef long long intptr_t;\n\
         typedef unsigned long long uintptr_t;\n\
         typedef unsigned long long size_t;\n\
         typedef unsigned short wchar_t;\n\
         typedef char *va_list;\n",
    );
    let mut inside_sdl = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#line ") {
            inside_sdl = trimmed.contains("include\\\\SDL3") || trimmed.contains("include/SDL3");
        } else if inside_sdl && !trimmed.starts_with('#') {
            output.push_str(line);
            output.push('\n');
        }
    }
    normalize_msvc_source(&output)
}

fn normalize_msvc_source(source: &str) -> String {
    source
        .replace("__int64", "long long")
        .replace("__cdecl", "")
        .replace("__stdcall", "")
        .replace("__forceinline", "inline")
        .replace("ui64", "ULL")
        .replace("i64", "LL")
        .replace("__declspec(noreturn)", "")
        .replace("__declspec(deprecated)", "")
}

fn is_typedef(declaration: &Declaration) -> bool {
    declaration.specifiers.iter().any(|specifier| {
        matches!(
            specifier.node,
            DeclarationSpecifier::StorageClass(ref storage)
                if storage.node == StorageClassSpecifier::Typedef
        )
    })
}

fn collect_typedef(
    declaration: &Declaration,
    model: &mut HeaderModel,
    enum_values: &mut BTreeMap<String, i128>,
) -> Result<(), HeaderError> {
    let Some(base) = declaration_base(&declaration.specifiers)? else {
        return Ok(());
    };
    for item in &declaration.declarators {
        let declarator = &item.node.declarator.node;
        let Some(name) = declarator_name(declarator) else {
            continue;
        };
        if !is_public_type_name(name) {
            continue;
        }

        match &base.kind {
            BaseKind::Record(record) => {
                let tag = record
                    .identifier
                    .as_ref()
                    .map(|identifier| identifier.node.name.as_str());
                if declarator_has_derived(declarator) {
                    let Some(tag) = tag else {
                        return Err(HeaderError::InvalidDeclaration(format!(
                            "typedef {name}: pointer to anonymous record"
                        )));
                    };
                    collect_record(tag, record, model, enum_values);
                    let ty = apply_declarator(
                        CType::Named(tag.to_owned()),
                        base.is_const,
                        declarator,
                        enum_values,
                    )?;
                    model.aliases.insert(name.to_owned(), ty);
                } else if let Some(tag) = tag
                    && tag != name
                {
                    collect_record(tag, record, model, enum_values);
                    model
                        .aliases
                        .insert(name.to_owned(), CType::Named(tag.to_owned()));
                } else {
                    collect_record(name, record, model, enum_values);
                }
            }
            BaseKind::Enum(enumeration) => {
                model.aliases.insert(name.to_owned(), CType::Int);
                collect_enum(name, enumeration, model, enum_values)?;
            }
            BaseKind::Type(ty) => {
                let ty = apply_declarator(ty.clone(), base.is_const, declarator, enum_values)
                    .map_err(|error| {
                        HeaderError::InvalidDeclaration(format!("typedef {name}: {error}"))
                    })?;
                if matches!(ty, CType::Function { .. }) {
                    model.callbacks.insert(name.to_owned(), ty);
                } else {
                    model.aliases.insert(name.to_owned(), ty);
                }
            }
        }
    }
    Ok(())
}

fn is_public_type_name(name: &str) -> bool {
    (name.starts_with("SDL_") && !name.starts_with("SDL_compile_time_assert"))
        || matches!(
            name,
            "Sint8" | "Uint8" | "Sint16" | "Uint16" | "Sint32" | "Uint32" | "Sint64" | "Uint64"
        )
}

#[derive(Debug, Clone)]
struct BaseType<'a> {
    kind: BaseKind<'a>,
    is_const: bool,
}

#[derive(Debug, Clone)]
enum BaseKind<'a> {
    Type(CType),
    Record(&'a lang_c::ast::StructType),
    Enum(&'a EnumType),
}

fn declaration_base(
    specifiers: &[lang_c::span::Node<DeclarationSpecifier>],
) -> Result<Option<BaseType<'_>>, HeaderError> {
    let mut qualifiers = Vec::new();
    let mut types = Vec::new();
    for specifier in specifiers {
        match &specifier.node {
            DeclarationSpecifier::TypeSpecifier(ty) => types.push(&ty.node),
            DeclarationSpecifier::TypeQualifier(qualifier) => qualifiers.push(&qualifier.node),
            DeclarationSpecifier::StorageClass(_)
            | DeclarationSpecifier::Function(_)
            | DeclarationSpecifier::Alignment(_)
            | DeclarationSpecifier::Extension(_) => {}
        }
    }
    let is_const = qualifiers.contains(&&TypeQualifier::Const);
    let kind = match types.as_slice() {
        [TypeSpecifier::Struct(record)] => BaseKind::Record(&record.node),
        [TypeSpecifier::Enum(enumeration)] => BaseKind::Enum(&enumeration.node),
        _ => BaseKind::Type(primitive_type(&types)?),
    };
    Ok(Some(BaseType { kind, is_const }))
}

fn qualifier_base(
    specifiers: &[lang_c::span::Node<SpecifierQualifier>],
) -> Result<BaseType<'_>, HeaderError> {
    let mut qualifiers = Vec::new();
    let mut types = Vec::new();
    for specifier in specifiers {
        match &specifier.node {
            SpecifierQualifier::TypeSpecifier(ty) => types.push(&ty.node),
            SpecifierQualifier::TypeQualifier(qualifier) => qualifiers.push(&qualifier.node),
            SpecifierQualifier::Extension(_) => {}
        }
    }
    let is_const = qualifiers.contains(&&TypeQualifier::Const);
    let kind = match types.as_slice() {
        [TypeSpecifier::Struct(record)] => BaseKind::Record(&record.node),
        [TypeSpecifier::Enum(enumeration)] => BaseKind::Enum(&enumeration.node),
        _ => BaseKind::Type(primitive_type(&types)?),
    };
    Ok(BaseType { kind, is_const })
}

fn primitive_type(types: &[&TypeSpecifier]) -> Result<CType, HeaderError> {
    if let [TypeSpecifier::TypedefName(name)] = types {
        return Ok(CType::Named(name.node.name.clone()));
    }
    if matches!(types, [TypeSpecifier::Void]) {
        return Ok(CType::Unit);
    }
    if matches!(types, [TypeSpecifier::Bool]) {
        return Ok(CType::Bool);
    }
    if matches!(types, [TypeSpecifier::Float]) {
        return Ok(CType::Float);
    }
    if matches!(types, [TypeSpecifier::Double]) {
        return Ok(CType::Double);
    }

    let unsigned = types.iter().any(|ty| matches!(ty, TypeSpecifier::Unsigned));
    let char_type = types.iter().any(|ty| matches!(ty, TypeSpecifier::Char));
    let short = types.iter().any(|ty| matches!(ty, TypeSpecifier::Short));
    let long_count = types
        .iter()
        .filter(|ty| matches!(ty, TypeSpecifier::Long))
        .count();
    let ty = if char_type {
        if unsigned {
            CType::UnsignedChar
        } else if types.iter().any(|ty| matches!(ty, TypeSpecifier::Signed)) {
            CType::SignedChar
        } else {
            CType::Char
        }
    } else if short {
        if unsigned {
            CType::UnsignedShort
        } else {
            CType::Short
        }
    } else if long_count >= 2 {
        if unsigned {
            CType::UnsignedLongLong
        } else {
            CType::LongLong
        }
    } else if long_count == 1 {
        if unsigned {
            CType::UnsignedLong
        } else {
            CType::Long
        }
    } else if types.iter().any(|ty| {
        matches!(
            ty,
            TypeSpecifier::Int | TypeSpecifier::Signed | TypeSpecifier::Unsigned
        )
    }) {
        if unsigned {
            CType::UnsignedInt
        } else {
            CType::Int
        }
    } else {
        return Err(HeaderError::InvalidDeclaration(format!(
            "unsupported type specifiers {types:?}"
        )));
    };
    Ok(ty)
}

fn declarator_name(declarator: &Declarator) -> Option<&str> {
    match &declarator.kind.node {
        DeclaratorKind::Identifier(identifier) => Some(&identifier.node.name),
        DeclaratorKind::Declarator(nested) => declarator_name(&nested.node),
        DeclaratorKind::Abstract => None,
    }
}

fn declarator_has_derived(declarator: &Declarator) -> bool {
    !declarator.derived.is_empty()
        || match &declarator.kind.node {
            DeclaratorKind::Declarator(nested) => declarator_has_derived(&nested.node),
            DeclaratorKind::Identifier(_) | DeclaratorKind::Abstract => false,
        }
}

fn apply_declarator(
    ty: CType,
    is_const: bool,
    declarator: &Declarator,
    constants: &BTreeMap<String, i128>,
) -> Result<CType, HeaderError> {
    apply_declarator_context(ty, is_const, declarator, false, constants)
}

fn apply_declarator_context(
    mut ty: CType,
    mut is_const: bool,
    declarator: &Declarator,
    parameter_context: bool,
    constants: &BTreeMap<String, i128>,
) -> Result<CType, HeaderError> {
    for part in &declarator.derived {
        match &part.node {
            DerivedDeclarator::Pointer(qualifiers) => {
                if !matches!(ty, CType::Function { .. }) {
                    ty = CType::Pointer {
                        target: Box::new(ty),
                        mutable: !is_const,
                    };
                }
                is_const = qualifiers.iter().any(|qualifier| {
                    matches!(
                        qualifier.node,
                        lang_c::ast::PointerQualifier::TypeQualifier(ref qualifier)
                            if qualifier.node == TypeQualifier::Const
                    )
                });
            }
            DerivedDeclarator::Array(array) => {
                ty = if parameter_context
                    && matches!(
                        array.node.size,
                        ArraySize::Unknown | ArraySize::VariableUnknown
                    ) {
                    CType::Pointer {
                        target: Box::new(ty),
                        mutable: !is_const,
                    }
                } else {
                    let length = array_length(&array.node.size, constants)?;
                    CType::Array {
                        element: Box::new(ty),
                        length,
                    }
                };
                is_const = false;
            }
            DerivedDeclarator::Function(function) => {
                if function.node.ellipsis == Ellipsis::Some {
                    return Err(HeaderError::InvalidDeclaration(
                        "variadic callback type".to_owned(),
                    ));
                }
                let mut parameters = Vec::new();
                for parameter in &function.node.parameters {
                    let Some(base) = declaration_base(&parameter.node.specifiers)? else {
                        continue;
                    };
                    let BaseKind::Type(parameter_type) = base.kind else {
                        return Err(HeaderError::InvalidDeclaration(
                            "inline callback parameter aggregate".to_owned(),
                        ));
                    };
                    let parameter_type = if let Some(declarator) = &parameter.node.declarator {
                        apply_declarator_context(
                            parameter_type,
                            base.is_const,
                            &declarator.node,
                            true,
                            constants,
                        )?
                    } else {
                        parameter_type
                    };
                    parameters.push(parameter_type);
                }
                if parameters == [CType::Unit] {
                    parameters.clear();
                }
                ty = CType::Function {
                    parameters,
                    result: Box::new(ty),
                };
                is_const = false;
            }
            DerivedDeclarator::KRFunction(_) | DerivedDeclarator::Block(_) => {
                return Err(HeaderError::InvalidDeclaration(
                    "unsupported callback declarator".to_owned(),
                ));
            }
        }
    }
    if let DeclaratorKind::Declarator(nested) = &declarator.kind.node {
        return apply_declarator_context(ty, is_const, &nested.node, parameter_context, constants);
    }
    Ok(ty)
}

fn array_length(
    size: &ArraySize,
    constants: &BTreeMap<String, i128>,
) -> Result<usize, HeaderError> {
    let expression = match size {
        ArraySize::VariableExpression(expression) | ArraySize::StaticExpression(expression) => {
            &expression.node
        }
        ArraySize::Unknown | ArraySize::VariableUnknown => {
            return Err(HeaderError::InvalidDeclaration(
                "flexible array member".to_owned(),
            ));
        }
    };
    let value = evaluate_integer(expression, constants)
        .ok_or_else(|| HeaderError::InvalidDeclaration("non-constant array length".to_owned()))?;
    usize::try_from(value)
        .map_err(|_| HeaderError::InvalidDeclaration("invalid array length".to_owned()))
}

fn collect_record(
    name: &str,
    record: &lang_c::ast::StructType,
    model: &mut HeaderModel,
    constants: &BTreeMap<String, i128>,
) {
    if record.kind.node == StructKind::Union {
        model.records.insert(
            name.to_owned(),
            Record {
                fields: None,
                blocker: Some(RecordBlocker::Union),
            },
        );
        return;
    }
    let Some(declarations) = &record.declarations else {
        model.records.entry(name.to_owned()).or_insert(Record {
            fields: None,
            blocker: None,
        });
        return;
    };

    let mut fields = Vec::new();
    let mut blocker = None;
    for declaration in declarations {
        let StructDeclaration::Field(field) = &declaration.node else {
            continue;
        };
        let Ok(base) = qualifier_base(&field.node.specifiers) else {
            blocker.get_or_insert(RecordBlocker::UnsupportedType);
            continue;
        };
        let base_type = match base.kind {
            BaseKind::Type(ty) => ty,
            BaseKind::Record(record) => {
                let Some(identifier) = &record.identifier else {
                    blocker.get_or_insert(RecordBlocker::AnonymousField);
                    continue;
                };
                CType::Named(identifier.node.name.clone())
            }
            BaseKind::Enum(enumeration) => {
                let Some(identifier) = &enumeration.identifier else {
                    blocker.get_or_insert(RecordBlocker::AnonymousField);
                    continue;
                };
                CType::Named(identifier.node.name.clone())
            }
        };
        for item in &field.node.declarators {
            if item.node.bit_width.is_some() {
                blocker.get_or_insert(RecordBlocker::BitField);
                continue;
            }
            let Some(declarator) = &item.node.declarator else {
                blocker.get_or_insert(RecordBlocker::AnonymousField);
                continue;
            };
            let Some(field_name) = declarator_name(&declarator.node) else {
                blocker.get_or_insert(RecordBlocker::AnonymousField);
                continue;
            };
            match apply_declarator(
                base_type.clone(),
                base.is_const,
                &declarator.node,
                constants,
            ) {
                Ok(ty) => fields.push(Field {
                    name: sanitize_field_name(field_name),
                    ty,
                }),
                Err(error) => {
                    let record_blocker = if error.to_string().contains("flexible array") {
                        RecordBlocker::FlexibleArray
                    } else {
                        RecordBlocker::UnsupportedType
                    };
                    blocker.get_or_insert(record_blocker);
                }
            }
        }
    }
    model.records.insert(
        name.to_owned(),
        Record {
            fields: blocker.is_none().then_some(fields),
            blocker,
        },
    );
}

fn sanitize_field_name(name: &str) -> String {
    const KEYWORDS: [&str; 34] = [
        "as", "async", "await", "break", "comptime", "const", "continue", "defer", "else", "enum",
        "extern", "false", "fn", "for", "from", "if", "impl", "import", "in", "let", "loop",
        "match", "move", "mut", "pub", "return", "static", "struct", "trait", "true", "type",
        "unsafe", "where", "while",
    ];
    if KEYWORDS.contains(&name) {
        format!("{name}_value")
    } else {
        name.to_owned()
    }
}

fn collect_enum(
    type_name: &str,
    enumeration: &EnumType,
    model: &mut HeaderModel,
    values: &mut BTreeMap<String, i128>,
) -> Result<(), HeaderError> {
    let mut next_value = 0_i128;
    for enumerator in &enumeration.enumerators {
        let name = &enumerator.node.identifier.node.name;
        let value = if let Some(expression) = &enumerator.node.expression {
            evaluate_integer(&expression.node, values)
                .ok_or_else(|| HeaderError::InvalidEnumExpression(name.clone()))?
        } else {
            next_value
        };
        values.insert(name.clone(), value);
        model.constants.push(EnumConstant {
            name: name.clone(),
            ty: type_name.to_owned(),
            value,
        });
        next_value = value.saturating_add(1);
    }
    Ok(())
}

fn evaluate_integer(expression: &Expression, values: &BTreeMap<String, i128>) -> Option<i128> {
    match expression {
        Expression::Identifier(identifier) => values.get(&identifier.node.name).copied(),
        Expression::Constant(constant) => match &constant.node {
            Constant::Integer(integer) => parse_integer(&integer.number, &integer.base),
            Constant::Character(character) => parse_character(character),
            Constant::Float(_) => None,
        },
        Expression::UnaryOperator(operation) => {
            let value = evaluate_integer(&operation.node.operand.node, values)?;
            match operation.node.operator.node {
                UnaryOperator::Plus => Some(value),
                UnaryOperator::Minus => value.checked_neg(),
                UnaryOperator::Complement => Some(!value),
                UnaryOperator::Negate => Some(i128::from(value == 0)),
                _ => None,
            }
        }
        Expression::BinaryOperator(operation) => {
            let left = evaluate_integer(&operation.node.lhs.node, values)?;
            let right = evaluate_integer(&operation.node.rhs.node, values)?;
            match operation.node.operator.node {
                BinaryOperator::Multiply => left.checked_mul(right),
                BinaryOperator::Divide => left.checked_div(right),
                BinaryOperator::Modulo => left.checked_rem(right),
                BinaryOperator::Plus => left.checked_add(right),
                BinaryOperator::Minus => left.checked_sub(right),
                BinaryOperator::ShiftLeft => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shl(shift)),
                BinaryOperator::ShiftRight => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shr(shift)),
                BinaryOperator::BitwiseAnd => Some(left & right),
                BinaryOperator::BitwiseXor => Some(left ^ right),
                BinaryOperator::BitwiseOr => Some(left | right),
                BinaryOperator::Less => Some(i128::from(left < right)),
                BinaryOperator::Greater => Some(i128::from(left > right)),
                BinaryOperator::LessOrEqual => Some(i128::from(left <= right)),
                BinaryOperator::GreaterOrEqual => Some(i128::from(left >= right)),
                BinaryOperator::Equals => Some(i128::from(left == right)),
                BinaryOperator::NotEquals => Some(i128::from(left != right)),
                BinaryOperator::LogicalAnd => Some(i128::from(left != 0 && right != 0)),
                BinaryOperator::LogicalOr => Some(i128::from(left != 0 || right != 0)),
                _ => None,
            }
        }
        Expression::Cast(cast) => evaluate_integer(&cast.node.expression.node, values),
        Expression::Comma(expressions) => expressions
            .last()
            .and_then(|expression| evaluate_integer(&expression.node, values)),
        _ => None,
    }
}

fn parse_character(source: &str) -> Option<i128> {
    let body = source
        .strip_prefix("u8")
        .or_else(|| source.strip_prefix('u'))
        .or_else(|| source.strip_prefix('U'))
        .or_else(|| source.strip_prefix('L'))
        .unwrap_or(source)
        .strip_prefix('\'')?
        .strip_suffix('\'')?;
    match body {
        "\\0" => Some(0),
        "\\n" => Some(i128::from(b'\n')),
        "\\r" => Some(i128::from(b'\r')),
        "\\t" => Some(i128::from(b'\t')),
        "\\\\" => Some(i128::from(b'\\')),
        "\\'" => Some(i128::from(b'\'')),
        _ => body
            .chars()
            .next()
            .map(|character| i128::from(u32::from(character))),
    }
}

fn parse_integer(number: &str, base: &IntegerBase) -> Option<i128> {
    let (digits, radix) = match base {
        IntegerBase::Decimal => (number, 10),
        IntegerBase::Octal => (number.strip_prefix('0').unwrap_or(number), 8),
        IntegerBase::Hexadecimal => (
            number
                .strip_prefix("0x")
                .or_else(|| number.strip_prefix("0X"))
                .unwrap_or(number),
            16,
        ),
        IntegerBase::Binary => (
            number
                .strip_prefix("0b")
                .or_else(|| number.strip_prefix("0B"))
                .unwrap_or(number),
            2,
        ),
    };
    i128::from_str_radix(if digits.is_empty() { "0" } else { digits }, radix).ok()
}

pub(crate) fn referenced_named_types(model: &HeaderModel) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for ty in model.aliases.values().chain(model.callbacks.values()) {
        collect_named_types(ty, &mut names);
    }
    for record in model.records.values() {
        for field in record.fields.iter().flatten() {
            collect_named_types(&field.ty, &mut names);
        }
    }
    names
}

fn collect_named_types(ty: &CType, output: &mut BTreeSet<String>) {
    match ty {
        CType::Named(name) => {
            if render_named_type(name) == name {
                output.insert(name.clone());
            }
        }
        CType::Pointer { target, .. } => collect_named_types(target, output),
        CType::Array { element, .. } => collect_named_types(element, output),
        CType::Function { parameters, result } => {
            for parameter in parameters {
                collect_named_types(parameter, output);
            }
            collect_named_types(result, output);
        }
        CType::Unit
        | CType::Bool
        | CType::Char
        | CType::SignedChar
        | CType::UnsignedChar
        | CType::Short
        | CType::UnsignedShort
        | CType::Int
        | CType::UnsignedInt
        | CType::Long
        | CType::UnsignedLong
        | CType::LongLong
        | CType::UnsignedLongLong
        | CType::Float
        | CType::Double => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CType, EnumConstant, HeaderModel, apply_declarator, declaration_base, evaluate_integer,
        parse_macro_expansions, render_integer_literal, render_macro_probe,
    };
    use lang_c::ast::{DeclarationSpecifier, ExternalDeclaration};
    use lang_c::driver::{Config, Flavor};
    use std::collections::BTreeMap;

    fn parse_typedef(source: &str) -> (CType, lang_c::ast::Declarator) {
        let config = Config {
            cpp_command: String::new(),
            cpp_options: Vec::new(),
            flavor: Flavor::ClangC11,
        };
        let parsed = lang_c::driver::parse_preprocessed(&config, source.to_owned())
            .expect("fixture should parse");
        let ExternalDeclaration::Declaration(declaration) = &parsed.unit.0[0].node else {
            panic!("fixture should be a declaration");
        };
        assert!(
            declaration
                .node
                .specifiers
                .iter()
                .any(|specifier| matches!(specifier.node, DeclarationSpecifier::StorageClass(_)))
        );
        let base = declaration_base(&declaration.node.specifiers)
            .expect("base should parse")
            .expect("base should exist");
        let super::BaseKind::Type(ty) = base.kind else {
            panic!("fixture should use a scalar base");
        };
        let declarator = declaration.node.declarators[0].node.declarator.node.clone();
        (
            apply_declarator(ty, base.is_const, &declarator, &BTreeMap::new())
                .expect("declarator should translate"),
            declarator,
        )
    }

    #[test]
    fn callback_pointer_should_render_as_function_type() {
        let (ty, _) = parse_typedef("typedef unsigned int (*Callback)(void *, int);");
        assert_eq!(ty.render(), "fn(*mut (), c::Int) -> c::UnsignedInt");
    }

    #[test]
    fn array_of_pointers_should_preserve_shape() {
        let (ty, _) = parse_typedef("typedef const char *Names[4];");
        assert_eq!(ty.render(), "[*const c::Char; 4]");
    }

    #[test]
    fn integer_expression_should_evaluate_bitwise_operations() {
        let config = Config {
            cpp_command: String::new(),
            cpp_options: Vec::new(),
            flavor: Flavor::ClangC11,
        };
        let parsed = lang_c::driver::parse_preprocessed(
            &config,
            "enum Value { FLAG = (1 << 7) | 3 };".to_owned(),
        )
        .expect("fixture should parse");
        let ExternalDeclaration::Declaration(declaration) = &parsed.unit.0[0].node else {
            panic!("fixture should be a declaration");
        };
        let enumeration = declaration
            .node
            .specifiers
            .iter()
            .find_map(|specifier| {
                let DeclarationSpecifier::TypeSpecifier(ty) = &specifier.node else {
                    return None;
                };
                let lang_c::ast::TypeSpecifier::Enum(enumeration) = &ty.node else {
                    return None;
                };
                Some(&enumeration.node)
            })
            .expect("enum should exist");
        let expression = enumeration.enumerators[0]
            .node
            .expression
            .as_ref()
            .expect("constant should be explicit");
        assert_eq!(
            evaluate_integer(&expression.node, &BTreeMap::new()),
            Some(131)
        );
    }

    #[test]
    fn macro_probe_should_only_include_public_object_macros() {
        let source = "#line 1 \"C:\\\\SDL\\\\include\\\\SDL3\\\\SDL_init.h\"\n\
                      #define SDL_INIT_VIDEO 0x20u\n\
                      #define SDL_FUNCTION(value) (value)\n\
                      #define SDL_mixedCase 1\n\
                      #line 1 \"C:\\\\SDK\\\\other.h\"\n\
                      #define SDL_OUTSIDE 2\n";

        let probe = render_macro_probe(source);

        assert!(probe.contains("SDL_BINDING_ENTRY(\"SDL_INIT_VIDEO\""));
        assert!(!probe.contains("SDL_FUNCTION"));
        assert!(!probe.contains("SDL_mixedCase"));
        assert!(!probe.contains("SDL_OUTSIDE"));
    }

    #[test]
    fn macro_expansions_should_preserve_types_and_unsigned_sentinels() {
        let mut aliases = BTreeMap::new();
        aliases.insert("SDL_Flags".to_owned(), CType::UnsignedInt);
        let header = HeaderModel {
            aliases,
            callbacks: BTreeMap::new(),
            records: BTreeMap::new(),
            constants: vec![EnumConstant {
                name: "SDL_EXISTING".to_owned(),
                ty: "SDL_Flags".to_owned(),
                value: 7,
            }],
        };
        let source = "SDL_BINDING_ENTRY(\"SDL_EXISTING\", \"SDL_EXISTING\")\n\
                      SDL_BINDING_ENTRY(\"SDL_INIT_VIDEO\", \"0x00000020u\")\n\
                      SDL_BINDING_ENTRY(\"SDL_ALL_BITS\", \"((Uint32)~0u)\")\n\
                      SDL_BINDING_ENTRY(\"SDL_INVALID_SIZE\", \"((size_t)-1)\")\n\
                      SDL_BINDING_ENTRY(\"SDL_NAME\", \"\\\"example\\\"\")\n\
                      SDL_BINDING_ENTRY(\"SDL_OLD\", \"SDL_OLD_renamed_SDL_EXISTING\")\n";

        let macros = parse_macro_expansions(source, &header);

        assert_eq!(macros.discovered, 5);
        assert_eq!(macros.excluded.len(), 1);
        assert_eq!(macros.excluded[0].name, "SDL_OLD");
        assert_eq!(macros.excluded[0].reason, "renamed_compatibility_alias");
        assert!(macros.blocked.is_empty());
        let constants = macros
            .constants
            .iter()
            .map(|constant| (constant.name.as_str(), constant))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(constants["SDL_INIT_VIDEO"].ty, "u32");
        assert_eq!(constants["SDL_INIT_VIDEO"].value, "32");
        assert_eq!(constants["SDL_ALL_BITS"].value, u32::MAX.to_string());
        assert_eq!(constants["SDL_INVALID_SIZE"].value, u64::MAX.to_string());
        assert_eq!(constants["SDL_NAME"].value, "c\"example\"");
        assert!(!constants.contains_key("SDL_OLD"));
    }

    #[test]
    fn enum_sentinels_should_wrap_to_their_unsigned_alias_width() {
        let aliases = BTreeMap::from([
            ("SDL_MouseID".to_owned(), CType::Named("Uint32".to_owned())),
            ("Uint32".to_owned(), CType::Named("uint32_t".to_owned())),
        ]);

        assert_eq!(
            render_integer_literal(-1, "SDL_MouseID", &aliases),
            Some(u32::MAX.to_string())
        );
    }
}
