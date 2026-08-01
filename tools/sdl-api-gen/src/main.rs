//! Deterministically translates SDL's official dynapi catalog into raw bindings.

mod header;

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use header::{CType, HeaderError, HeaderModel, MacroModel, RecordBlocker, referenced_named_types};

const SDL_VERSION: &str = "3.4.12";
const SDL_COMMIT: &str = "f87239e71e42da91ca317a12eefb82cfbf3393eb";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("SDL binding generation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), GeneratorError> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if let [command, preprocessed_file, output_file] = arguments.as_slice()
        && command == "macro-probe"
    {
        let source =
            fs::read_to_string(preprocessed_file).map_err(|source| GeneratorError::Io {
                operation: "read macro-preserving SDL headers",
                path: PathBuf::from(preprocessed_file),
                source,
            })?;
        let probe = header::render_macro_probe(&source);
        write_generated(Path::new(output_file), &probe)?;
        return Ok(());
    }
    let [
        source_root,
        preprocessed_file,
        macro_expansions_file,
        functions_file,
        types_file,
        constants_file,
        report_file,
    ] = arguments.as_slice()
    else {
        return Err(GeneratorError::Usage);
    };

    let source_root = Path::new(source_root);
    verify_version(source_root)?;
    let catalog_path = source_root
        .join("src")
        .join("dynapi")
        .join("SDL_dynapi_procs.h");
    let catalog = fs::read_to_string(&catalog_path).map_err(|source| GeneratorError::Io {
        operation: "read SDL dynapi catalog",
        path: catalog_path.clone(),
        source,
    })?;
    let preprocessed_path = Path::new(preprocessed_file);
    let preprocessed =
        fs::read_to_string(preprocessed_path).map_err(|source| GeneratorError::Io {
            operation: "read target-preprocessed SDL headers",
            path: preprocessed_path.to_path_buf(),
            source,
        })?;
    let macro_expansions_path = Path::new(macro_expansions_file);
    let macro_expansions =
        fs::read_to_string(macro_expansions_path).map_err(|source| GeneratorError::Io {
            operation: "read expanded SDL macro catalog",
            path: macro_expansions_path.to_path_buf(),
            source,
        })?;
    let procedures = parse_catalog(&catalog)?;
    let header = header::parse_header(&preprocessed).map_err(GeneratorError::Header)?;
    let macros = header::parse_macro_expansions(&macro_expansions, &header);
    let functions = render_bindings(&procedures)?;
    let types = render_types(&header)?;
    let constants = render_constants(&header, &macros)?;
    let report = render_report(&procedures, &header, &macros);

    write_generated(Path::new(functions_file), &functions)?;
    write_generated(Path::new(types_file), &types)?;
    write_generated(Path::new(constants_file), &constants)?;
    write_generated(Path::new(report_file), &report)?;
    Ok(())
}

fn verify_version(source_root: &Path) -> Result<(), GeneratorError> {
    let version_path = source_root
        .join("include")
        .join("SDL3")
        .join("SDL_version.h");
    let source = fs::read_to_string(&version_path).map_err(|source| GeneratorError::Io {
        operation: "read SDL version header",
        path: version_path.clone(),
        source,
    })?;
    let expected = [
        "#define SDL_MAJOR_VERSION   3",
        "#define SDL_MINOR_VERSION   4",
        "#define SDL_MICRO_VERSION   12",
    ];
    if expected.iter().all(|line| source.contains(line)) {
        Ok(())
    } else {
        Err(GeneratorError::VersionMismatch {
            expected: SDL_VERSION,
            path: version_path,
        })
    }
}

#[derive(Debug)]
enum GeneratorError {
    Usage,
    Header(HeaderError),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidCatalog {
        line: usize,
        detail: String,
    },
    VersionMismatch {
        expected: &'static str,
        path: PathBuf,
    },
    UnsupportedType {
        procedure: String,
        declaration: String,
    },
    Formatting(fmt::Error),
}

