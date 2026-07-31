#!/usr/bin/env python3
"""Generate Reimer OpenGL bindings from a pinned Khronos registry snapshot."""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import OrderedDict
from pathlib import Path
import re
import urllib.request
import xml.etree.ElementTree as ET


PACKAGE_ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = PACKAGE_ROOT / "registry.lock.json"
OUTPUT_PATH = PACKAGE_ROOT / "src" / "raw.reim"
CHECKSUM_PATH = PACKAGE_ROOT / "checksums.sha256"

TYPE_MAP = {
    "void": "()",
    "GLenum": "u32",
    "GLboolean": "u8",
    "GLbitfield": "u32",
    "GLvoid": "()",
    "GLbyte": "i8",
    "GLubyte": "u8",
    "GLshort": "i16",
    "GLushort": "u16",
    "GLint": "i32",
    "GLuint": "u32",
    "GLsizei": "i32",
    "GLfloat": "f32",
    "GLclampf": "f32",
    "GLdouble": "f64",
    "GLclampd": "f64",
    "GLchar": "u8",
    "GLcharARB": "u8",
    "GLhalf": "u16",
    "GLhalfARB": "u16",
    "GLfixed": "i32",
    "GLintptr": "isize",
    "GLsizeiptr": "isize",
    "GLint64": "i64",
    "GLuint64": "u64",
    "GLsync": "*mut ()",
    "GLDEBUGPROC": "DebugCallback",
}

RESERVED = {
    "as", "break", "comptime", "const", "continue", "defer", "else",
    "enum", "extern", "false", "fn", "for", "from", "if", "impl",
    "import", "in", "let", "loop", "match", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "trait", "true",
    "unsafe", "use", "where", "while",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=Path,
        help="Use a local gl.xml after verifying it against registry.lock.json.",
    )
    return parser.parse_args()


def load_registry(path: Path | None) -> tuple[ET.Element, dict[str, str]]:
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    if path is None:
        url = (
            f"https://raw.githubusercontent.com/KhronosGroup/OpenGL-Registry/"
            f"{lock['commit']}/{lock['path']}"
        )
        with urllib.request.urlopen(url) as response:
            payload = response.read()
    else:
        payload = path.read_bytes()
    digest = hashlib.sha256(payload).hexdigest()
    if digest != lock["sha256"]:
        raise SystemExit(
            f"registry checksum mismatch: expected {lock['sha256']}, found {digest}"
        )
    return ET.fromstring(payload), lock


def version_key(value: str) -> tuple[int, int]:
    major, minor = value.split(".", 1)
    return int(major), int(minor)


def selected_names(
    root: ET.Element, api: str, maximum: str
) -> tuple[list[str], list[str]]:
    commands: OrderedDict[str, None] = OrderedDict()
    enums: OrderedDict[str, None] = OrderedDict()
    maximum_key = version_key(maximum)
    for feature in root.findall("feature"):
        if feature.get("api") != api:
            continue
        if version_key(feature.get("number", "0.0")) > maximum_key:
            continue
        for require in feature.findall("require"):
            if require.get("profile") not in (None, "core"):
                continue
            if require.get("api") not in (None, api):
                continue
            for command in require.findall("command"):
                commands[command.get("name", "")] = None
            for enum in require.findall("enum"):
                enums[enum.get("name", "")] = None
        for remove in feature.findall("remove"):
            if remove.get("profile") != "core":
                continue
            for command in remove.findall("command"):
                commands.pop(command.get("name", ""), None)
            for enum in remove.findall("enum"):
                enums.pop(enum.get("name", ""), None)
    return list(commands), list(enums)


def pointer_type(base: str, declaration: str) -> str:
    mapped = TYPE_MAP.get(base)
    if mapped is None:
        raise ValueError(f"unsupported OpenGL type {base!r} in {declaration!r}")
    stars = declaration.count("*")
    if "[" in declaration:
        stars += 1
    if stars == 0:
        return mapped
    immutable = "const" in declaration
    for _ in range(stars):
        mapped = f"*{'const' if immutable else 'mut'} {mapped}"
    return mapped


def declaration_type(element: ET.Element) -> str:
    name = element.findtext("name")
    declaration = "".join(element.itertext())
    if name:
        declaration = declaration.replace(name, "", 1)
    base = element.findtext("ptype") or "void"
    return pointer_type(base, declaration)


def command_signatures(
    root: ET.Element, names: list[str]
) -> list[tuple[str, list[str], str]]:
    available = {
        command.findtext("proto/name"): command
        for command in root.findall("./commands/command")
        if command.find("proto") is not None
    }
    signatures = []
    for name in names:
        command = available[name]
        parameters = [declaration_type(param) for param in command.findall("param")]
        returns = declaration_type(command.find("proto"))
        signatures.append((name, parameters, returns))
    return signatures


