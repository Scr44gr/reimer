//! Parser for the regular, generated declaration shapes used by wgpu-native.

use crate::model::{
    Alias, Api, CType, Callback, Constant, ConstantValue, EnumEntry, Enumeration, Field, Function,
    Handle, Parameter, PointerKind, Structure,
};

/// Parses the standard WebGPU header followed by wgpu-native extensions.
pub(crate) fn parse_headers(standard: &str, native: &str) -> Result<Api, String> {
    let mut api = Api::default();
    parse_header(standard, false, &mut api)?;
    parse_header(native, true, &mut api)?;
    ensure_unique_names(&api)?;
    Ok(api)
}

fn parse_header(source: &str, native_extension: bool, api: &mut Api) -> Result<(), String> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut pending_documentation = String::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.starts_with("/**") {
            let (documentation, next) = collect_block_comment(&lines, index);
            pending_documentation = documentation;
            index = next;
            continue;
        }
        if let Some(documentation) = line.strip_prefix("//") {
            documentation.trim().clone_into(&mut pending_documentation);
            index += 1;
            continue;
        }
        if line.is_empty() || line.starts_with('#') || is_linkage_line(line) {
            index += 1;
            continue;
        }

        if line.starts_with("typedef enum ") {
            let (enumeration, next) = parse_enumeration(
                &lines,
                index,
                take_documentation(&mut pending_documentation),
            )?;
            api.enumerations.push(enumeration);
            index = next;
            continue;
        }
        if is_structure_start(&lines, index) {
            let (structure, next) = parse_structure(
                &lines,
                index,
                take_documentation(&mut pending_documentation),
            )?;
            api.structures.push(structure);
            index = next;
            continue;
        }
        if let Some(handle) = parse_handle(line, &mut pending_documentation) {
            api.handles.push(handle);
            index += 1;
            continue;
        }
        if line.starts_with("typedef ") && line.contains("(*") {
            let callback = parse_callback(line, take_documentation(&mut pending_documentation))?;
            api.callbacks.push(callback);
            index += 1;
            continue;
        }
        if let Some(alias) = parse_alias(line, &mut pending_documentation)? {
            api.aliases.push(alias);
            index += 1;
            continue;
        }
        if let Some(constant) = parse_static_constant(line, &mut pending_documentation)? {
            api.constants.push(constant);
            index += 1;
            continue;
        }
        if let Some(constant) = parse_sentinel_constant(line, &mut pending_documentation)? {
            api.constants.push(constant);
            index += 1;
            continue;
        }
        if is_function_declaration(line, native_extension) {
            let function = parse_function(
                line,
                take_documentation(&mut pending_documentation),
                native_extension,
            )?;
            api.functions.push(function);
            index += 1;
            continue;
        }

        pending_documentation.clear();
        index += 1;
    }
    Ok(())
}

fn collect_block_comment(lines: &[&str], start: usize) -> (String, usize) {
    let mut documentation = Vec::new();
    let mut index = start;
    loop {
        let line = lines[index].trim();
        let line = line.strip_prefix("/**").unwrap_or(line);
        let line = line.strip_suffix("*/").unwrap_or(line);
        let line = line.trim_start_matches('*').trim();
        if !line.is_empty() {
            documentation.push(line.to_owned());
        } else if !documentation.is_empty() {
            documentation.push(String::new());
        }
        index += 1;
        if lines[index - 1].contains("*/") || index == lines.len() {
            break;
        }
    }
    while documentation.last().is_some_and(String::is_empty) {
        documentation.pop();
    }
    (documentation.join("\n"), index)
}

