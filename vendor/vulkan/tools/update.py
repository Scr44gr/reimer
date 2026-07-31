#!/usr/bin/env python3
"""Generate Reimer Vulkan bindings from a pinned Khronos registry snapshot."""

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

BUILTIN_TYPES = {
    "void": "()",
    "char": "u8",
    "float": "f32",
    "double": "f64",
    "int8_t": "i8",
    "uint8_t": "u8",
    "int16_t": "i16",
    "uint16_t": "u16",
    "int32_t": "i32",
    "uint32_t": "u32",
    "int64_t": "i64",
    "uint64_t": "u64",
    "size_t": "usize",
    "PFN_vkVoidFunction": "*const u8",
}

ACTUAL_STRUCTS = {
    "VkApplicationInfo",
    "VkInstanceCreateInfo",
    "VkExtensionProperties",
    "VkLayerProperties",
}

GLOBAL_COMMANDS = {
    "vkCreateInstance",
    "vkEnumerateInstanceExtensionProperties",
    "vkEnumerateInstanceLayerProperties",
    "vkEnumerateInstanceVersion",
}

DEVICE_DISPATCH = {"VkDevice", "VkQueue", "VkCommandBuffer"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=Path,
        help="Use a local vk.xml after verifying it against registry.lock.json.",
    )
    return parser.parse_args()


