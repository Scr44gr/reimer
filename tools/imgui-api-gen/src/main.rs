//! Translates Dear Bindings metadata into documented Reimer declarations.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Map, Value};

const IMGUI_VERSION: &str = "1.92.8";
const DEAR_BINDINGS_VERSION: &str = "0.21";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Dear ImGui binding generation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), GeneratorError> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let [
        core_metadata,
        sdl_metadata,
        opengl_metadata,
        types_file,
        constants_file,
        functions_file,
        backends_file,
        report_file,
    ] = arguments.as_slice()
    else {
        return Err(GeneratorError::Usage);
    };

    let core = read_metadata(Path::new(core_metadata))?;
    let sdl = read_metadata(Path::new(sdl_metadata))?;
    let opengl = read_metadata(Path::new(opengl_metadata))?;
    verify_versions(&core)?;

    let types = render_types(&core)?;
    let constants = render_constants(&core)?;
    let (functions, function_coverage) = render_functions(&core, FunctionDomain::Core)?;
    let (backends, backend_coverage) = render_backends(&sdl, &opengl)?;
    let report = render_report(&function_coverage, &backend_coverage);

    write_generated(Path::new(types_file), &types)?;
    write_generated(Path::new(constants_file), &constants)?;
    write_generated(Path::new(functions_file), &functions)?;
    write_generated(Path::new(backends_file), &backends)?;
    write_generated(Path::new(report_file), &report)?;
    Ok(())
}