impl fmt::Display for GeneratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "usage: sdl-api-gen macro-probe <preprocessed.c> <probe.c>\n       sdl-api-gen <SDL source root> <preprocessed.c> <macro-expansions.c> <functions.reim> <types.reim> <constants.reim> <coverage.toml>"
            ),
            Self::Header(source) => write!(formatter, "{source}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "could not {operation} `{}`: {source}",
                path.display()
            ),
            Self::InvalidCatalog { line, detail } => {
                write!(
                    formatter,
                    "invalid dynapi catalog entry on line {line}: {detail}"
                )
            }
            Self::VersionMismatch { expected, path } => write!(
                formatter,
                "SDL source at `{}` is not the pinned {expected} release",
                path.display()
            ),
            Self::UnsupportedType {
                procedure,
                declaration,
            } => write!(
                formatter,
                "unsupported C type `{declaration}` in `{procedure}`"
            ),
            Self::Formatting(source) => {
                write!(formatter, "could not format generated output: {source}")
            }
        }
    }
}

impl Error for GeneratorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Formatting(source) => Some(source),
            Self::Header(source) => Some(source),
            Self::Usage
            | Self::InvalidCatalog { .. }
            | Self::VersionMismatch { .. }
            | Self::UnsupportedType { .. } => None,
        }
    }
}

