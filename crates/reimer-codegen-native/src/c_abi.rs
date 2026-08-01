//! Target-specific lowering rules for C aggregates passed by value.

use cranelift_codegen::ir;
use reimer_hir::{ExternFunction, TypeDefinition, TypeRepresentation};
use reimer_layout::Layouts;
use reimer_types::Type;

/// How one source parameter is represented at the native call boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParameterPassing {
    /// Scalar values keep their ordinary Cranelift representation.
    Scalar,
    /// A small aggregate is loaded and passed as an integer value.
    DirectAggregate(ir::Type),
    /// The caller copies the aggregate into aligned storage and passes its address.
    IndirectAggregate {
        /// Minimum alignment required for the caller-owned copy.
        minimum_alignment: u32,
    },
}

/// How the source return value is represented at the native call boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReturnPassing {
    /// The function has no runtime return value.
    Unit,
    /// Scalar values keep their ordinary Cranelift representation.
    Scalar,
    /// A small aggregate is returned as an integer value.
    DirectAggregate(ir::Type),
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

/// Classifies a validated external function for the host C ABI.
pub(crate) fn classify_function(
    function: &ExternFunction,
    definitions: &[TypeDefinition],
    layouts: &Layouts,
    target: &str,
) -> Result<FunctionAbi, String> {
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| classify_parameter(parameter.ty, definitions, layouts, target))
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if function.return_type == Type::Unit {
        ReturnPassing::Unit
    } else if is_c_struct(function.return_type, definitions)? {
        match classify_aggregate(function.return_type, layouts, target)? {
            AggregatePassing::Direct(value_type) => ReturnPassing::DirectAggregate(value_type),
            AggregatePassing::Indirect { minimum_alignment } => {
                ReturnPassing::IndirectAggregate { minimum_alignment }
            }
        }
    } else {
        ReturnPassing::Scalar
    };
    Ok(FunctionAbi {
        parameters,
        return_value,
    })
}

fn classify_parameter(
    ty: Type,
    definitions: &[TypeDefinition],
    layouts: &Layouts,
    target: &str,
) -> Result<ParameterPassing, String> {
    if !is_c_struct(ty, definitions)? {
        return Ok(ParameterPassing::Scalar);
    }
    Ok(match classify_aggregate(ty, layouts, target)? {
        AggregatePassing::Direct(value_type) => ParameterPassing::DirectAggregate(value_type),
        AggregatePassing::Indirect { minimum_alignment } => {
            ParameterPassing::IndirectAggregate { minimum_alignment }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggregatePassing {
    Direct(ir::Type),
    Indirect { minimum_alignment: u32 },
}

fn classify_aggregate(
    ty: Type,
    layouts: &Layouts,
    target: &str,
) -> Result<AggregatePassing, String> {
    let layout = layouts.value_layout(ty)?;
    classify_host_aggregate(layout.size, target)
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the target-independent caller also handles unsupported target classifiers"
)]
fn classify_host_aggregate(size: u32, _target: &str) -> Result<AggregatePassing, String> {
    let passing = match size {
        1 => AggregatePassing::Direct(ir::types::I8),
        2 => AggregatePassing::Direct(ir::types::I16),
        4 => AggregatePassing::Direct(ir::types::I32),
        8 => AggregatePassing::Direct(ir::types::I64),
        _ => AggregatePassing::Indirect {
            minimum_alignment: 16,
        },
    };
    Ok(passing)
}

#[cfg(not(all(target_arch = "x86_64", target_os = "windows")))]
fn classify_host_aggregate(_size: u32, target: &str) -> Result<AggregatePassing, String> {
    Err(format!(
        "by-value `@repr(C)` aggregates are not implemented for target `{target}`"
    ))
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