fn read_metadata(path: &Path) -> Result<Value, GeneratorError> {
    let source = fs::read_to_string(path).map_err(|source| GeneratorError::Io {
        operation: "read Dear Bindings metadata",
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&source).map_err(|source| GeneratorError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn verify_versions(metadata: &Value) -> Result<(), GeneratorError> {
    let defines = array(metadata, "defines")?;
    let version = defines
        .iter()
        .find(|define| string(define, "name") == Some("IMGUI_VERSION"))
        .and_then(|define| string(define, "content"))
        .map(|value| value.trim_matches('"'));
    if version == Some(IMGUI_VERSION) {
        Ok(())
    } else {
        Err(GeneratorError::VersionMismatch {
            expected: IMGUI_VERSION,
            actual: version.unwrap_or("missing").to_owned(),
        })
    }
}

fn render_types(metadata: &Value) -> Result<String, GeneratorError> {
    let mut output = generated_header("raw types and callback aliases");
    writeln!(output, "import std::c;\n")?;

    let structs = array(metadata, "structs")?;
    let struct_names = structs
        .iter()
        .filter_map(|record| string(record, "name"))
        .filter(|name| !name.starts_with("__anonymous"))
        .collect::<BTreeSet<_>>();
    let mut aliases = BTreeSet::new();

    for alias in array(metadata, "typedefs")? {
        if boolean(alias, "is_internal")
            || string(alias, "name") == Some("ImWchar") && aliases.contains("ImWchar")
        {
            continue;
        }
        let name = required_string(alias, "name")?;
        if struct_names.contains(name) {
            continue;
        }
        let type_value = object(alias, "type")?;
        let rendered = if type_value
            .get("type_details")
            .and_then(Value::as_object)
            .and_then(|details| string_map(details, "flavour"))
            == Some("function_pointer")
        {
            render_callback(type_value, TypeDomain::Core)?
        } else {
            render_type(object_map(type_value, "description")?, TypeDomain::Core)?
        };
        write_docs(
            &mut output,
            alias,
            &format!("Dear ImGui type alias `{name}`."),
        )?;
        writeln!(output, "pub type {name} = {rendered};\n")?;
        aliases.insert(name);
    }

    for (name, rendered, documentation) in [
        (
            "ImGuiSelectionRequestType",
            "c::Int",
            "Selection request operation kind.",
        ),
        (
            "ImTextureFormat",
            "c::Int",
            "Dear ImGui texture pixel format.",
        ),
        (
            "ImTextureStatus",
            "c::Int",
            "Dear ImGui texture upload state.",
        ),
        (
            "va_list",
            "*mut ()",
            "Opaque platform variadic argument-list storage.",
        ),
    ] {
        if aliases.insert(name) {
            writeln!(
                output,
                "/// {documentation}\npub type {name} = {rendered};\n"
            )?;
        }
    }

    if aliases.insert("ImStr") {
        writeln!(
            output,
            "/// Opaque Dear ImGui internal string view returned by raw helpers.\n@repr(C)\npub struct ImStr {{\n    private_byte: u8,\n}}\n"
        )?;
    }

    render_records(&mut output, structs)?;

    writeln!(
        output,
        "comptime {{\n    assert(size_of<ImVec2>() == 8);\n    assert(align_of<ImVec2>() == 4);\n    assert(size_of<ImVec4>() == 16);\n    assert(align_of<ImVec4>() == 4);\n}}"
    )?;
    Ok(output)
}

fn render_records(output: &mut String, records: &[Value]) -> Result<(), GeneratorError> {
    for record in records {
        let name = required_string(record, "name")?;
        if name.starts_with("__anonymous") {
            continue;
        }
        write_docs(
            output,
            record,
            &format!("C-compatible storage for Dear ImGui's `{name}` type."),
        )?;
        writeln!(output, "@repr(C)")?;
        if boolean(record, "by_value") {
            writeln!(output, "pub struct {name} {{")?;
            for field in array(record, "fields")? {
                let field_name = sanitize_identifier(required_string(field, "name")?);
                let rendered = render_type(
                    object_map(object(field, "type")?, "description")?,
                    TypeDomain::Core,
                )?;
                write_docs_indented(
                    output,
                    field,
                    &format!("Native `{}` field.", required_string(field, "name")?),
                )?;
                writeln!(output, "    pub {field_name}: {rendered},")?;
            }
            writeln!(output, "}}\n")?;
        } else {
            writeln!(output, "pub struct {name} {{\n    private_byte: u8,\n}}\n")?;
        }
    }
    Ok(())
}

fn render_callback(
    type_value: &Map<String, Value>,
    domain: TypeDomain,
) -> Result<String, GeneratorError> {
    let details = object_map(type_value, "type_details")?;
    let return_type = render_type(
        object_map(object_map(details, "return_type")?, "description")?,
        domain,
    )?;
    let mut parameters = Vec::new();
    for argument in array_map(details, "arguments")? {
        if boolean(argument, "is_varargs") {
            return Err(GeneratorError::UnsupportedType(
                "variadic callback".to_owned(),
            ));
        }
        parameters.push(render_type(
            object_map(object(argument, "type")?, "description")?,
            domain,
        )?);
    }
    Ok(format!("fn({}) -> {return_type}", parameters.join(", ")))
}

fn render_constants(metadata: &Value) -> Result<String, GeneratorError> {
    let mut output = generated_header("raw constants");
    writeln!(output, "import std::c;\n")?;
    writeln!(
        output,
        "/// Dear ImGui semantic version used to build this package."
    )?;
    writeln!(
        output,
        "pub const IMGUI_VERSION: cstr = c\"{IMGUI_VERSION}\";"
    )?;
    writeln!(
        output,
        "/// Numeric Dear ImGui version used to build this package."
    )?;
    writeln!(output, "pub const IMGUI_VERSION_NUM: c::Int = 19280;\n")?;

    let mut names = BTreeSet::new();
    for enumeration in array(metadata, "enums")? {
        if boolean(enumeration, "is_internal") {
            continue;
        }
        let enum_name = required_string(enumeration, "name")?;
        for element in array(enumeration, "elements")? {
            let name = required_string(element, "name")?;
            if boolean(element, "is_internal") || !names.insert(name) {
                continue;
            }
            write_docs(
                &mut output,
                element,
                &format!("`{name}` value from Dear ImGui's `{enum_name}` catalog."),
            )?;
            let value = element
                .get("value")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    GeneratorError::InvalidMetadata(format!(
                        "constant `{name}` has no integer value"
                    ))
                })?;
            writeln!(output, "pub const {name}: c::Int = {value};\n")?;
        }
    }
    Ok(output)
}

fn render_functions(
    metadata: &Value,
    domain: FunctionDomain,
) -> Result<(String, Coverage), GeneratorError> {
    let mut output = generated_header("raw functions");
    writeln!(output, "import std::c;")?;
    writeln!(output, "import self::types as types;\n")?;
    writeln!(output, "@link(\"imgui\")\nextern \"C\" {{")?;
    let mut coverage = Coverage::default();

    for function in array(metadata, "functions")? {
        if boolean(function, "is_internal") {
            coverage.internal += 1;
            continue;
        }
        if condition_mentions(function, "IMGUI_HAS_IMSTR") {
            coverage.conditionally_unavailable += 1;
            continue;
        }
        if array(function, "arguments")?
            .iter()
            .any(|argument| boolean(argument, "is_varargs"))
        {
            coverage.variadic += 1;
            continue;
        }
        match render_function(function, domain) {
            Ok(rendered) => {
                output.push_str(&rendered);
                coverage.generated += 1;
            }
            Err(GeneratorError::UnsupportedType(reason)) => {
                coverage
                    .unsupported
                    .push(format!("{}: {reason}", required_string(function, "name")?));
            }
            Err(error) => return Err(error),
        }
    }
    writeln!(output, "}}")?;
    Ok((output, coverage))
}

fn render_function(function: &Value, domain: FunctionDomain) -> Result<String, GeneratorError> {
    let name = required_string(function, "name")?;
    let original = string(function, "original_fully_qualified_name").unwrap_or(name);
    let mut output = String::new();
    write_docs(
        &mut output,
        function,
        &format!("Calls Dear ImGui's `{original}` operation through the generated C ABI."),
    )?;
    write!(output, "    pub fn {name}(")?;
    let type_domain = match domain {
        FunctionDomain::Core => TypeDomain::CoreQualified,
        FunctionDomain::Backend => TypeDomain::Backend,
    };
    let mut arguments = Vec::new();
    for argument in array(function, "arguments")? {
        let argument_name = sanitize_identifier(required_string(argument, "name")?);
        let description = object_map(object(argument, "type")?, "description")?;
        let rendered = if boolean(argument, "is_array") {
            render_array_parameter(description, type_domain)?
        } else {
            render_type(description, type_domain)?
        };
        arguments.push(format!("{argument_name}: {rendered}"));
    }
    write!(output, "{}", arguments.join(", "))?;
    let return_type = render_type(
        object_map(object(function, "return_type")?, "description")?,
        type_domain,
    )?;
    if return_type == "()" {
        writeln!(output, ");\n")?;
    } else {
        writeln!(output, ") -> {return_type};\n")?;
    }
    Ok(output)
}

fn render_backends(sdl: &Value, opengl: &Value) -> Result<(String, Coverage), GeneratorError> {
    let mut output = generated_header("SDL3, OpenGL3, and wgpu backend functions");
    writeln!(output, "import std::c;")?;
    writeln!(output, "import sdl3::raw::types as sdl_types;")?;
    writeln!(output, "import wgpu::raw::types as wgpu_types;")?;
    writeln!(output, "import self::types as types;\n")?;

    for enumeration in array(sdl, "enums")? {
        let name = required_string(enumeration, "name")?;
        write_docs(
            &mut output,
            enumeration,
            &format!("Dear ImGui SDL3 backend `{name}` mode."),
        )?;
        writeln!(output, "pub type {name} = c::Int;\n")?;
        for element in array(enumeration, "elements")? {
            let element_name = required_string(element, "name")?;
            let value = element
                .get("value")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    GeneratorError::InvalidMetadata(format!(
                        "backend constant `{element_name}` has no value"
                    ))
                })?;
            write_docs(
                &mut output,
                element,
                &format!("`{element_name}` mode for the Dear ImGui SDL3 backend."),
            )?;
            writeln!(output, "pub const {element_name}: {name} = {value};\n")?;
        }
    }

    writeln!(output, "@link(\"imgui\")\nextern \"C\" {{")?;
    let mut coverage = Coverage::default();
    for metadata in [sdl, opengl] {
        for function in array(metadata, "functions")? {
            match render_function(function, FunctionDomain::Backend) {
                Ok(rendered) => {
                    output.push_str(&rendered);
                    coverage.generated += 1;
                }
                Err(GeneratorError::UnsupportedType(reason)) => {
                    coverage
                        .unsupported
                        .push(format!("{}: {reason}", required_string(function, "name")?));
                }
                Err(error) => return Err(error),
            }
        }
    }
    coverage.generated += render_wgpu_bridge(&mut output)?;
    writeln!(output, "}}")?;
    Ok((output, coverage))
}