fn parse_enumeration(
    lines: &[&str],
    start: usize,
    documentation: String,
) -> Result<(Enumeration, usize), String> {
    let name = lines[start]
        .trim()
        .strip_prefix("typedef enum ")
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or_else(|| format!("invalid enum declaration `{}`", lines[start].trim()))?
        .to_owned();
    let mut entries = Vec::new();
    let mut entry_documentation = String::new();
    let mut index = start + 1;
    if lines[start].trim().ends_with('{') {
        index = start + 1;
    } else if lines.get(index).is_some_and(|line| line.trim() == "{") {
        index += 1;
    } else {
        return Err(format!("enum `{name}` has no opening brace"));
    }
    let mut previous = None;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.starts_with("/**") {
            let (comment, next) = collect_block_comment(lines, index);
            entry_documentation = comment;
            index = next;
            continue;
        }
        if line.starts_with('}') {
            return Ok((
                Enumeration {
                    name,
                    documentation,
                    entries,
                },
                index + 1,
            ));
        }
        if line.starts_with("WGPU") {
            let declaration = line.trim_end_matches(',');
            let (entry_name, value) =
                if let Some((entry_name, expression)) = declaration.split_once('=') {
                    (
                        entry_name.trim().to_owned(),
                        parse_integer_expression(expression.trim())?,
                    )
                } else {
                    let value = previous
                        .and_then(|value: u64| value.checked_add(1))
                        .unwrap_or(0);
                    (declaration.trim().to_owned(), value)
                };
            previous = Some(value);
            entries.push(EnumEntry {
                name: entry_name,
                value,
                documentation: take_documentation(&mut entry_documentation),
            });
        }
        index += 1;
    }
    Err(format!("enum `{name}` has no closing brace"))
}

fn is_structure_start(lines: &[&str], index: usize) -> bool {
    let line = lines[index].trim();
    if !line.starts_with("typedef struct WGPU") || line.contains('*') {
        return false;
    }
    line.ends_with('{') || lines.get(index + 1).is_some_and(|line| line.trim() == "{")
}

fn parse_structure(
    lines: &[&str],
    start: usize,
    documentation: String,
) -> Result<(Structure, usize), String> {
    let name = lines[start]
        .trim()
        .strip_prefix("typedef struct ")
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or_else(|| format!("invalid struct declaration `{}`", lines[start].trim()))?
        .to_owned();
    let mut fields = Vec::new();
    let mut field_documentation = String::new();
    let mut index = start + 1;
    if !lines[start].trim().ends_with('{') {
        index += 1;
    }
    while index < lines.len() {
        let line = lines[index].trim();
        if line.starts_with("/**") {
            let (comment, next) = collect_block_comment(lines, index);
            field_documentation = comment;
            index = next;
            continue;
        }
        if let Some(comment) = line.strip_prefix("//") {
            comment.trim().clone_into(&mut field_documentation);
            index += 1;
            continue;
        }
        if line.starts_with('}') {
            return Ok((
                Structure {
                    name,
                    documentation,
                    fields,
                },
                index + 1,
            ));
        }
        if line.ends_with(';') && !line.starts_with('#') {
            let (c_name, ty) = parse_named_declaration(line)?;
            fields.push(Field {
                name: to_snake_case(&c_name),
                c_name,
                ty,
                documentation: take_documentation(&mut field_documentation),
            });
        }
        index += 1;
    }
    Err(format!("struct `{name}` has no closing brace"))
}

fn parse_handle(line: &str, documentation: &mut String) -> Option<Handle> {
    let rest = line.strip_prefix("typedef struct WGPU")?;
    if !rest.contains("Impl*") || !line.ends_with("WGPU_OBJECT_ATTRIBUTE;") {
        return None;
    }
    let (_, alias) = line.split_once("Impl*")?;
    let name = alias.split_whitespace().next()?.to_owned();
    Some(Handle {
        name,
        documentation: take_documentation(documentation),
    })
}

fn parse_callback(line: &str, documentation: String) -> Result<Callback, String> {
    let name_start = line
        .find("(*")
        .ok_or_else(|| format!("callback declaration has no name: `{line}`"))?
        + 2;
    let name_end = line[name_start..]
        .find(')')
        .map(|offset| name_start + offset)
        .ok_or_else(|| format!("callback declaration has no closing name: `{line}`"))?;
    let name = line[name_start..name_end].trim();
    if !name.starts_with("WGPU") {
        return Err(format!("unexpected callback name `{name}`"));
    }
    Ok(Callback {
        name: name.to_owned(),
        documentation,
    })
}