def enum_definitions(root: ET.Element) -> dict[str, ET.Element]:
    definitions: dict[str, ET.Element] = {}
    for group in root.findall("enums"):
        for enum in group.findall("enum"):
            name = enum.get("name")
            if name and name not in definitions:
                definitions[name] = enum
    return definitions


def enum_value(
    name: str, definitions: dict[str, ET.Element], seen: set[str] | None = None
) -> int:
    seen = set() if seen is None else seen
    if name in seen:
        raise ValueError(f"cyclic enum alias involving {name}")
    seen.add(name)
    enum = definitions[name]
    alias = enum.get("alias")
    if alias:
        return enum_value(alias, definitions, seen)
    if enum.get("bitpos") is not None:
        return 1 << int(enum.get("bitpos", "0"))
    value = enum.get("value")
    if value is None:
        raise ValueError(f"OpenGL enum {name} has no numeric value")
    normalized = re.sub(r"[uUlL]+$", "", value.strip())
    if normalized.startswith("(~"):
        bits = 64 if "LL" in value.upper() else 32
        inner = re.search(r"~\s*(0x[0-9A-Fa-f]+|\d+)", value)
        if inner is None:
            raise ValueError(f"unsupported OpenGL enum expression {value!r}")
        return (~int(inner.group(1), 0)) & ((1 << bits) - 1)
    return int(normalized, 0)


def render(
    root: ET.Element, lock: dict[str, str], commands: list[str], enums: list[str]
) -> str:
    signatures = command_signatures(root, commands)
    definitions = enum_definitions(root)
    lines = [
        "// Generated by tools/update.py. Do not edit by hand.",
        "// SPDX-License-Identifier: Apache-2.0",
        f"// Khronos OpenGL-Registry commit: {lock['commit']}",
        f"// gl.xml SHA-256: {lock['sha256']}",
        "",
        "/// Callback accepted by `glDebugMessageCallback`.",
        "pub type DebugCallback = fn(u32, u32, u32, u32, i32, *const u8, *const ()) -> ();",
        "/// Platform callback that resolves an OpenGL command for the current context.",
        "pub type LoadFunction = fn(cstr) -> *const u8;",
        "",
        f"pub const CORE_FUNCTION_COUNT: usize = {len(signatures)};",
        f"pub const CORE_CONSTANT_COUNT: usize = {len(enums)};",
        "",
    ]
    for name in enums:
        value = enum_value(name, definitions)
        ty = "u64" if value > 0xFFFF_FFFF else "u32"
        literal = f"0x{value:016x}" if ty == "u64" else f"0x{value:08x}"
        lines.append(f"pub const {name}: {ty} = {literal};")
    lines.extend(["", "// Typed command signatures from the OpenGL 4.6 core profile."])
    for name, parameters, returns in signatures:
        params = ", ".join(parameters)
        lines.append(f"pub type PFN_{name} = fn({params}) -> {returns};")
    lines.extend(["", "/// OpenGL 4.6 core commands resolved for one current context.", "pub struct Functions {"])
    for name, _, _ in signatures:
        lines.append(f"    pub {name}: Option<PFN_{name}>,")
    lines.extend(["}", "", "impl Functions {", "    /// Resolves every core command exposed by the current context.", "    pub fn load(resolve: LoadFunction) -> Functions {", "        Functions {"])
    for name, _, _ in signatures:
        lines.append(f"            {name}: load_{name}(resolve),")
    lines.extend(["        }", "    }", "}", ""])
    for name, _, _ in signatures:
        lines.extend(
            [
                f"fn load_{name}(resolve: LoadFunction) -> Option<PFN_{name}> {{",
                f"    let address = resolve(c\"{name}\");",
                "    if address as usize == 0 {",
                "        None",
                "    } else {",
                f"        Some(unsafe {{ address as PFN_{name} }})",
                "    }",
                "}",
                "",
            ]
        )
    return "\n".join(lines)


def main() -> None:
    args = parse_args()
    root, lock = load_registry(args.registry)
    commands, enums = selected_names(root, lock["api"], lock["version"])
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_PATH.write_text(
        render(root, lock, commands, enums), encoding="utf-8", newline="\n"
    )
    generated_hash = hashlib.sha256(OUTPUT_PATH.read_bytes()).hexdigest()
    license_hash = hashlib.sha256((PACKAGE_ROOT / "LICENSE.txt").read_bytes()).hexdigest()
    CHECKSUM_PATH.write_text(
        f"{lock['sha256']}  gl.xml@{lock['commit']}\n"
        f"{generated_hash}  src/raw.reim\n"
        f"{license_hash}  LICENSE.txt\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"generated {OUTPUT_PATH} ({len(commands)} commands, {len(enums)} constants)")


if __name__ == "__main__":
    main()