fn render_wgpu_bridge(output: &mut String) -> Result<usize, fmt::Error> {
    writeln!(
        output,
        "    /// Initializes the official Dear ImGui wgpu renderer backend.\n    pub fn imgui_bridge_wgpu_init(device: wgpu_types::WGPUDevice, render_target_format: wgpu_types::WGPUTextureFormat, frames_in_flight: c::Int) -> bool;\n\n    /// Releases renderer resources owned by the Dear ImGui wgpu backend.\n    pub fn imgui_bridge_wgpu_shutdown();\n\n    /// Prepares the Dear ImGui wgpu renderer backend for a new frame.\n    pub fn imgui_bridge_wgpu_new_frame();\n\n    /// Records Dear ImGui draw data into a borrowed active wgpu render pass.\n    pub fn imgui_bridge_wgpu_render_draw_data(draw_data: *mut types::ImDrawData, pass: wgpu_types::WGPURenderPassEncoder);\n\n    /// Draws a length-delimited UTF-8 string without treating it as a format string.\n    pub fn imgui_bridge_text_unformatted(value: *const u8, byte_length: usize);\n\n    /// Reports whether Dear ImGui wants to consume mouse input this frame.\n    pub fn imgui_bridge_wants_mouse() -> bool;\n\n    /// Reports whether Dear ImGui wants to consume keyboard input this frame.\n    pub fn imgui_bridge_wants_keyboard() -> bool;\n\n    /// Draws a UTF-8 label followed by one signed integer.\n    pub fn imgui_bridge_text_i64(label: *const u8, byte_length: usize, value: i64);\n\n    /// Draws a UTF-8 label followed by one floating-point value.\n    pub fn imgui_bridge_text_f64(label: *const u8, byte_length: usize, value: f64);\n"
    )?;
    Ok(9)
}