fn parse_alias(line: &str, documentation: &mut String) -> Result<Option<Alias>, String> {
    if !line.starts_with("typedef ")
        || !line.ends_with(';')
        || line.starts_with("typedef enum ")
        || line.starts_with("typedef struct ")
    {
        return Ok(None);
    }
    let declaration = line
        .trim_start_matches("typedef ")
        .trim_end_matches(';')
        .trim();
    let (name, target) = parse_named_declaration(&format!("{declaration};"))?;
    if !name.starts_with("WGPU") {
        return Ok(None);
    }
    Ok(Some(Alias {
        name,
        target,
        documentation: take_documentation(documentation),
    }))
}

fn parse_static_constant(
    line: &str,
    documentation: &mut String,
) -> Result<Option<Constant>, String> {
    let Some(declaration) = line.strip_prefix("static const ") else {
        return Ok(None);
    };
    let declaration = declaration.trim_end_matches(';');
    let Some((left, expression)) = declaration.split_once('=') else {
        return Err(format!("static constant has no value: `{line}`"));
    };
    let (name, ty) = parse_named_declaration(&format!("{};", left.trim()))?;
    Ok(Some(Constant {
        name,
        ty,
        value: ConstantValue::Integer(parse_integer_expression(expression.trim())?),
        documentation: take_documentation(documentation),
    }))
}

fn parse_sentinel_constant(
    line: &str,
    documentation: &mut String,
) -> Result<Option<Constant>, String> {
    let Some(rest) = line.strip_prefix("#define WGPU_") else {
        return Ok(None);
    };
    let Some((suffix, expression)) = rest.split_once(char::is_whitespace) else {
        return Ok(None);
    };
    let (ty, value) = match suffix {
        "TRUE"
        | "FALSE"
        | "ARRAY_LAYER_COUNT_UNDEFINED"
        | "COPY_STRIDE_UNDEFINED"
        | "DEPTH_SLICE_UNDEFINED"
        | "LIMIT_U32_UNDEFINED"
        | "MIP_LEVEL_COUNT_UNDEFINED"
        | "QUERY_SET_INDEX_UNDEFINED" => (
            CType::scalar("uint32_t"),
            ConstantValue::Integer(parse_integer_expression(expression)?),
        ),
        "LIMIT_U64_UNDEFINED" | "WHOLE_SIZE" => (
            CType::scalar("uint64_t"),
            ConstantValue::Integer(parse_integer_expression(expression)?),
        ),
        "STRLEN" | "WHOLE_MAP_SIZE" => (CType::scalar("size_t"), ConstantValue::Integer(u64::MAX)),
        "DEPTH_CLEAR_VALUE_UNDEFINED" => (CType::scalar("double"), ConstantValue::FloatNaN),
        _ => return Ok(None),
    };
    Ok(Some(Constant {
        name: format!("WGPU_{suffix}"),
        ty,
        value,
        documentation: take_documentation(documentation),
    }))
}

fn is_function_declaration(line: &str, native_extension: bool) -> bool {
    if line.starts_with("WGPU_EXPORT ") {
        return true;
    }
    native_extension
        && line.ends_with(';')
        && line.contains(" wgpu")
        && line.contains('(')
        && !line.starts_with("typedef ")
}

fn parse_function(
    line: &str,
    documentation: String,
    native_extension: bool,
) -> Result<Function, String> {
    let declaration = line
        .strip_prefix("WGPU_EXPORT ")
        .unwrap_or(line)
        .trim_end_matches(';')
        .trim_end_matches(" WGPU_FUNCTION_ATTRIBUTE")
        .trim();
    let open = declaration
        .find('(')
        .ok_or_else(|| format!("function has no parameter list: `{line}`"))?;
    let close = declaration
        .rfind(')')
        .ok_or_else(|| format!("function has no closing parenthesis: `{line}`"))?;
    let prefix = declaration[..open].trim();
    let name_start = prefix
        .rfind(char::is_whitespace)
        .ok_or_else(|| format!("function has no return type: `{line}`"))?;
    let name = prefix[name_start..].trim().to_owned();
    let return_type = parse_type(prefix[..name_start].trim())?;
    let parameters = parse_parameters(&declaration[open + 1..close])?;
    Ok(Function {
        name,
        return_type,
        parameters,
        documentation,
        native_extension,
    })
}

