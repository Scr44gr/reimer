//! Reimer source rendering and documentation cleanup.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::model::{Api, CType, ConstantValue, PointerKind};

const WGPU_NATIVE_VERSION: &str = "29.0.1.1";

pub(crate) struct RenderedApi {
    pub(crate) types: String,
    pub(crate) constants: String,
    pub(crate) functions: String,
    pub(crate) coverage: String,
}

pub(crate) fn render_api(api: &Api) -> Result<RenderedApi, String> {
    Ok(RenderedApi {
        types: render_types(api)?,
        constants: render_constants(api)?,
        functions: render_functions(api)?,
        coverage: render_coverage(api),
    })
}

fn render_types(api: &Api) -> Result<String, String> {
    let mut output = generated_header("raw C types");
    writeln!(output, "import std::c;\n").map_err(format_error)?;

    for handle in &api.handles {
        write_documentation(
            &mut output,
            &handle.documentation,
            &format!(
                "Opaque, reference-counted `{}` WebGPU object handle.",
                handle.name
            ),
            "",
        )?;
        writeln!(output, "pub type {} = *mut ();\n", handle.name).map_err(format_error)?;
    }

    for alias in &api.aliases {
        write_documentation(
            &mut output,
            &alias.documentation,
            &format!("C-compatible `{}` value.", alias.name),
            "",
        )?;
        writeln!(
            output,
            "pub type {} = {};\n",
            alias.name,
            render_type(&alias.target, false)?
        )
        .map_err(format_error)?;
    }

    for enumeration in &api.enumerations {
        write_documentation(
            &mut output,
            &enumeration.documentation,
            &format!("C enumeration storage for `{}`.", enumeration.name),
            "",
        )?;
        writeln!(output, "pub type {} = c::Int;\n", enumeration.name).map_err(format_error)?;
    }

    for callback in &api.callbacks {
        write_documentation(
            &mut output,
            &callback.documentation,
            &format!("Opaque C callback address for `{}`.", callback.name),
            "",
        )?;
        writeln!(
            output,
            "/// Use the safe facade for callback-driven operations; it supplies ABI-correct native trampolines.\npub type {} = *mut ();\n",
            callback.name
        )
        .map_err(format_error)?;
    }

    for structure in &api.structures {
        write_documentation(
            &mut output,
            &structure.documentation,
            &format!(
                "C-compatible storage for WebGPU's `{}` structure.",
                structure.name
            ),
            "",
        )?;
        writeln!(output, "@repr(C)\npub struct {} {{", structure.name).map_err(format_error)?;
        for field in &structure.fields {
            write_documentation(
                &mut output,
                &field.documentation,
                &format!("Native `{}` field.", field.c_name),
                "    ",
            )?;
            writeln!(
                output,
                "    pub {}: {},",
                field.name,
                render_type(&field.ty, false)?
            )
            .map_err(format_error)?;
        }
        writeln!(output, "}}\n").map_err(format_error)?;
    }
    Ok(output)
}

fn render_constants(api: &Api) -> Result<String, String> {
    let mut output = generated_header("raw constants and enumeration values");
    writeln!(output, "import self::types as types;\n").map_err(format_error)?;
    writeln!(
        output,
        "/// Pinned wgpu-native release represented by these declarations.\npub const WGPU_NATIVE_VERSION: cstr = c\"{WGPU_NATIVE_VERSION}\";\n"
    )
    .map_err(format_error)?;

    for enumeration in &api.enumerations {
        for entry in &enumeration.entries {
            write_documentation(
                &mut output,
                &entry.documentation,
                &format!("`{}` value from `{}`.", entry.name, enumeration.name),
                "",
            )?;
            writeln!(
                output,
                "pub const {}: types::{} = {};",
                entry.name,
                enumeration.name,
                format_integer(entry.value)
            )
            .map_err(format_error)?;
        }
        writeln!(output).map_err(format_error)?;
    }

    let mut names = BTreeSet::new();
    for constant in &api.constants {
        if !names.insert(constant.name.as_str()) {
            continue;
        }
        write_documentation(
            &mut output,
            &constant.documentation,
            &format!("WebGPU constant `{}`.", constant.name),
            "",
        )?;
        let value = match constant.value {
            ConstantValue::Integer(value) => format_integer(value),
            ConstantValue::FloatNaN => "0.0 / 0.0".to_owned(),
        };
        writeln!(
            output,
            "pub const {}: {} = {value};\n",
            constant.name,
            render_type(&constant.ty, true)?
        )
        .map_err(format_error)?;
    }
    Ok(output)
}

fn render_functions(api: &Api) -> Result<String, String> {
    let mut output = generated_header("raw functions");
    writeln!(output, "import self::types as types;\n").map_err(format_error)?;
    writeln!(output, "@link(\"wgpu_native\")\nextern \"C\" {{").map_err(format_error)?;
    for function in &api.functions {
        write_documentation(
            &mut output,
            &function.documentation,
            &format!("Calls the native `{}` WebGPU operation.", function.name),
            "    ",
        )?;
        if function.native_extension {
            writeln!(output, "    /// This operation is a wgpu-native extension.")
                .map_err(format_error)?;
        }
        write!(output, "    pub fn {}(", function.name).map_err(format_error)?;
        for (index, parameter) in function.parameters.iter().enumerate() {
            if index > 0 {
                write!(output, ", ").map_err(format_error)?;
            }
            write!(
                output,
                "{}: {}",
                parameter.name,
                render_type(&parameter.ty, true)?
            )
            .map_err(format_error)?;
        }
        writeln!(
            output,
            ") -> {};\n",
            render_type(&function.return_type, true)?
        )
        .map_err(format_error)?;
    }
    writeln!(output, "}}").map_err(format_error)?;
    Ok(output)
}