fn render_array_parameter(
    description: &Map<String, Value>,
    domain: TypeDomain,
) -> Result<String, GeneratorError> {
    if string_map(description, "kind") != Some("Array") {
        return render_type(description, domain);
    }
    let inner = object_map(description, "inner_type")?;
    let mutability = if has_const_storage(inner) {
        "const"
    } else {
        "mut"
    };
    Ok(format!("*{mutability} {}", render_type(inner, domain)?))
}

fn render_type(
    description: &Map<String, Value>,
    domain: TypeDomain,
) -> Result<String, GeneratorError> {
    match string_map(description, "kind") {
        Some("Builtin") => match string_map(description, "builtin_type") {
            Some("void") => Ok("()".to_owned()),
            Some("bool") => Ok("bool".to_owned()),
            Some("char") => Ok("c::Char".to_owned()),
            Some("unsigned_char") => Ok("u8".to_owned()),
            Some("short") => Ok("i16".to_owned()),
            Some("unsigned_short") => Ok("u16".to_owned()),
            Some("int") => Ok("c::Int".to_owned()),
            Some("unsigned_int") => Ok("c::UnsignedInt".to_owned()),
            Some("long_long") => Ok("i64".to_owned()),
            Some("unsigned_long_long") => Ok("u64".to_owned()),
            Some("float") => Ok("f32".to_owned()),
            Some("double") => Ok("f64".to_owned()),
            Some(other) => Err(GeneratorError::UnsupportedType(format!(
                "builtin `{other}`"
            ))),
            None => Err(GeneratorError::InvalidMetadata(
                "builtin type has no name".to_owned(),
            )),
        },
        Some("User") => Ok(render_user_type(
            required_string_map(description, "name")?,
            domain,
        )),
        Some("Pointer") => {
            let inner = object_map(description, "inner_type")?;
            let mutability = if has_const_storage(inner) {
                "const"
            } else {
                "mut"
            };
            let rendered = if string_map(inner, "kind") == Some("Builtin")
                && string_map(inner, "builtin_type") == Some("void")
            {
                "()".to_owned()
            } else {
                render_type(inner, domain)?
            };
            Ok(format!("*{mutability} {rendered}"))
        }
        Some("Array") => {
            let bound = string_map(description, "bounds")
                .and_then(|bound| bound.parse::<usize>().ok())
                .ok_or_else(|| {
                    GeneratorError::UnsupportedType("symbolically sized array".to_owned())
                })?;
            Ok(format!(
                "[{}; {bound}]",
                render_type(object_map(description, "inner_type")?, domain)?
            ))
        }
        Some("Type") => render_type(object_map(description, "inner_type")?, domain),
        Some("Function") => {
            let return_type = render_type(object_map(description, "return_type")?, domain)?;
            let parameters = array_map(description, "parameters")?
                .iter()
                .map(|parameter| render_type(object(parameter, "inner_type")?, domain))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("fn({}) -> {return_type}", parameters.join(", ")))
        }
        Some(kind) => Err(GeneratorError::UnsupportedType(format!(
            "description kind `{kind}`"
        ))),
        None => Err(GeneratorError::InvalidMetadata(
            "type description has no kind".to_owned(),
        )),
    }
}

