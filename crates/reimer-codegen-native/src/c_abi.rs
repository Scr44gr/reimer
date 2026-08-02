//! Target-specific lowering rules for C aggregates passed by value.

use std::str::FromStr;

use cranelift_codegen::ir;
use reimer_hir::{ExternFunction, TypeDefinition, TypeRepresentation};
use reimer_layout::{AggregateLayoutKind, Layouts, ValueLayout};
use reimer_types::Type;
use target_lexicon::{Aarch64Architecture, Architecture, OperatingSystem, Triple, Vendor};

const SYSV_INTEGER_ARGUMENTS: u8 = 6;
const SYSV_FLOAT_ARGUMENTS: u8 = 8;
const AAPCS64_INTEGER_ARGUMENTS: u8 = 8;
const AAPCS64_FLOAT_ARGUMENTS: u8 = 8;

/// One register-sized piece of a directly passed aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AggregateComponent {
    /// Byte offset in the aggregate's in-memory representation.
    pub(crate) offset: u32,
    /// Cranelift type used at the C call boundary.
    pub(crate) value_type: ir::Type,
}

/// How one source parameter is represented at the native call boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParameterPassing {
    /// Scalar values keep their ordinary Cranelift representation.
    Scalar,
    /// A small aggregate is split into one or more register-sized values.
    DirectAggregate {
        /// Synthetic scalar arguments that exhaust an ABI register class before a spill.
        padding: Vec<ir::Type>,
        /// Pieces loaded from the source aggregate.
        components: Vec<AggregateComponent>,
        /// Padded storage required to load every component without over-reading.
        storage_size: u32,
    },
    /// The caller copies the aggregate into aligned storage and passes its address.
    IndirectAggregate {
        /// Minimum alignment required for the caller-owned copy.
        minimum_alignment: u32,
    },
    /// A System V memory-class aggregate is copied directly into the outgoing stack area.
    StackAggregate {
        /// Size rounded to the System V eightbyte boundary.
        size: u32,
        /// Minimum alignment required for the caller-owned source copy.
        minimum_alignment: u32,
    },
}

/// How the source return value is represented at the native call boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnPassing {
    /// The function has no runtime return value.
    Unit,
    /// Scalar values keep their ordinary Cranelift representation.
    Scalar,
    /// A small aggregate is returned in one or more registers.
    DirectAggregate {
        /// Pieces stored into the returned aggregate.
        components: Vec<AggregateComponent>,
        /// Padded storage required to store every component.
        storage_size: u32,
    },
    /// The caller supplies an aligned structure-return destination.
    IndirectAggregate {
        /// Minimum alignment required for the return destination.
        minimum_alignment: u32,
    },
}

/// Fully classified ABI behavior for one external function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionAbi {
    /// One lowering rule per source parameter.
    pub(crate) parameters: Vec<ParameterPassing>,
    /// Lowering rule for the source return value.
    pub(crate) return_value: ReturnPassing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CAbi {
    MicrosoftX64,
    SystemVx64,
    Aapcs64,
    AppleAarch64,
}

#[derive(Debug, Default)]
struct RegisterState {
    integers: u8,
    floats: u8,
}