fn render_coverage(api: &Api) -> String {
    let standard_functions = api
        .functions
        .iter()
        .filter(|function| !function.native_extension)
        .count();
    let native_functions = api.functions.len() - standard_functions;
    format!(
        "wgpu_native_version = \"{WGPU_NATIVE_VERSION}\"\nhandles = {}\naliases = {}\nenumerations = {}\nenumeration_values = {}\nconstants = {}\nstructures = {}\nopaque_callbacks = {}\nstandard_functions = {standard_functions}\nnative_functions = {native_functions}\n",
        api.handles.len(),
        api.aliases.len(),
        api.enumerations.len(),
        api.enumerations
            .iter()
            .map(|enumeration| enumeration.entries.len())
            .sum::<usize>(),
        api.constants.len(),
        api.structures.len(),
        api.callbacks.len(),
    )
}

fn render_type(ty: &CType, qualified: bool) -> Result<String, String> {
    let base = match ty.base.as_str() {
        "void" => "()".to_owned(),
        "char" => "c::Char".to_owned(),
        "signed char" | "int8_t" => "i8".to_owned(),
        "unsigned char" | "uint8_t" => "u8".to_owned(),
        "short" | "int16_t" => "i16".to_owned(),
        "unsigned short" | "uint16_t" => "u16".to_owned(),
        "int" | "int32_t" => "i32".to_owned(),
        "unsigned int" | "uint32_t" => "u32".to_owned(),
        "int64_t" => "i64".to_owned(),
        "uint64_t" => "u64".to_owned(),
        "size_t" => "usize".to_owned(),
        "float" => "f32".to_owned(),
        "double" => "f64".to_owned(),
        "bool" | "_Bool" => "bool".to_owned(),
        name if name.starts_with("WGPU") => {
            if qualified {
                format!("types::{name}")
            } else {
                name.to_owned()
            }
        }
        unknown => return Err(format!("unsupported C type `{unknown}`")),
    };
    let mut rendered = base;
    for pointer in &ty.pointers {
        rendered = match pointer {
            PointerKind::Const => format!("*const {rendered}"),
            PointerKind::Mut => format!("*mut {rendered}"),
        };
    }
    Ok(rendered)
}

fn write_documentation(
    output: &mut String,
    source: &str,
    fallback: &str,
    indentation: &str,
) -> Result<(), String> {
    let documentation = clean_documentation(source);
    let documentation = if documentation.is_empty()
        || documentation.eq_ignore_ascii_case("TODO")
        || documentation.starts_with("@copydoc")
    {
        fallback.to_owned()
    } else {
        documentation
    };
    for line in documentation.lines() {
        if line.is_empty() {
            writeln!(output, "{indentation}///").map_err(format_error)?;
        } else {
            writeln!(output, "{indentation}/// {line}").map_err(format_error)?;
        }
    }
    Ok(())
}

fn clean_documentation(source: &str) -> String {
    let mut output = Vec::new();
    let mut in_code = false;
    for raw_line in source.lines() {
        let mut line = raw_line.trim().to_owned();
        if line.eq_ignore_ascii_case("TODO") {
            continue;
        }
        if line.contains("@{") || line.contains("@}") || line.starts_with("\\defgroup") {
            continue;
        }
        if line == "@code" {
            output.push("```".to_owned());
            in_code = true;
            continue;
        }
        if line == "@endcode" {
            output.push("```".to_owned());
            in_code = false;
            continue;
        }
        if matches!(line.as_str(), "@returns" | "@return") {
            "Returns:".clone_into(&mut line);
        }
        line = line
            .replace("@ref ", "")
            .replace("\\ref ", "")
            .replace("@brief ", "")
            .replace("\\brief ", "")
            .replace("@remark ", "Note: ")
            .replace("@note ", "Note: ")
            .replace("@warning ", "Warning: ")
            .replace("@returns ", "Returns: ")
            .replace("@return ", "Returns: ")
            .replace("@param ", "Parameter ")
            .replace("@c ", "");
        if !in_code {
            line = line.replace("  ", " ");
        }
        output.push(line.trim().to_owned());
    }
    while output.first().is_some_and(String::is_empty) {
        output.remove(0);
    }
    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    output.join("\n")
}

fn generated_header(subject: &str) -> String {
    format!(
        "// Generated from wgpu-native {WGPU_NATIVE_VERSION}: {subject}. Do not edit by hand.\n// Run `vendor/wgpu/tools/generate.ps1` to update this file.\n\n"
    )
}

fn format_integer(value: u64) -> String {
    if u32::try_from(value).is_ok() {
        format!("0x{value:08x}")
    } else {
        format!("0x{value:016x}")
    }
}

fn format_error(error: std::fmt::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_documentation;

    #[test]
    fn documentation_should_remove_doxygen_commands_without_losing_meaning() {
        let source = "@note Use @ref WGPUDevice before @c submit.";
        assert_eq!(
            clean_documentation(source),
            "Note: Use WGPUDevice before submit."
        );
    }
}