def load_registry(path: Path | None) -> tuple[ET.Element, dict[str, str]]:
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    if path is None:
        url = (
            f"https://raw.githubusercontent.com/KhronosGroup/Vulkan-Headers/"
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


def includes_api(value: str | None, api: str) -> bool:
    return value is None or api in value.split(",")


def version_key(value: str) -> tuple[int, int]:
    major, minor, *_ = value.split(".")
    return int(major), int(minor)


def selected_names(
    root: ET.Element, api: str, maximum: str
) -> tuple[list[str], list[str]]:
    commands: OrderedDict[str, None] = OrderedDict()
    enums: OrderedDict[str, None] = OrderedDict()
    maximum_key = version_key(maximum)
    for feature in root.findall("feature"):
        if not includes_api(feature.get("api"), api):
            continue
        if version_key(feature.get("number", "0.0")) > maximum_key:
            continue
        for require in feature.findall("require"):
            if not includes_api(require.get("api"), api):
                continue
            for command in require.findall("command"):
                commands[command.get("name", "")] = None
            for enum in require.findall("enum"):
                enums[enum.get("name", "")] = None
    return list(commands), list(enums)


def source_types(root: ET.Element) -> dict[str, ET.Element]:
    result: dict[str, ET.Element] = {}
    for element in root.findall("./types/type"):
        name = element.get("name") or element.findtext("name")
        if name:
            result[name] = element
    return result


def public_name(name: str) -> str:
    if name == "VkResult":
        return "ResultCode"
    return name.removeprefix("Vk")


def scalar_type(name: str, types: dict[str, ET.Element]) -> str:
    if name in BUILTIN_TYPES:
        return BUILTIN_TYPES[name]
    element = types.get(name)
    if element is None:
        raise ValueError(f"unsupported Vulkan type {name!r}")
    category = element.get("category")
    if category == "handle":
        declaration = "".join(element.itertext())
        if "NON_DISPATCHABLE" in declaration:
            return "u64"
        return "*mut ()"
    if category == "enum":
        return "i32"
    if category == "bitmask":
        declaration = "".join(element.itertext())
        return "u64" if "VkFlags64" in declaration else "u32"
    if category == "basetype":
        declaration = "".join(element.itertext())
        for builtin, mapped in BUILTIN_TYPES.items():
            if re.search(rf"\b{re.escape(builtin)}\b", declaration):
                return mapped
    if category in {"struct", "union"}:
        return public_name(name)
    raise ValueError(
        f"unsupported Vulkan type {name!r} with category {category!r}"
    )


def declaration_type(element: ET.Element, types: dict[str, ET.Element]) -> str:
    name = element.findtext("name")
    declaration = "".join(element.itertext())
    if name:
        declaration = declaration.replace(name, "", 1)
    base = element.findtext("type") or "void"
    mapped = public_name(base) if base.startswith("Vk") and base in types else scalar_type(base, types)
    stars = declaration.count("*")
    if "[" in declaration:
        stars += 1
    immutable = "const" in declaration
    for _ in range(stars):
        mapped = f"*{'const' if immutable else 'mut'} {mapped}"
    return mapped


def command_elements(root: ET.Element) -> tuple[dict[str, ET.Element], dict[str, str]]:
    commands: dict[str, ET.Element] = {}
    aliases: dict[str, str] = {}
    for command in root.findall("./commands/command"):
        name = command.findtext("proto/name")
        if name:
            commands[name] = command
        elif command.get("name") and command.get("alias"):
            aliases[command.get("name", "")] = command.get("alias", "")
    return commands, aliases


def command_signatures(
    root: ET.Element, names: list[str], types: dict[str, ET.Element]
) -> list[tuple[str, list[str], str, str | None]]:
    available, aliases = command_elements(root)
    signatures = []
    for name in names:
        command = available.get(name)
        if command is None:
            command = available[aliases[name]]
        parameters = [
            declaration_type(parameter, types)
            for parameter in command.findall("param")
        ]
        if name in {"vkGetInstanceProcAddr", "vkGetDeviceProcAddr"}:
            parameters[1] = "cstr"
        return_name = command.findtext("proto/type") or "void"
        returns = (
            public_name(return_name)
            if return_name.startswith("Vk") and return_name in types
            else scalar_type(return_name, types)
        )
        first = command.findtext("param/type")
        signatures.append((name, parameters, returns, first))
    return signatures


def enum_catalog(
    root: ET.Element, selected: list[str]
) -> tuple[OrderedDict[str, ET.Element], dict[str, str]]:
    definitions: dict[str, ET.Element] = {}
    groups: dict[str, str] = {}
    ordered: OrderedDict[str, ET.Element] = OrderedDict()
    for group in root.findall("enums"):
        group_name = group.get("name", "")
        group_kind = group.get("type", "constants")
        for enum in group.findall("enum"):
            name = enum.get("name")
            if not name:
                continue
            definitions.setdefault(name, enum)
            groups.setdefault(name, group_name if group_kind != "constants" else "")
            if enum.get("extnumber") is None:
                ordered[name] = enum
    for enum in root.findall(".//require/enum"):
        name = enum.get("name")
        if not name:
            continue
        if any(enum.get(key) is not None for key in ("value", "bitpos", "alias", "offset")):
            definitions[name] = enum
        if enum.get("extends"):
            groups[name] = enum.get("extends", "")
    included: OrderedDict[str, ET.Element] = OrderedDict()
    for name, enum in ordered.items():
        included[name] = definitions[name]
    for name in selected:
        if name in definitions:
            included[name] = definitions[name]
    return included, groups


def enum_value(
    name: str,
    definitions: dict[str, ET.Element],
    seen: set[str] | None = None,
) -> int | float:
    seen = set() if seen is None else seen
    if name in seen:
        raise ValueError(f"cyclic enum alias involving {name}")
    seen.add(name)
    enum = definitions[name]
    alias = enum.get("alias")
    if alias:
        return enum_value(alias, definitions, seen)
    bitpos = enum.get("bitpos")
    if bitpos is not None:
        return 1 << int(bitpos)
    offset = enum.get("offset")
    if offset is not None:
        extension = int(enum.get("extnumber") or "0")
        value = 1_000_000_000 + (extension - 1) * 1000 + int(offset)
        return -value if enum.get("dir") == "-" else value
    value = enum.get("value")
    if value is None:
        raise ValueError(f"Vulkan enum {name} has no numeric value")
    if re.fullmatch(r"-?\d+(\.\d+)?[fF]", value):
        return float(value[:-1])
    if value.startswith("(~"):
        bits = 64 if "LL" in value.upper() else 32
        inner = re.search(r"~\s*(0x[0-9A-Fa-f]+|\d+)", value)
        if inner is None:
            raise ValueError(f"unsupported Vulkan enum expression {value!r}")
        return (~int(inner.group(1), 0)) & ((1 << bits) - 1)
    normalized = re.sub(r"[uUlL]+$", "", value.strip())
    return int(normalized, 0)


def constant_type(
    name: str, value: int | float, enum: ET.Element, groups: dict[str, str]
) -> str:
    if isinstance(value, float) or enum.get("type") == "float":
        return "f32"
    explicit = enum.get("type")
    if explicit in {"uint64_t", "VkDeviceSize"} or value > 0xFFFF_FFFF:
        return "u64"
    if explicit == "uint32_t":
        return "u32"
    group = groups.get(name, "")
    if group.endswith("FlagBits2") or group.endswith("Flags2"):
        return "u64"
    if "FlagBits" in group or value >= 0:
        return "u32" if "FlagBits" in group or not group else "i32"
    return "i32"


def render_literal(value: int | float, ty: str) -> str:
    if ty == "f32":
        return f"{value:.1f}"
    if ty == "i32":
        return str(value)
    width = 16 if ty == "u64" else 8
    return f"0x{value:0{width}x}"


def render_structs(lines: list[str], used: set[str], types: dict[str, ET.Element]) -> None:
    for name in sorted(used):
        category = types[name].get("category")
        if category not in {"struct", "union"} or name in ACTUAL_STRUCTS:
            continue
        lines.extend(
            [
                f"/// Opaque registry type `{name}`; use it through raw pointers.",
                f"pub struct {public_name(name)} {{",
                "    _opaque: u8,",
                "}",
                "",
            ]
        )
    lines.extend(
        [
            "@repr(C)",
            "pub struct ApplicationInfo {",
            "    pub structure_type: StructureType,",
            "    pub next: *const (),",
            "    pub application_name: cstr,",
            "    pub application_version: u32,",
            "    pub engine_name: cstr,",
            "    pub engine_version: u32,",
            "    pub api_version: u32,",
            "}",
            "",
            "@repr(C)",
            "pub struct InstanceCreateInfo {",
            "    pub structure_type: StructureType,",
            "    pub next: *const (),",
            "    pub flags: InstanceCreateFlags,",
            "    pub application_info: *const ApplicationInfo,",
            "    pub enabled_layer_count: u32,",
            "    pub enabled_layer_names: *const *const u8,",
            "    pub enabled_extension_count: u32,",
            "    pub enabled_extension_names: *const *const u8,",
            "}",
            "",
            "@repr(C)",
            "pub struct ExtensionProperties {",
            "    pub extension_name: [u8; 256],",
            "    pub spec_version: u32,",
            "}",
            "",
            "@repr(C)",
            "pub struct LayerProperties {",
            "    pub layer_name: [u8; 256],",
            "    pub spec_version: u32,",
            "    pub implementation_version: u32,",
            "    pub description: [u8; 256],",
            "}",
            "",
        ]
    )


def render_table(
    lines: list[str],
    table: str,
    signatures: list[tuple[str, list[str], str, str | None]],
    resolver: str,
    receiver: str,
) -> None:
    lines.extend([f"pub struct {table} {{"])
    for name, _, _, _ in signatures:
        lines.append(f"    pub {name}: Option<PFN_{name}>,")
    lines.extend(["}", "", f"impl {table} {{"])
    if table == "GlobalFunctions":
        lines.extend(
            [
                "    /// Loads global commands without creating an instance.",
                "    pub fn load(resolve: GetInstanceProcAddr) -> GlobalFunctions {",
                "        let instance = 0 as usize as Instance;",
                "        GlobalFunctions {",
            ]
        )
    elif table == "InstanceFunctions":
        lines.extend(
            [
                "    /// Loads commands dispatched by an instance or physical device.",
                "    pub fn load(",
                "        instance: Instance,",
                "        resolve: GetInstanceProcAddr,",
                "    ) -> InstanceFunctions {",
                "        InstanceFunctions {",
            ]
        )
    else:
        lines.extend(
            [
                "    /// Loads commands dispatched by a device, queue, or command buffer.",
                "    pub fn load(",
                "        device: Device,",
                "        resolve: PFN_vkGetDeviceProcAddr,",
                "    ) -> DeviceFunctions {",
                "        DeviceFunctions {",
            ]
        )
    for name, _, _, _ in signatures:
        lines.append(f"            {name}: load_{name}({resolver}, {receiver}),")
    lines.extend(["        }", "    }", "}", ""])


def render(
    root: ET.Element, lock: dict[str, str], command_names: list[str], enum_names: list[str]
) -> str:
    types = source_types(root)
    signatures = command_signatures(root, command_names, types)
    available, aliases_by_command = command_elements(root)
    used_types: set[str] = set()
    for name in command_names:
        command = available.get(name)
        if command is None:
            command = available[aliases_by_command[name]]
        for element in [command.find("proto"), *command.findall("param")]:
            if element is not None and element.findtext("type") in types:
                used_types.add(element.findtext("type") or "")
    used_types.update(ACTUAL_STRUCTS)
    constants, groups = enum_catalog(root, enum_names)
    definitions = dict(constants)
    lines = [
        "// Generated by tools/update.py. Do not edit by hand.",
        "// SPDX-License-Identifier: Apache-2.0 OR MIT",
        f"// Khronos Vulkan-Headers commit: {lock['commit']}",
        f"// vk.xml SHA-256: {lock['sha256']}",
        "",
        f"pub const CORE_FUNCTION_COUNT: usize = {len(signatures)};",
        f"pub const CORE_CONSTANT_COUNT: usize = {len(constants)};",
        "",
    ]
    aliases: OrderedDict[str, str] = OrderedDict()
    for name in sorted(used_types):
        category = types[name].get("category")
        if category in {"handle", "enum", "bitmask", "basetype"}:
            aliases[public_name(name)] = scalar_type(name, types)
    aliases.setdefault("Flags", "u32")
    aliases.setdefault("Flags64", "u64")
    aliases.setdefault("StructureType", "i32")
    aliases.setdefault("InstanceCreateFlags", "u32")
    for name, target in aliases.items():
        lines.append(f"pub type {name} = {target};")
    lines.extend(
        [
            "",
            "/// Platform callback matching `vkGetInstanceProcAddr`.",
            "pub type GetInstanceProcAddr = fn(Instance, cstr) -> *const u8;",
            "",
        ]
    )
    render_structs(lines, {name for name in used_types if name}, types)
    lines.append("// Constants from the Vulkan 1.4 core registry.")
    for name, enum in constants.items():
        try:
            value = enum_value(name, definitions)
        except ValueError:
            continue
        ty = constant_type(name, value, enum, groups)
        lines.append(f"pub const {name}: {ty} = {render_literal(value, ty)};")
    lines.extend(
        [
            "",
            "pub const API_VERSION_1_0: u32 = (1 << 22);",
            "pub const API_VERSION_1_1: u32 = (1 << 22) | (1 << 12);",
            "pub const API_VERSION_1_2: u32 = (1 << 22) | (2 << 12);",
            "pub const API_VERSION_1_3: u32 = (1 << 22) | (3 << 12);",
            "pub const API_VERSION_1_4: u32 = (1 << 22) | (4 << 12);",
            "",
            "// Typed command signatures from Vulkan core 1.0 through 1.4.",
        ]
    )
    for name, parameters, returns, _ in signatures:
        lines.append(
            f"pub type PFN_{name} = fn({', '.join(parameters)}) -> {returns};"
        )
    globals_ = [item for item in signatures if item[0] in GLOBAL_COMMANDS]
    instances = [
        item
        for item in signatures
        if item[0] not in GLOBAL_COMMANDS
        and item[0] != "vkGetInstanceProcAddr"
        and (item[0] == "vkGetDeviceProcAddr" or item[3] not in DEVICE_DISPATCH)
    ]
    devices = [
        item
        for item in signatures
        if item[0] not in {"vkGetDeviceProcAddr", "vkGetInstanceProcAddr"}
        and item[3] in DEVICE_DISPATCH
    ]
    lines.append("")
    render_table(lines, "GlobalFunctions", globals_, "resolve", "instance")
    render_table(lines, "InstanceFunctions", instances, "resolve", "instance")
    render_table(lines, "DeviceFunctions", devices, "resolve", "device")
    for name, _, _, _ in signatures:
        if name == "vkGetInstanceProcAddr":
            continue
        resolver_ty = (
            "PFN_vkGetDeviceProcAddr" if name not in GLOBAL_COMMANDS and any(
                item[0] == name for item in devices
            ) else "GetInstanceProcAddr"
        )
        handle_ty = "Device" if resolver_ty == "PFN_vkGetDeviceProcAddr" else "Instance"
        lines.extend(
            [
                f"fn load_{name}(",
                f"    resolve: {resolver_ty},",
                f"    owner: {handle_ty},",
                f") -> Option<PFN_{name}> {{",
                f"    let address = resolve(owner, c\"{name}\");",
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
    license_hash = hashlib.sha256((PACKAGE_ROOT / "LICENSE.md").read_bytes()).hexdigest()
    CHECKSUM_PATH.write_text(
        f"{lock['sha256']}  vk.xml@{lock['commit']}\n"
        f"{generated_hash}  src/raw.reim\n"
        f"{license_hash}  LICENSE.md\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"generated {OUTPUT_PATH} ({len(commands)} commands)")


if __name__ == "__main__":
    main()