fn render_user_type(name: &str, domain: TypeDomain) -> String {
    if name == "size_t" {
        return "usize".to_owned();
    }
    if name == "va_list" {
        return "*mut ()".to_owned();
    }
    let prefix = match domain {
        TypeDomain::Core => "",
        TypeDomain::Backend
            if matches!(
                name,
                "SDL_Window" | "SDL_Renderer" | "SDL_Gamepad" | "SDL_Event"
            ) =>
        {
            "sdl_types::"
        }
        TypeDomain::Backend if name.starts_with("ImGui_ImplSDL3_") => "",
        TypeDomain::CoreQualified | TypeDomain::Backend => "types::",
    };
    format!("{prefix}{name}")
}

fn has_const_storage(description: &Map<String, Value>) -> bool {
    description
        .get("storage_classes")
        .and_then(Value::as_array)
        .is_some_and(|classes| classes.iter().any(|class| class.as_str() == Some("const")))
}

fn write_docs(output: &mut String, value: &Value, fallback: &str) -> Result<(), fmt::Error> {
    let lines = documentation_lines(value);
    if lines.is_empty() {
        writeln!(output, "    /// {fallback}")
    } else {
        for line in lines {
            writeln!(output, "    /// {line}")?;
        }
        Ok(())
    }
}

fn write_docs_indented(
    output: &mut String,
    value: &Value,
    fallback: &str,
) -> Result<(), fmt::Error> {
    let lines = documentation_lines(value);
    if lines.is_empty() {
        writeln!(output, "    /// {fallback}")
    } else {
        for line in lines {
            writeln!(output, "    /// {line}")?;
        }
        Ok(())
    }
}