/// Classifies a validated external function for the requested target C ABI.
pub(crate) fn classify_function(
    function: &ExternFunction,
    definitions: &[TypeDefinition],
    layouts: &Layouts,
    target: &str,
) -> Result<FunctionAbi, String> {
    let target_abi = CAbi::from_target(target)?;
    let return_value = classify_return(function.return_type, definitions, layouts, target_abi)?;
    let mut registers = RegisterState::default();
    if target_abi == CAbi::SystemVx64
        && matches!(return_value, ReturnPassing::IndirectAggregate { .. })
    {
        registers.integers = 1;
    }
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| {
            classify_parameter(
                parameter.ty,
                definitions,
                layouts,
                target_abi,
                &mut registers,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(FunctionAbi {
        parameters,
        return_value,
    })
}

impl CAbi {
    fn from_target(target: &str) -> Result<Self, String> {
        let triple = Triple::from_str(target)
            .map_err(|error| format!("cannot parse target triple `{target}`: {error}"))?;
        match triple.architecture {
            Architecture::X86_64 | Architecture::X86_64h => {
                if triple.operating_system == OperatingSystem::Windows {
                    Ok(Self::MicrosoftX64)
                } else {
                    Ok(Self::SystemVx64)
                }
            }
            Architecture::Aarch64(Aarch64Architecture::Aarch64) => {
                if triple.vendor == Vendor::Apple {
                    Ok(Self::AppleAarch64)
                } else {
                    Ok(Self::Aapcs64)
                }
            }
            Architecture::Aarch64(Aarch64Architecture::Aarch64be) => Err(format!(
                "by-value `@repr(C)` aggregates are not implemented for big-endian target `{target}`"
            )),
            _ => Err(format!(
                "by-value `@repr(C)` aggregates are not implemented for target `{target}`"
            )),
        }
    }
}

fn classify_return(
    ty: Type,
    definitions: &[TypeDefinition],
    layouts: &Layouts,
    target: CAbi,
) -> Result<ReturnPassing, String> {
    if ty == Type::Unit {
        return Ok(ReturnPassing::Unit);
    }
    if !is_c_struct(ty, definitions)? {
        return Ok(ReturnPassing::Scalar);
    }
    let layout = layouts.value_layout(ty)?;
    match target {
        CAbi::MicrosoftX64 => match microsoft_x64_component(layout.size) {
            Some(component) => Ok(ReturnPassing::DirectAggregate {
                components: vec![component],
                storage_size: layout.size,
            }),
            None => Ok(ReturnPassing::IndirectAggregate {
                minimum_alignment: 16,
            }),
        },
        CAbi::SystemVx64 => {
            let classes = classify_system_v_aggregate(ty, layouts)?;
            if let Some(classes) = classes {
                let components = system_v_components(&classes, layout.size);
                Ok(ReturnPassing::DirectAggregate {
                    storage_size: component_storage_size(layout.size, &components),
                    components,
                })
            } else {
                Ok(ReturnPassing::IndirectAggregate {
                    minimum_alignment: layout.align,
                })
            }
        }
        CAbi::Aapcs64 | CAbi::AppleAarch64 => {
            if let Some(elements) = homogeneous_float_aggregate(ty, layouts)? {
                return Ok(ReturnPassing::DirectAggregate {
                    storage_size: component_storage_size(layout.size, &elements),
                    components: elements,
                });
            }
            if layout.size <= 16 {
                let components = integer_chunks(layout.size);
                Ok(ReturnPassing::DirectAggregate {
                    storage_size: component_storage_size(layout.size, &components),
                    components,
                })
            } else {
                Ok(ReturnPassing::IndirectAggregate {
                    minimum_alignment: layout.align,
                })
            }
        }
    }
}

fn classify_parameter(
    ty: Type,
    definitions: &[TypeDefinition],
    layouts: &Layouts,
    target: CAbi,
    registers: &mut RegisterState,
) -> Result<ParameterPassing, String> {
    if !is_c_struct(ty, definitions)? {
        consume_scalar_register(ty, target, registers);
        return Ok(ParameterPassing::Scalar);
    }
    let layout = layouts.value_layout(ty)?;
    match target {
        CAbi::MicrosoftX64 => Ok(match microsoft_x64_component(layout.size) {
            Some(component) => ParameterPassing::DirectAggregate {
                components: vec![component],
                padding: Vec::new(),
                storage_size: layout.size,
            },
            None => ParameterPassing::IndirectAggregate {
                minimum_alignment: 16,
            },
        }),
        CAbi::SystemVx64 => classify_system_v_parameter(ty, layouts, layout, registers),
        CAbi::Aapcs64 | CAbi::AppleAarch64 => {
            classify_aapcs64_parameter(ty, layouts, layout, registers, target)
        }
    }
}

fn consume_scalar_register(ty: Type, target: CAbi, registers: &mut RegisterState) {
    match target {
        CAbi::MicrosoftX64 => {}
        CAbi::SystemVx64 => {
            if ty.is_float() {
                registers.floats = registers.floats.saturating_add(1).min(SYSV_FLOAT_ARGUMENTS);
            } else {
                registers.integers = registers
                    .integers
                    .saturating_add(1)
                    .min(SYSV_INTEGER_ARGUMENTS);
            }
        }
        CAbi::Aapcs64 | CAbi::AppleAarch64 => {
            if ty.is_float() {
                registers.floats = registers
                    .floats
                    .saturating_add(1)
                    .min(AAPCS64_FLOAT_ARGUMENTS);
            } else {
                registers.integers = registers
                    .integers
                    .saturating_add(1)
                    .min(AAPCS64_INTEGER_ARGUMENTS);
            }
        }
    }
}

fn microsoft_x64_component(size: u32) -> Option<AggregateComponent> {
    let value_type = match size {
        1 => ir::types::I8,
        2 => ir::types::I16,
        4 => ir::types::I32,
        8 => ir::types::I64,
        _ => return None,
    };
    Some(AggregateComponent {
        offset: 0,
        value_type,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemVClass {
    Integer,
    Sse,
}

fn classify_system_v_parameter(
    ty: Type,
    layouts: &Layouts,
    layout: ValueLayout,
    registers: &mut RegisterState,
) -> Result<ParameterPassing, String> {
    let Some(classes) = classify_system_v_aggregate(ty, layouts)? else {
        return Ok(ParameterPassing::StackAggregate {
            size: align_up(layout.size, 8)?,
            minimum_alignment: layout.align.max(8),
        });
    };
    let integer_count = count_class(&classes, SystemVClass::Integer);
    let float_count = count_class(&classes, SystemVClass::Sse);
    if registers.integers.saturating_add(integer_count) > SYSV_INTEGER_ARGUMENTS
        || registers.floats.saturating_add(float_count) > SYSV_FLOAT_ARGUMENTS
    {
        return Ok(ParameterPassing::StackAggregate {
            size: align_up(layout.size, 8)?,
            minimum_alignment: layout.align.max(8),
        });
    }
    registers.integers += integer_count;
    registers.floats += float_count;
    let components = system_v_components(&classes, layout.size);
    Ok(ParameterPassing::DirectAggregate {
        padding: Vec::new(),
        storage_size: component_storage_size(layout.size, &components),
        components,
    })
}

fn classify_system_v_aggregate(
    ty: Type,
    layouts: &Layouts,
) -> Result<Option<Vec<SystemVClass>>, String> {
    let layout = layouts.value_layout(ty)?;
    if layout.size == 0 || layout.size > 16 {
        return Ok(None);
    }
    let class_count = usize::try_from(layout.size.div_ceil(8))
        .map_err(|_| "System V aggregate class count exceeds usize".to_owned())?;
    let mut classes = vec![None; class_count];
    if !classify_system_v_value(ty, 0, layouts, &mut classes)? {
        return Ok(None);
    }
    classes
        .into_iter()
        .map(|class| {
            class.ok_or_else(|| "System V aggregate contains an unclassified eightbyte".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn classify_system_v_value(
    ty: Type,
    offset: u32,
    layouts: &Layouts,
    classes: &mut [Option<SystemVClass>],
) -> Result<bool, String> {
    let layout = layouts.value_layout(ty)?;
    if !offset.is_multiple_of(layout.align) {
        return Ok(false);
    }
    if let Some(fields) = layouts.product_fields(ty) {
        let aggregate = layouts.aggregate(ty)?;
        let AggregateLayoutKind::Product { offsets } = &aggregate.kind else {
            return Err("product metadata differs from its aggregate layout".to_owned());
        };
        for (field, field_offset) in fields.iter().zip(offsets) {
            if !classify_system_v_value(
                *field,
                offset
                    .checked_add(*field_offset)
                    .ok_or_else(|| "System V field offset overflowed".to_owned())?,
                layouts,
                classes,
            )? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if let Some((element, length, stride)) = layouts.array_shape(ty) {
        for index in 0..length {
            let element_offset = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(stride))
                .and_then(|element_offset| offset.checked_add(element_offset))
                .ok_or_else(|| "System V array element offset overflowed".to_owned())?;
            if !classify_system_v_value(element, element_offset, layouts, classes)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    let class = if ty.is_float() {
        SystemVClass::Sse
    } else {
        SystemVClass::Integer
    };
    merge_system_v_class(classes, offset, layout.size, class)?;
    Ok(true)
}

fn merge_system_v_class(
    classes: &mut [Option<SystemVClass>],
    offset: u32,
    size: u32,
    incoming: SystemVClass,
) -> Result<(), String> {
    if size == 0 {
        return Ok(());
    }
    let first =
        usize::try_from(offset / 8).map_err(|_| "System V class index exceeds usize".to_owned())?;
    let last_byte = offset
        .checked_add(size - 1)
        .ok_or_else(|| "System V class range overflowed".to_owned())?;
    let last = usize::try_from(last_byte / 8)
        .map_err(|_| "System V class index exceeds usize".to_owned())?;
    for index in first..=last {
        let slot = classes
            .get_mut(index)
            .ok_or_else(|| "System V class range exceeds the aggregate".to_owned())?;
        *slot = Some(match (*slot, incoming) {
            (Some(SystemVClass::Integer), _) | (_, SystemVClass::Integer) => SystemVClass::Integer,
            _ => SystemVClass::Sse,
        });
    }
    Ok(())
}

fn system_v_components(classes: &[SystemVClass], size: u32) -> Vec<AggregateComponent> {
    classes
        .iter()
        .enumerate()
        .map(|(index, class)| {
            let offset = u32::try_from(index).unwrap_or(0) * 8;
            let bytes = size.saturating_sub(offset).min(8);
            let value_type = match class {
                SystemVClass::Integer => integer_type(bytes),
                SystemVClass::Sse if bytes <= 4 => ir::types::F32,
                SystemVClass::Sse => ir::types::F64,
            };
            AggregateComponent { offset, value_type }
        })
        .collect()
}

fn count_class(classes: &[SystemVClass], expected: SystemVClass) -> u8 {
    u8::try_from(classes.iter().filter(|class| **class == expected).count()).unwrap_or(u8::MAX)
}

fn classify_aapcs64_parameter(
    ty: Type,
    layouts: &Layouts,
    layout: ValueLayout,
    registers: &mut RegisterState,
    target: CAbi,
) -> Result<ParameterPassing, String> {
    if let Some(components) = homogeneous_float_aggregate(ty, layouts)? {
        let required = u8::try_from(components.len())
            .map_err(|_| "AArch64 HFA element count exceeds u8".to_owned())?;
        if registers.floats.saturating_add(required) <= AAPCS64_FLOAT_ARGUMENTS {
            registers.floats += required;
            return Ok(ParameterPassing::DirectAggregate {
                padding: Vec::new(),
                storage_size: component_storage_size(layout.size, &components),
                components,
            });
        }
        let remaining = AAPCS64_FLOAT_ARGUMENTS.saturating_sub(registers.floats);
        registers.floats = AAPCS64_FLOAT_ARGUMENTS;
        let value_type = components
            .first()
            .map(|component| component.value_type)
            .ok_or_else(|| "AArch64 HFA has no components".to_owned())?;
        let packed = packed_float_chunks(layout.size);
        return Ok(ParameterPassing::DirectAggregate {
            padding: vec![value_type; usize::from(remaining)],
            storage_size: component_storage_size(layout.size, &packed),
            components: packed,
        });
    }
    if layout.size > 16 {
        consume_integer_register(registers, AAPCS64_INTEGER_ARGUMENTS);
        return Ok(ParameterPassing::IndirectAggregate {
            minimum_alignment: layout.align.max(8),
        });
    }
    let components = integer_chunks(layout.size);
    let required = u8::try_from(components.len())
        .map_err(|_| "AArch64 aggregate chunk count exceeds u8".to_owned())?;
    let mut padding = Vec::new();
    if target != CAbi::AppleAarch64 && layout.align >= 16 && registers.integers % 2 == 1 {
        padding.push(ir::types::I64);
        consume_integer_register(registers, AAPCS64_INTEGER_ARGUMENTS);
    }
    if registers.integers.saturating_add(required) <= AAPCS64_INTEGER_ARGUMENTS {
        registers.integers += required;
    } else {
        let remaining = AAPCS64_INTEGER_ARGUMENTS.saturating_sub(registers.integers);
        padding.extend(std::iter::repeat_n(ir::types::I64, usize::from(remaining)));
        registers.integers = AAPCS64_INTEGER_ARGUMENTS;
    }
    Ok(ParameterPassing::DirectAggregate {
        padding,
        storage_size: component_storage_size(layout.size, &components),
        components,
    })
}

fn consume_integer_register(registers: &mut RegisterState, maximum: u8) {
    registers.integers = registers.integers.saturating_add(1).min(maximum);
}

fn homogeneous_float_aggregate(
    ty: Type,
    layouts: &Layouts,
) -> Result<Option<Vec<AggregateComponent>>, String> {
    let mut elements = Vec::new();
    if !collect_float_elements(ty, 0, layouts, &mut elements)? || elements.is_empty() {
        return Ok(None);
    }
    if elements.len() > 4
        || elements
            .iter()
            .any(|element| element.value_type != elements[0].value_type)
    {
        return Ok(None);
    }
    Ok(Some(elements))
}

fn collect_float_elements(
    ty: Type,
    offset: u32,
    layouts: &Layouts,
    elements: &mut Vec<AggregateComponent>,
) -> Result<bool, String> {
    if ty == Type::F32 || ty == Type::F64 {
        elements.push(AggregateComponent {
            offset,
            value_type: if ty == Type::F32 {
                ir::types::F32
            } else {
                ir::types::F64
            },
        });
        return Ok(true);
    }
    if let Some(fields) = layouts.product_fields(ty) {
        let aggregate = layouts.aggregate(ty)?;
        let AggregateLayoutKind::Product { offsets } = &aggregate.kind else {
            return Err("product metadata differs from its aggregate layout".to_owned());
        };
        for (field, field_offset) in fields.iter().zip(offsets) {
            if !collect_float_elements(
                *field,
                offset
                    .checked_add(*field_offset)
                    .ok_or_else(|| "AArch64 HFA field offset overflowed".to_owned())?,
                layouts,
                elements,
            )? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if let Some((element, length, stride)) = layouts.array_shape(ty) {
        for index in 0..length {
            let element_offset = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(stride))
                .and_then(|element_offset| offset.checked_add(element_offset))
                .ok_or_else(|| "AArch64 HFA element offset overflowed".to_owned())?;
            if !collect_float_elements(element, element_offset, layouts, elements)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    Ok(false)
}

fn integer_chunks(size: u32) -> Vec<AggregateComponent> {
    (0..size.div_ceil(8))
        .map(|index| {
            let offset = index * 8;
            AggregateComponent {
                offset,
                value_type: integer_type(size.saturating_sub(offset).min(8)),
            }
        })
        .collect()
}

fn packed_float_chunks(size: u32) -> Vec<AggregateComponent> {
    (0..size.div_ceil(8))
        .map(|index| {
            let offset = index * 8;
            AggregateComponent {
                offset,
                value_type: if size.saturating_sub(offset) <= 4 {
                    ir::types::F32
                } else {
                    ir::types::F64
                },
            }
        })
        .collect()
}

fn integer_type(bytes: u32) -> ir::Type {
    match bytes {
        0 | 1 => ir::types::I8,
        2 => ir::types::I16,
        3 | 4 => ir::types::I32,
        _ => ir::types::I64,
    }
}

fn component_storage_size(size: u32, components: &[AggregateComponent]) -> u32 {
    components.iter().fold(size.max(1), |storage, component| {
        storage.max(component.offset + component.value_type.bytes())
    })
}

fn align_up(value: u32, alignment: u32) -> Result<u32, String> {
    let mask = alignment
        .checked_sub(1)
        .ok_or_else(|| "ABI alignment cannot be zero".to_owned())?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or_else(|| "ABI storage size overflowed".to_owned())
}

fn is_c_struct(ty: Type, definitions: &[TypeDefinition]) -> Result<bool, String> {
    let Type::Struct(id) = ty else {
        return Ok(false);
    };
    let index = usize::try_from(id.0)
        .map_err(|_| format!("type definition {} exceeds the host index range", id.0))?;
    let definition = definitions
        .get(index)
        .ok_or_else(|| format!("type definition {} is missing", id.0))?;
    Ok(definition.representation == TypeRepresentation::C)
}

#[cfg(test)]
mod tests {
    use reimer_diagnostics::Span;
    use reimer_hir::{
        FunctionId, LocalId, Parameter, TypeDefinitionKind, TypeField, TypeRepresentation,
    };
    use reimer_types::TypeId;

    use super::*;

    const WINDOWS_X64: &str = "x86_64-pc-windows-msvc";
    const LINUX_X64: &str = "x86_64-unknown-linux-gnu";
    const LINUX_ARM64: &str = "aarch64-unknown-linux-gnu";
    const MACOS_ARM64: &str = "aarch64-apple-darwin";

    fn c_struct(id: u32, fields: &[Type], alignment: Option<u32>) -> TypeDefinition {
        TypeDefinition {
            id: TypeId(id),
            name: Some(format!("Struct{id}")),
            documentation: None,
            kind: TypeDefinitionKind::Struct {
                fields: fields
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| TypeField {
                        name: format!("field_{index}"),
                        is_public: true,
                        ty: *ty,
                        span: Span::empty(0),
                    })
                    .collect(),
            },
            representation: TypeRepresentation::C,
            alignment,
            derives: Vec::new(),
            marker_traits: Vec::new(),
            must_use: false,
            span: Span::empty(0),
        }
    }

    fn external_function(parameters: &[Type], return_type: Type) -> ExternFunction {
        ExternFunction {
            id: FunctionId(0),
            name: "external".to_owned(),
            symbol: "external".to_owned(),
            link: None,
            is_public: false,
            abi: "C".to_owned(),
            parameters: parameters
                .iter()
                .enumerate()
                .map(|(index, ty)| Parameter {
                    local: LocalId(u32::try_from(index).expect("test parameter index fits u32")),
                    name: format!("parameter_{index}"),
                    ty: *ty,
                    span: Span::empty(0),
                })
                .collect(),
            return_type,
            span: Span::empty(0),
        }
    }

    #[test]
    fn microsoft_x64_should_pass_only_one_two_four_and_eight_byte_structs_directly() {
        let definitions = [
            c_struct(0, &[Type::U64], None),
            c_struct(1, &[Type::U64, Type::U64], None),
        ];
        let layouts = Layouts::build(&definitions).expect("valid C layouts");
        let function = external_function(
            &[Type::Struct(TypeId(0)), Type::Struct(TypeId(1))],
            Type::Struct(TypeId(1)),
        );

        let abi = classify_function(&function, &definitions, &layouts, WINDOWS_X64)
            .expect("supported Microsoft x64 ABI");

        assert!(matches!(
            &abi.parameters[0],
            ParameterPassing::DirectAggregate { components, .. }
                if components == &[AggregateComponent { offset: 0, value_type: ir::types::I64 }]
        ));
        assert!(matches!(
            &abi.parameters[1],
            ParameterPassing::IndirectAggregate {
                minimum_alignment: 16
            }
        ));
        assert!(matches!(
            abi.return_value,
            ReturnPassing::IndirectAggregate {
                minimum_alignment: 16
            }
        ));
    }

    #[test]
    fn system_v_x64_should_split_mixed_structs_between_sse_and_integer_registers() {
        let definitions = [c_struct(0, &[Type::F64, Type::U64], None)];
        let layouts = Layouts::build(&definitions).expect("valid C layout");
        let function = external_function(&[Type::Struct(TypeId(0))], Type::Struct(TypeId(0)));

        let abi = classify_function(&function, &definitions, &layouts, LINUX_X64)
            .expect("supported System V x64 ABI");
        let expected = [
            AggregateComponent {
                offset: 0,
                value_type: ir::types::F64,
            },
            AggregateComponent {
                offset: 8,
                value_type: ir::types::I64,
            },
        ];

        assert!(matches!(
            &abi.parameters[0],
            ParameterPassing::DirectAggregate { components, .. } if components == &expected
        ));
        assert!(matches!(
            abi.return_value,
            ReturnPassing::DirectAggregate { components, .. } if components == expected
        ));
    }

    #[test]
    fn system_v_x64_should_roll_back_an_aggregate_when_integer_registers_are_exhausted() {
        let definitions = [
            c_struct(0, &[Type::U64, Type::U64], None),
            c_struct(1, &[Type::U64], None),
        ];
        let layouts = Layouts::build(&definitions).expect("valid C layouts");
        let function = external_function(
            &[
                Type::U64,
                Type::U64,
                Type::U64,
                Type::U64,
                Type::U64,
                Type::Struct(TypeId(0)),
                Type::Struct(TypeId(1)),
            ],
            Type::Unit,
        );

        let abi = classify_function(&function, &definitions, &layouts, LINUX_X64)
            .expect("supported System V x64 ABI");

        assert!(matches!(
            abi.parameters[5],
            ParameterPassing::StackAggregate { size: 16, .. }
        ));
        assert!(matches!(
            abi.parameters[6],
            ParameterPassing::DirectAggregate { .. }
        ));
    }

    #[test]
    fn system_v_x64_should_pass_aggregates_larger_than_two_eightbytes_on_the_stack() {
        let definitions = [c_struct(0, &[Type::U64, Type::U64, Type::U64], None)];
        let layouts = Layouts::build(&definitions).expect("valid C layout");
        let function = external_function(&[Type::Struct(TypeId(0))], Type::Unit);

        let abi = classify_function(&function, &definitions, &layouts, LINUX_X64)
            .expect("supported System V x64 ABI");

        assert!(matches!(
            abi.parameters[0],
            ParameterPassing::StackAggregate {
                size: 24,
                minimum_alignment: 8
            }
        ));
    }

    #[test]
    fn aapcs64_should_pass_homogeneous_float_aggregates_in_simd_registers() {
        let definitions = [c_struct(
            0,
            &[Type::F32, Type::F32, Type::F32, Type::F32],
            None,
        )];
        let layouts = Layouts::build(&definitions).expect("valid C layout");
        let function = external_function(&[Type::Struct(TypeId(0))], Type::Struct(TypeId(0)));

        let abi = classify_function(&function, &definitions, &layouts, LINUX_ARM64)
            .expect("supported AAPCS64 ABI");

        assert!(matches!(
            &abi.parameters[0],
            ParameterPassing::DirectAggregate { padding, components, .. }
                if padding.is_empty()
                    && components.len() == 4
                    && components.iter().all(|component| component.value_type == ir::types::F32)
        ));
        assert!(matches!(
            abi.return_value,
            ReturnPassing::DirectAggregate { components, .. }
                if components.len() == 4
                    && components.iter().all(|component| component.value_type == ir::types::F32)
        ));
    }

    #[test]
    fn aapcs64_should_spill_an_entire_hfa_when_simd_registers_are_exhausted() {
        let definitions = [c_struct(0, &[Type::F32, Type::F32, Type::F32], None)];
        let layouts = Layouts::build(&definitions).expect("valid C layout");
        let function = external_function(
            &[
                Type::F64,
                Type::F64,
                Type::F64,
                Type::F64,
                Type::F64,
                Type::F64,
                Type::F64,
                Type::Struct(TypeId(0)),
            ],
            Type::Unit,
        );

        let abi = classify_function(&function, &definitions, &layouts, LINUX_ARM64)
            .expect("supported AAPCS64 ABI");

        assert!(matches!(
            &abi.parameters[7],
            ParameterPassing::DirectAggregate { padding, components, storage_size: 12 }
                if padding == &[ir::types::F32]
                    && components == &[
                        AggregateComponent { offset: 0, value_type: ir::types::F64 },
                        AggregateComponent { offset: 8, value_type: ir::types::F32 },
                    ]
        ));
    }

    #[test]
    fn apple_aarch64_should_not_apply_the_generic_even_register_rule() {
        let definitions = [c_struct(0, &[Type::U64, Type::U64], Some(16))];
        let layouts = Layouts::build(&definitions).expect("valid aligned C layout");
        let function = external_function(&[Type::U64, Type::Struct(TypeId(0))], Type::Unit);

        let generic = classify_function(&function, &definitions, &layouts, LINUX_ARM64)
            .expect("supported generic AAPCS64 ABI");
        let apple = classify_function(&function, &definitions, &layouts, MACOS_ARM64)
            .expect("supported Apple AArch64 ABI");

        assert!(matches!(
            &generic.parameters[1],
            ParameterPassing::DirectAggregate { padding, .. } if padding == &[ir::types::I64]
        ));
        assert!(matches!(
            &apple.parameters[1],
            ParameterPassing::DirectAggregate { padding, .. } if padding.is_empty()
        ));
    }

    #[test]
    fn aapcs64_should_pass_aggregates_larger_than_sixteen_bytes_indirectly() {
        let definitions = [c_struct(0, &[Type::U64, Type::U64, Type::U64], None)];
        let layouts = Layouts::build(&definitions).expect("valid C layout");
        let function = external_function(&[Type::Struct(TypeId(0))], Type::Struct(TypeId(0)));

        let abi = classify_function(&function, &definitions, &layouts, LINUX_ARM64)
            .expect("supported AAPCS64 ABI");

        assert!(matches!(
            abi.parameters[0],
            ParameterPassing::IndirectAggregate {
                minimum_alignment: 8
            }
        ));
        assert!(matches!(
            abi.return_value,
            ReturnPassing::IndirectAggregate {
                minimum_alignment: 8
            }
        ));
    }
}