fn parse_parameters(source: &str) -> Result<Vec<Parameter>, String> {
    if source.trim().is_empty() || source.trim() == "void" {
        return Ok(Vec::new());
    }
    source
        .split(',')
        .enumerate()
        .map(|(index, parameter)| {
            let (name, ty) = parse_named_declaration(&format!("{};", parameter.trim()))?;
            Ok(Parameter {
                name: if name.is_empty() {
                    format!("argument_{index}")
                } else {
                    to_snake_case(&name)
                },
                ty,
            })
        })
        .collect()
}

fn parse_named_declaration(source: &str) -> Result<(String, CType), String> {
    let normalized = source
        .trim()
        .trim_end_matches(';')
        .replace("WGPU_NULLABLE", "")
        .replace("WGPU_STRUCTURE_ATTRIBUTE", "")
        .replace("WGPU_ENUM_ATTRIBUTE", "")
        .replace("WGPU_OBJECT_ATTRIBUTE", "")
        .replace("WGPU_FUNCTION_ATTRIBUTE", "")
        .replace('*', " * ");
    let mut tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let name = tokens
        .pop()
        .ok_or_else(|| format!("empty declaration `{source}`"))?
        .to_owned();
    let ty = parse_type_tokens(&tokens)?;
    Ok((name, ty))
}

fn parse_type(source: &str) -> Result<CType, String> {
    let normalized = source.replace("WGPU_NULLABLE", "").replace('*', " * ");
    parse_type_tokens(&normalized.split_whitespace().collect::<Vec<_>>())
}

fn parse_type_tokens(tokens: &[&str]) -> Result<CType, String> {
    let first_pointer = tokens.iter().position(|token| *token == "*");
    let is_const = tokens.iter().enumerate().any(|(index, token)| {
        *token == "const" && first_pointer.is_none_or(|pointer| index < pointer)
    });
    let pointer_count = tokens.iter().filter(|token| **token == "*").count();
    let base = tokens
        .iter()
        .filter(|token| !matches!(**token, "*" | "const" | "struct"))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if base.is_empty() {
        return Err(format!("type has no base in `{}`", tokens.join(" ")));
    }
    let mut pointers = Vec::with_capacity(pointer_count);
    for index in 0..pointer_count {
        pointers.push(if index == 0 && is_const {
            PointerKind::Const
        } else {
            PointerKind::Mut
        });
    }
    Ok(CType { base, pointers })
}

impl CType {
    fn scalar(base: &str) -> Self {
        Self {
            base: base.to_owned(),
            pointers: Vec::new(),
        }
    }
}

fn parse_integer_expression(source: &str) -> Result<u64, String> {
    let normalized = source
        .trim()
        .trim_matches(|character| character == '(' || character == ')')
        .replace("UINT32_C(", "")
        .replace("UINT64_C(", "")
        .replace("SIZE_MAX", &u64::MAX.to_string())
        .replace("UINT32_MAX", &u32::MAX.to_string())
        .replace("UINT64_MAX", &u64::MAX.to_string())
        .replace(')', "");
    let mut value = 0_u64;
    for term in normalized.split('|') {
        let term = term
            .trim()
            .trim_matches(|character| character == '(' || character == ')');
        let term_value = if let Some((left, right)) = term.split_once("<<") {
            parse_integer(left.trim())?
                .checked_shl(
                    u32::try_from(parse_integer(right.trim())?)
                        .map_err(|_| format!("shift exceeds u32 in `{source}`"))?,
                )
                .ok_or_else(|| format!("shift overflows in `{source}`"))?
        } else {
            parse_integer(term)?
        };
        value |= term_value;
    }
    Ok(value)
}