fn documentation_lines(value: &Value) -> Vec<String> {
    let Some(comments) = value.get("comments") else {
        return Vec::new();
    };
    let mut raw = Vec::new();
    if let Some(preceding) = comments.get("preceding").and_then(Value::as_array) {
        raw.extend(preceding.iter().filter_map(Value::as_str));
    }
    if let Some(attached) = comments.get("attached").and_then(Value::as_str) {
        raw.push(attached);
    }
    raw.into_iter()
        .map(|line| {
            line.trim()
                .strip_prefix("//")
                .unwrap_or(line)
                .trim()
                .to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn generated_header(subject: &str) -> String {
    format!(
        "// Generated from Dear ImGui {IMGUI_VERSION} with Dear Bindings {DEAR_BINDINGS_VERSION}: {subject}. Do not edit by hand.\n// Run `vendor/imgui/tools/generate.ps1` to update this file.\n\n"
    )
}

fn render_report(functions: &Coverage, backends: &Coverage) -> String {
    let mut blockers = BTreeMap::new();
    blockers.insert("core_variadic", functions.variadic);
    blockers.insert("core_internal", functions.internal);
    blockers.insert(
        "core_conditionally_unavailable",
        functions.conditionally_unavailable,
    );
    blockers.insert("core_unsupported", functions.unsupported.len());
    blockers.insert("backend_unsupported", backends.unsupported.len());
    let mut output = format!(
        "imgui_version = \"{IMGUI_VERSION}\"\ndear_bindings_version = \"{DEAR_BINDINGS_VERSION}\"\ncore_functions = {}\nbackend_functions = {}\n",
        functions.generated, backends.generated
    );
    for (name, count) in blockers {
        let _ = writeln!(output, "{name} = {count}");
    }
    for blocker in functions.unsupported.iter().chain(&backends.unsupported) {
        let escaped = blocker.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(output, "\n[[unsupported]]\ndetail = \"{escaped}\"");
    }
    output
}

fn sanitize_identifier(name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "as", "comptime", "const", "defer", "else", "enum", "extern", "false", "fn", "for", "from",
        "if", "impl", "import", "in", "let", "loop", "match", "move", "pub", "ref", "return",
        "self", "static", "struct", "trait", "true", "type", "unsafe", "where", "while",
    ];
    if KEYWORDS.contains(&name) {
        format!("{name}_value")
    } else {
        name.to_owned()
    }
}

fn condition_mentions(value: &Value, name: &str) -> bool {
    value
        .get("conditionals")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                string(condition, "expression").is_some_and(|expression| expression.contains(name))
            })
        })
}

fn write_generated(path: &Path, content: &str) -> Result<(), GeneratorError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| GeneratorError::Io {
            operation: "create output directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, normalize_generated_content(content)).map_err(|source| GeneratorError::Io {
        operation: "write generated binding",
        path: path.to_path_buf(),
        source,
    })
}

fn normalize_generated_content(content: &str) -> String {
    let mut normalized = content.trim_end().to_owned();
    normalized.push('\n');
    normalized
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], GeneratorError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| GeneratorError::InvalidMetadata(format!("`{key}` is not an array")))
}

fn array_map<'a>(value: &'a Map<String, Value>, key: &str) -> Result<&'a [Value], GeneratorError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| GeneratorError::InvalidMetadata(format!("`{key}` is not an array")))
}

fn object<'a>(value: &'a Value, key: &str) -> Result<&'a Map<String, Value>, GeneratorError> {
    value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| GeneratorError::InvalidMetadata(format!("`{key}` is not an object")))
}

fn object_map<'a>(
    value: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, GeneratorError> {
    value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| GeneratorError::InvalidMetadata(format!("`{key}` is not an object")))
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    let mut current = value;
    for part in key.split('.') {
        current = current.get(part)?;
    }
    current.as_str()
}

fn string_map<'a>(value: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, GeneratorError> {
    string(value, key)
        .ok_or_else(|| GeneratorError::InvalidMetadata(format!("`{key}` is not a string")))
}

fn required_string_map<'a>(
    value: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, GeneratorError> {
    string_map(value, key)
        .ok_or_else(|| GeneratorError::InvalidMetadata(format!("`{key}` is not a string")))
}