impl From<fmt::Error> for GeneratorError {
    fn from(source: fmt::Error) -> Self {
        Self::Formatting(source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Procedure {
    name: String,
    return_type: String,
    parameters: Vec<String>,
    blocker: Option<Blocker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blocker {
    Variadic,
    ByValueAggregate,
    PlatformVaList,
}

impl Blocker {
    const fn label(self) -> &'static str {
        match self {
            Self::Variadic => "c_variadic",
            Self::ByValueAggregate => "by_value_aggregate",
            Self::PlatformVaList => "platform_va_list",
        }
    }
}

fn parse_catalog(source: &str) -> Result<Vec<Procedure>, GeneratorError> {
    let mut procedures = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let Some(body) = trimmed
            .strip_prefix("SDL_DYNAPI_PROC(")
            .and_then(|body| body.strip_suffix(')'))
        else {
            continue;
        };
        let fields = split_top_level(body, ',');
        if fields.len() != 5 {
            return Err(GeneratorError::InvalidCatalog {
                line: index + 1,
                detail: format!("expected five macro fields, found {}", fields.len()),
            });
        }
        let raw_parameters =
            strip_parentheses(fields[2]).ok_or_else(|| GeneratorError::InvalidCatalog {
                line: index + 1,
                detail: "parameter list is not parenthesized".to_owned(),
            })?;
        let parameters = if raw_parameters.trim() == "void" || raw_parameters.trim().is_empty() {
            Vec::new()
        } else {
            split_top_level(raw_parameters, ',')
                .into_iter()
                .map(str::trim)
                .map(str::to_owned)
                .collect()
        };
        let return_type = fields[0].trim().to_owned();
        let blocker = classify_blocker(&return_type, &parameters);
        procedures.push(Procedure {
            name: fields[1].trim().to_owned(),
            return_type,
            parameters,
            blocker,
        });
    }
    if procedures.is_empty() {
        return Err(GeneratorError::InvalidCatalog {
            line: 0,
            detail: "no SDL_DYNAPI_PROC entries were found".to_owned(),
        });
    }
    Ok(procedures)
}

fn split_top_level(source: &str, separator: char) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (offset, character) in source.char_indices() {
        match character {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            value if value == separator && depth == 0 => {
                fields.push(&source[start..offset]);
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    fields.push(&source[start..]);
    fields
}

fn strip_parentheses(source: &str) -> Option<&str> {
    source
        .trim()
        .strip_prefix('(')
        .and_then(|body| body.strip_suffix(')'))
}

fn classify_blocker(return_type: &str, parameters: &[String]) -> Option<Blocker> {
    const AGGREGATES: [&str; 2] = ["SDL_GUID", "SDL_FColor"];

    if parameters.iter().any(|parameter| parameter.trim() == "...") {
        return Some(Blocker::Variadic);
    }
    if is_by_value(return_type, "va_list")
        || parameters
            .iter()
            .any(|parameter| is_by_value(parameter, "va_list"))
    {
        return Some(Blocker::PlatformVaList);
    }
    if AGGREGATES.iter().any(|name| {
        is_by_value(return_type, name)
            || parameters
                .iter()
                .any(|parameter| is_by_value(parameter, name))
    }) {
        return Some(Blocker::ByValueAggregate);
    }
    None
}

fn is_by_value(declaration: &str, type_name: &str) -> bool {
    let declaration = strip_annotations(declaration);
    declaration
        .split_whitespace()
        .any(|token| token == type_name)
        && !declaration.contains('*')
}

fn render_bindings(procedures: &[Procedure]) -> Result<String, GeneratorError> {
    let mut output = String::new();
    writeln!(
        output,
        "// Generated from SDL {SDL_VERSION} ({SDL_COMMIT}). Do not edit by hand."
    )?;
    writeln!(
        output,
        "// Run `vendor/sdl3/tools/generate.ps1` to update this file."
    )?;
    writeln!(output)?;
    writeln!(output, "import std::c;")?;
    writeln!(output, "import self::types as types;")?;
    writeln!(output)?;
    writeln!(output, "@link(\"SDL3\")")?;
    writeln!(output, "extern \"C\" {{")?;
    for procedure in procedures
        .iter()
        .filter(|procedure| procedure.blocker.is_none())
    {
        let signature = render_procedure(procedure)?;
        writeln!(output, "{signature}")?;
    }
    writeln!(output, "}}")?;
    Ok(output)
}

fn render_types(model: &HeaderModel) -> Result<String, GeneratorError> {
    let mut output = String::new();
    writeln!(
        output,
        "// Generated from SDL {SDL_VERSION} ({SDL_COMMIT}). Do not edit by hand."
    )?;
    writeln!(
        output,
        "// Run `vendor/sdl3/tools/generate.ps1` to update this file."
    )?;
    writeln!(output)?;
    writeln!(output, "import std::c;")?;
    writeln!(output)?;

    for (name, ty) in &model.aliases {
        writeln!(output, "pub type {name} = {};", ty.render())?;
    }
    writeln!(output, "pub type wchar_t = u16;")?;
    let mut defined = model
        .aliases
        .keys()
        .chain(model.callbacks.keys())
        .chain(model.records.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    defined.insert("wchar_t".to_owned());
    for (name, ty) in [
        ("VkInstance", "*mut ()"),
        ("VkPhysicalDevice", "*mut ()"),
        ("VkSurfaceKHR", "u64"),
        ("XTaskQueueHandle", "*mut ()"),
        ("XUserHandle", "*mut ()"),
    ] {
        if defined.insert(name.to_owned()) {
            writeln!(output, "pub type {name} = {ty};")?;
        }
    }
    writeln!(output)?;

    for (name, ty) in &model.callbacks {
        writeln!(output, "pub type {name} = {};", ty.render())?;
    }
    for (name, ty) in [
        (
            "SDL_RequestAndroidPermissionCallback",
            "fn(*mut (), *const c::Char, bool) -> ()",
        ),
        ("SDL_iOSAnimationCallback", "fn(*mut ()) -> ()"),
        ("SDL_main_func", "fn(c::Int, *mut *mut c::Char) -> c::Int"),
    ] {
        if defined.insert(name.to_owned()) {
            writeln!(output, "pub type {name} = {ty};")?;
        }
    }
    writeln!(output)?;

    let referenced = referenced_named_types(model);
    let missing = referenced
        .iter()
        .filter(|name| !defined.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for name in missing {
        match name.as_str() {
            "VkInstance" | "VkPhysicalDevice" | "XTaskQueueHandle" | "XUserHandle" => {
                writeln!(output, "pub type {name} = *mut ();")?;
            }
            "VkSurfaceKHR" => writeln!(output, "pub type {name} = u64;")?,
            "va_list" => writeln!(output, "pub type {name} = *mut c::Char;")?,
            _ => render_opaque_record(&mut output, &name)?,
        }
        defined.insert(name.clone());
    }
    for name in ["JavaVM", "VkAllocationCallbacks"] {
        if defined.insert(name.to_owned()) {
            render_opaque_record(&mut output, name)?;
        }
    }
    if referenced
        .iter()
        .any(|name| !defined.contains(name.as_str()))
    {
        writeln!(output)?;
    }

    let blockers = propagated_record_blockers(model);
    for (name, record) in &model.records {
        if name == "SDL_Event" {
            render_event_storage(&mut output)?;
            continue;
        }
        if blockers.contains_key(name) || record.fields.is_none() {
            render_opaque_record(&mut output, name)?;
            continue;
        }
        writeln!(output, "@repr(C)")?;
        writeln!(output, "pub struct {name} {{")?;
        for field in record.fields.iter().flatten() {
            writeln!(output, "    pub {}: {},", field.name, field.ty.render())?;
        }
        writeln!(output, "}}")?;
        writeln!(output)?;
    }
    Ok(output)
}

fn render_opaque_record(output: &mut String, name: &str) -> Result<(), fmt::Error> {
    writeln!(output, "@repr(C)")?;
    writeln!(output, "pub struct {name} {{")?;
    writeln!(output, "    private_byte: u8,")?;
    writeln!(output, "}}")?;
    writeln!(output)
}

fn render_event_storage(output: &mut String) -> Result<(), fmt::Error> {
    writeln!(
        output,
        "/// Storage-compatible SDL event value. Use typed safe event views when available."
    )?;
    writeln!(output, "@derive(Default)")?;
    writeln!(output, "@repr(C)")?;
    writeln!(output, "@align(8)")?;
    writeln!(output, "pub struct SDL_Event {{")?;
    writeln!(output, "    pub kind: u32,")?;
    writeln!(output, "    pub reserved: u32,")?;
    writeln!(output, "    private_storage: [u64; 15],")?;
    writeln!(output, "}}")?;
    writeln!(output)?;
    writeln!(output, "comptime {{")?;
    writeln!(output, "    assert(size_of<SDL_Event>() == 128);")?;
    writeln!(output, "    assert(align_of<SDL_Event>() == 8);")?;
    writeln!(output, "}}")?;
    writeln!(output)
}

fn propagated_record_blockers(
    model: &HeaderModel,
) -> std::collections::BTreeMap<String, RecordBlocker> {
    let mut blockers = model
        .records
        .iter()
        .filter_map(|(name, record)| record.blocker.map(|blocker| (name.clone(), blocker)))
        .collect::<std::collections::BTreeMap<_, _>>();
    loop {
        let mut changed = false;
        for (name, record) in &model.records {
            if blockers.contains_key(name) {
                continue;
            }
            let blocked_dependency = record
                .fields
                .iter()
                .flatten()
                .any(|field| has_blocked_value_dependency(&field.ty, &blockers, false));
            if blocked_dependency {
                blockers.insert(name.clone(), RecordBlocker::UnsupportedType);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    blockers
}

fn has_blocked_value_dependency(
    ty: &CType,
    blockers: &std::collections::BTreeMap<String, RecordBlocker>,
    behind_pointer: bool,
) -> bool {
    match ty {
        CType::Named(name) => !behind_pointer && blockers.contains_key(name),
        CType::Pointer { target, .. } => has_blocked_value_dependency(target, blockers, true),
        CType::Array { element, .. } => {
            has_blocked_value_dependency(element, blockers, behind_pointer)
        }
        CType::Function { .. }
        | CType::Unit
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
        | CType::Double => false,
    }
}

fn render_constants(model: &HeaderModel, macros: &MacroModel) -> Result<String, GeneratorError> {
    let mut output = String::new();
    writeln!(
        output,
        "// Generated from SDL {SDL_VERSION} ({SDL_COMMIT}). Do not edit by hand."
    )?;
    writeln!(
        output,
        "// Run `vendor/sdl3/tools/generate.ps1` to update this file."
    )?;
    writeln!(output)?;
    writeln!(output, "import self::types as types;")?;
    writeln!(output)?;
    for constant in &model.constants {
        let value = header::render_integer_literal(constant.value, &constant.ty, &model.aliases)
            .unwrap_or_else(|| constant.value.to_string());
        writeln!(
            output,
            "pub const {}: types::{} = {};",
            constant.name, constant.ty, value
        )?;
    }
    for constant in &macros.constants {
        let ty = if constant.ty.starts_with("SDL_") {
            format!("types::{}", constant.ty)
        } else {
            constant.ty.clone()
        };
        writeln!(
            output,
            "pub const {}: {ty} = {};",
            constant.name, constant.value
        )?;
    }
    Ok(output)
}

fn render_procedure(procedure: &Procedure) -> Result<String, GeneratorError> {
    let mut rendered = String::new();
    write!(rendered, "    pub fn {}(", procedure.name)?;
    if procedure.parameters.len() <= 2 {
        for (index, declaration) in procedure.parameters.iter().enumerate() {
            if index != 0 {
                rendered.push_str(", ");
            }
            let (name, ty) = render_parameter(&procedure.name, declaration, index)?;
            write!(rendered, "{name}: {ty}")?;
        }
        rendered.push(')');
    } else {
        rendered.push('\n');
        for (index, declaration) in procedure.parameters.iter().enumerate() {
            let (name, ty) = render_parameter(&procedure.name, declaration, index)?;
            writeln!(rendered, "        {name}: {ty},")?;
        }
        rendered.push_str("    )");
    }
    let return_type = translate_type(&procedure.name, &procedure.return_type)?;
    if return_type != "()" {
        write!(rendered, " -> {return_type}")?;
    }
    rendered.push(';');
    Ok(rendered)
}

fn render_parameter(
    procedure: &str,
    declaration: &str,
    index: usize,
) -> Result<(String, String), GeneratorError> {
    let cleaned = strip_annotations(declaration);
    let (type_declaration, array_depth) =
        remove_parameter_name(&cleaned).ok_or_else(|| GeneratorError::UnsupportedType {
            procedure: procedure.to_owned(),
            declaration: declaration.to_owned(),
        })?;
    let mut ty = translate_type(procedure, type_declaration)?;
    for _ in 0..array_depth {
        ty = format!("*mut {ty}");
    }
    Ok((format!("argument_{index}"), ty))
}

fn remove_parameter_name(declaration: &str) -> Option<(&str, usize)> {
    let trimmed = declaration.trim();
    let array_depth = usize::from(trimmed.ends_with("[]"));
    let without_array = trimmed.strip_suffix("[]").unwrap_or(trimmed).trim_end();
    let name_start = without_array
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_ascii_alphanumeric() || *character == '_')
        .last()
        .map(|(offset, _)| offset)?;
    let name = &without_array[name_start..];
    if name.is_empty()
        || !name.starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
    {
        return None;
    }
    let ty = without_array[..name_start].trim_end();
    (!ty.is_empty()).then_some((ty, array_depth))
}

fn strip_annotations(declaration: &str) -> String {
    const MARKERS: [&str; 2] = ["SDL_PRINTF_FORMAT_STRING", "SDL_SCANF_FORMAT_STRING"];
    let mut output = declaration.to_owned();
    for marker in MARKERS {
        output = output.replace(marker, "");
    }
    for prefix in ["SDL_IN", "SDL_OUT"] {
        while let Some(start) = output.find(prefix) {
            let Some(relative_end) = output[start..].find(')') else {
                break;
            };
            output.replace_range(start..=start + relative_end, "");
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn translate_type(procedure: &str, declaration: &str) -> Result<String, GeneratorError> {
    let cleaned = strip_annotations(declaration)
        .replace("struct ", "")
        .replace("enum ", "");
    let segments = cleaned.split('*').map(str::trim).collect::<Vec<_>>();
    let base_segment = segments.first().copied().unwrap_or_default();
    let base_name = base_segment
        .split_whitespace()
        .filter(|token| *token != "const" && *token != "volatile")
        .collect::<Vec<_>>()
        .join(" ");
    let mut translated =
        translate_base(&base_name).ok_or_else(|| GeneratorError::UnsupportedType {
            procedure: procedure.to_owned(),
            declaration: declaration.to_owned(),
        })?;
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        let immutable = segment.split_whitespace().any(|token| token == "const");
        translated = if immutable {
            format!("*const {translated}")
        } else {
            format!("*mut {translated}")
        };
    }
    Ok(translated)
}

fn translate_base(base: &str) -> Option<String> {
    let primitive = match base {
        "void" => "()",
        "bool" => "bool",
        "char" => "c::Char",
        "signed char" => "c::SignedChar",
        "unsigned char" => "c::UnsignedChar",
        "short" | "short int" | "signed short" | "signed short int" => "c::Short",
        "unsigned short" | "unsigned short int" => "c::UnsignedShort",
        "int" | "signed" | "signed int" => "c::Int",
        "unsigned" | "unsigned int" => "c::UnsignedInt",
        "long" | "long int" | "signed long" | "signed long int" => "c::Long",
        "unsigned long" | "unsigned long int" => "c::UnsignedLong",
        "long long" | "long long int" | "signed long long" | "signed long long int" => {
            "c::LongLong"
        }
        "unsigned long long" | "unsigned long long int" => "c::UnsignedLongLong",
        "float" => "f32",
        "double" => "f64",
        "size_t" => "usize",
        "Sint8" | "int8_t" => "i8",
        "Uint8" | "uint8_t" => "u8",
        "Sint16" | "int16_t" => "i16",
        "Uint16" | "uint16_t" => "u16",
        "Sint32" | "int32_t" => "i32",
        "Uint32" | "uint32_t" => "u32",
        "Sint64" | "int64_t" => "i64",
        "Uint64" | "uint64_t" => "u64",
        _ if is_c_identifier(base) => return Some(format!("types::{base}")),
        _ => return None,
    };
    Some(primitive.to_owned())
}

fn is_c_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn render_report(procedures: &[Procedure], model: &HeaderModel, macros: &MacroModel) -> String {
    let generated = procedures
        .iter()
        .filter(|procedure| procedure.blocker.is_none())
        .count();
    let record_blockers = propagated_record_blockers(model);
    let concrete_records = model
        .records
        .iter()
        .filter(|(name, record)| {
            name.as_str() == "SDL_Event"
                || (record.fields.is_some() && !record_blockers.contains_key(name.as_str()))
        })
        .count();
    let opaque_records = model.records.len().saturating_sub(concrete_records);
    let mut report = format!(
        "# Generated by sdl-api-gen. Do not edit by hand.\n\
         sdl_version = \"{SDL_VERSION}\"\n\
         source_commit = \"{SDL_COMMIT}\"\n\
         catalog_functions = {}\n\
         generated_functions = {generated}\n\
         blocked_functions = {}\n\
         type_aliases = {}\n\
         callback_types = {}\n\
         enum_constants = {}\n\
         discovered_object_macros = {}\n\
         generated_object_macros = {}\n\
         excluded_object_macros = {}\n\
         blocked_object_macros = {}\n\
         concrete_records = {concrete_records}\n\
         opaque_records = {opaque_records}\n",
        procedures.len(),
        procedures.len() - generated,
        model.aliases.len(),
        model.callbacks.len(),
        model.constants.len(),
        macros.discovered,
        macros.constants.len(),
        macros.excluded.len(),
        macros.blocked.len(),
    );
    for procedure in procedures
        .iter()
        .filter(|procedure| procedure.blocker.is_some())
    {
        let blocker = procedure.blocker.map_or("unknown", Blocker::label);
        report.push_str("\n[[blocked]]\n");
        let _ = writeln!(report, "name = \"{}\"", procedure.name);
        let _ = writeln!(report, "reason = \"{blocker}\"");
    }
    for (name, blocker) in record_blockers {
        report.push_str("\n[[opaque_record]]\n");
        let _ = writeln!(report, "name = \"{name}\"");
        let _ = writeln!(report, "reason = \"{}\"", blocker.label());
    }
    for name in &macros.blocked {
        report.push_str("\n[[blocked_macro]]\n");
        let _ = writeln!(report, "name = \"{name}\"");
        let _ = writeln!(report, "reason = \"unsupported_constant_expression\"");
    }
    for exclusion in &macros.excluded {
        report.push_str("\n[[excluded_macro]]\n");
        let _ = writeln!(report, "name = \"{}\"", exclusion.name);
        let _ = writeln!(report, "reason = \"{}\"", exclusion.reason);
    }
    report
}

fn write_generated(path: &Path, contents: &str) -> Result<(), GeneratorError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| GeneratorError::Io {
            operation: "create generated output directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let contents = format!("{}\n", contents.trim_end());
    fs::write(path, contents).map_err(|source| GeneratorError::Io {
        operation: "write generated output",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{Blocker, parse_catalog, render_procedure, split_top_level, translate_type};

    #[test]
    fn split_top_level_should_preserve_nested_parameter_lists() {
        let fields = split_top_level("int,name,(int a, fn(void) b),(a,b),return", ',');
        assert_eq!(
            fields,
            ["int", "name", "(int a, fn(void) b)", "(a,b)", "return"]
        );
    }

    #[test]
    fn parse_catalog_should_classify_unsupported_abi_entries() {
        let source = "\
SDL_DYNAPI_PROC(bool,SDL_Load,(const char *a),(a),return)\n\
SDL_DYNAPI_PROC(void,SDL_Log,(const char *a,...),(a),)\n\
SDL_DYNAPI_PROC(SDL_GUID,SDL_GetGuid,(void),(),return)";
        let procedures = parse_catalog(source).expect("fixture should parse");
        assert_eq!(procedures.len(), 3);
        assert_eq!(procedures[0].blocker, None);
        assert_eq!(procedures[1].blocker, Some(Blocker::Variadic));
        assert_eq!(procedures[2].blocker, Some(Blocker::ByValueAggregate));
    }

    #[test]
    fn translate_type_should_preserve_pointer_constness() {
        assert_eq!(
            translate_type("fixture", "const char *const *").expect("type should translate"),
            "*const *const c::Char"
        );
        assert_eq!(
            translate_type("fixture", "SDL_Window **").expect("type should translate"),
            "*mut *mut types::SDL_Window"
        );
    }

    #[test]
    fn render_procedure_should_emit_readable_multiline_signatures() {
        let procedures = parse_catalog(
            "SDL_DYNAPI_PROC(bool,SDL_Create,(SDL_Window *a, const char *b, Uint32 c),(a,b,c),return)",
        )
        .expect("fixture should parse");
        let rendered = render_procedure(&procedures[0]).expect("procedure should render");
        assert!(rendered.contains("pub fn SDL_Create("));
        assert!(rendered.contains("argument_0: *mut types::SDL_Window"));
        assert!(rendered.ends_with(" -> bool;"));
    }
}