fn parse_integer(source: &str) -> Result<u64, String> {
    let value = source.trim().trim_end_matches(['u', 'U', 'l', 'L']).trim();
    if let Some(hexadecimal) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hexadecimal, 16)
            .map_err(|error| format!("invalid hexadecimal integer `{source}`: {error}"))
    } else {
        value
            .parse::<u64>()
            .map_err(|error| format!("invalid integer `{source}`: {error}"))
    }
}

fn to_snake_case(name: &str) -> String {
    let mut output = String::with_capacity(name.len() + 4);
    let characters = name.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().copied().enumerate() {
        let previous_is_lower = index > 0 && characters[index - 1].is_ascii_lowercase();
        let next_is_lower = characters
            .get(index + 1)
            .is_some_and(char::is_ascii_lowercase);
        if character.is_ascii_uppercase()
            && index > 0
            && (previous_is_lower || next_is_lower)
            && !output.ends_with('_')
        {
            output.push('_');
        }
        output.push(character.to_ascii_lowercase());
    }
    sanitize_identifier(&output)
}

fn sanitize_identifier(identifier: &str) -> String {
    if matches!(
        identifier,
        "as" | "break"
            | "comptime"
            | "const"
            | "continue"
            | "defer"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "from"
            | "if"
            | "impl"
            | "import"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "move"
            | "mut"
            | "pub"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "while"
    ) {
        format!("{identifier}_value")
    } else {
        identifier.to_owned()
    }
}

fn is_linkage_line(line: &str) -> bool {
    matches!(line, "extern \"C\"" | "{" | "}" | "};") || line.starts_with("} // extern")
}

fn take_documentation(documentation: &mut String) -> String {
    std::mem::take(documentation)
}

fn ensure_unique_names(api: &Api) -> Result<(), String> {
    let mut types = std::collections::BTreeSet::new();
    for name in api
        .handles
        .iter()
        .map(|item| item.name.as_str())
        .chain(api.aliases.iter().map(|item| item.name.as_str()))
        .chain(api.enumerations.iter().map(|item| item.name.as_str()))
        .chain(api.callbacks.iter().map(|item| item.name.as_str()))
        .chain(api.structures.iter().map(|item| item.name.as_str()))
    {
        if !types.insert(name) {
            return Err(format!("duplicate C type declaration `{name}`"));
        }
    }
    let mut functions = std::collections::BTreeSet::new();
    for function in &api.functions {
        if !functions.insert(function.name.as_str()) {
            return Err(format!(
                "duplicate C function declaration `{}`",
                function.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_headers, parse_integer_expression, to_snake_case};

    #[test]
    fn integer_expression_should_evaluate_native_bitflag_combinations() {
        assert_eq!(
            parse_integer_expression("(1 << 0) | (1 << 2) | (1 << 5)"),
            Ok(37)
        );
    }

    #[test]
    fn snake_case_should_preserve_acronym_words() {
        assert_eq!(to_snake_case("vendorID"), "vendor_id");
        assert_eq!(to_snake_case("sType"), "s_type");
    }

    #[test]
    fn parser_should_collect_a_minimal_api() {
        let standard = r"
            typedef struct WGPUDeviceImpl* WGPUDevice WGPU_OBJECT_ATTRIBUTE;
            typedef enum WGPUStatus {
                WGPUStatus_Success = 0x00000001,
            } WGPUStatus;
            typedef struct WGPUDescriptor {
                WGPUDevice device;
            } WGPUDescriptor WGPU_STRUCTURE_ATTRIBUTE;
            WGPU_EXPORT void wgpuDeviceRelease(WGPUDevice device) WGPU_FUNCTION_ATTRIBUTE;
        ";

        let api = parse_headers(standard, "").expect("valid generated header shapes");

        assert_eq!(api.handles.len(), 1);
        assert_eq!(api.enumerations.len(), 1);
        assert_eq!(api.structures.len(), 1);
        assert_eq!(api.functions.len(), 1);
    }
}