fn boolean(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[derive(Debug, Clone, Copy)]
enum TypeDomain {
    Core,
    CoreQualified,
    Backend,
}

#[derive(Debug, Clone, Copy)]
enum FunctionDomain {
    Core,
    Backend,
}

#[derive(Debug, Default)]
struct Coverage {
    generated: usize,
    variadic: usize,
    internal: usize,
    conditionally_unavailable: usize,
    unsupported: Vec<String>,
}

#[derive(Debug)]
enum GeneratorError {
    Usage,
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    VersionMismatch {
        expected: &'static str,
        actual: String,
    },
    InvalidMetadata(String),
    UnsupportedType(String),
    Formatting(fmt::Error),
}

impl fmt::Display for GeneratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "usage: imgui-api-gen <dcimgui.json> <dcimgui_impl_sdl3.json> <dcimgui_impl_opengl3.json> <types.reim> <constants.reim> <functions.reim> <backends.reim> <coverage.toml>"
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "could not {operation} `{}`: {source}",
                path.display()
            ),
            Self::Json { path, source } => {
                write!(formatter, "invalid metadata `{}`: {source}", path.display())
            }
            Self::VersionMismatch { expected, actual } => write!(
                formatter,
                "expected Dear ImGui {expected}, metadata describes {actual}"
            ),
            Self::InvalidMetadata(detail) => {
                write!(formatter, "invalid Dear Bindings metadata: {detail}")
            }
            Self::UnsupportedType(detail) => write!(formatter, "unsupported C type: {detail}"),
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
            Self::Json { source, .. } => Some(source),
            Self::Formatting(source) => Some(source),
            Self::Usage
            | Self::VersionMismatch { .. }
            | Self::InvalidMetadata(_)
            | Self::UnsupportedType(_) => None,
        }
    }
}

impl From<fmt::Error> for GeneratorError {
    fn from(source: fmt::Error) -> Self {
        Self::Formatting(source)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        FunctionDomain, TypeDomain, condition_mentions, documentation_lines,
        normalize_generated_content, render_functions, render_type, render_wgpu_bridge,
    };

    #[test]
    fn generated_content_should_end_with_exactly_one_newline() {
        assert_eq!(normalize_generated_content("value\n\n"), "value\n");
    }

    #[test]
    fn comments_should_become_clean_documentation_lines() {
        let value = json!({
            "comments": {
                "preceding": ["// Starts a frame.", "//"],
                "attached": "// Must be paired with Render()."
            }
        });

        assert_eq!(
            documentation_lines(&value),
            [
                "Starts a frame.".to_owned(),
                "Must be paired with Render().".to_owned(),
            ]
        );
    }

    #[test]
    fn pointers_should_preserve_const_qualification() {
        let description = json!({
            "kind": "Pointer",
            "inner_type": {
                "kind": "Builtin",
                "builtin_type": "char",
                "storage_classes": ["const"]
            }
        });

        assert_eq!(
            render_type(
                description
                    .as_object()
                    .expect("description should be an object"),
                TypeDomain::Core,
            )
            .expect("const character pointer should render"),
            "*const c::Char"
        );
    }

    #[test]
    fn inactive_imstr_helpers_should_not_enter_the_native_catalog() {
        let metadata = json!({
            "functions": [{
                "name": "ImStr_FromCharStr",
                "original_fully_qualified_name": "ImStr_FromCharStr",
                "return_type": {
                    "description": { "kind": "User", "name": "ImStr" }
                },
                "arguments": [],
                "conditionals": [{
                    "condition": "if",
                    "expression": "defined(IMGUI_HAS_IMSTR)"
                }]
            }]
        });

        let function = &metadata["functions"][0];
        assert!(condition_mentions(function, "IMGUI_HAS_IMSTR"));
        let (rendered, coverage) = render_functions(&metadata, FunctionDomain::Core)
            .expect("conditional catalog should render");
        assert!(!rendered.contains("ImStr_FromCharStr"));
        assert_eq!(coverage.conditionally_unavailable, 1);
    }

    #[test]
    fn wgpu_bridge_should_keep_native_handles_typed() {
        let mut output = String::new();
        let generated = render_wgpu_bridge(&mut output).expect("bridge declarations should render");

        assert_eq!(generated, 9);
        assert!(output.contains("device: wgpu_types::WGPUDevice"));
        assert!(output.contains("pass: wgpu_types::WGPURenderPassEncoder"));
        assert!(output.contains("byte_length: usize"));
    }
}
