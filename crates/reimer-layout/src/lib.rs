//! Shared native layouts for typed HIR.

use reimer_hir::{EnumVariant, EnumVariantFields, TypeDefinition, TypeDefinitionKind};
use reimer_types::{Type, TypeId};

/// Size and alignment of a native value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueLayout {
    /// Storage size in bytes.
    pub size: u32,
    /// Required power-of-two alignment in bytes.
    pub align: u32,
}

/// Layout and addressable structure of an aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateLayout {
    /// Overall size and alignment.
    pub value: ValueLayout,
    /// Shape-specific addressing metadata.
    pub kind: AggregateLayoutKind,
}

/// Addressing metadata for an aggregate value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateLayoutKind {
    /// A pointer-shaped definition with no addressable fields.
    Scalar,
    /// Struct or tuple field offsets.
    Product {
        /// Byte offset of each field, in declaration order.
        offsets: Vec<u32>,
    },
    /// Fixed-size array metadata.
    Array {
        /// Byte distance between adjacent elements.
        stride: u32,
        /// Number of elements.
        length: u64,
    },
    /// Enum field offsets, indexed by variant and then field.
    Enum {
        /// Absolute byte offsets of each variant's fields.
        variants: Vec<Vec<u32>>,
    },
    /// Pointer-and-length view metadata.
    Slice {
        /// Byte offset of the data pointer.
        data_offset: u32,
        /// Byte offset of the element or byte length.
        length_offset: u32,
    },
}

#[derive(Debug, Clone)]
enum TypeMetadata {
    Product {
        fields: Vec<Type>,
    },
    Array {
        element: Type,
        length: u64,
        stride: u32,
    },
    Enum {
        variants: Vec<Vec<Type>>,
    },
    Pointer {
        target: Type,
    },
    Slice {
        element: Type,
        stride: u32,
    },
    Function {
        parameters: Vec<Type>,
        return_type: Type,
    },
}

/// Native layouts and structural metadata for every HIR type definition.
pub struct Layouts {
    aggregates: Vec<AggregateLayout>,
    str_layout: AggregateLayout,
    metadata: Vec<TypeMetadata>,
}

impl Layouts {
    /// Builds a consistent layout table for a compilation unit.
    ///
    /// # Errors
    ///
    /// Returns an error when a definition is missing, recursive by value, or
    /// exceeds the native backend's addressing limits.
    pub fn build(definitions: &[TypeDefinition]) -> Result<Self, String> {
        let mut cache = vec![None; definitions.len()];
        let mut visiting = vec![false; definitions.len()];
        for definition in definitions {
            build_aggregate_layout(definitions, &mut cache, &mut visiting, definition.id)?;
        }
        let aggregates: Vec<AggregateLayout> = cache
            .into_iter()
            .enumerate()
            .map(|(index, layout)| {
                layout.ok_or_else(|| format!("type definition {index} has no native layout"))
            })
            .collect::<Result<_, _>>()?;
        let metadata = definitions
            .iter()
            .map(|definition| match &definition.kind {
                TypeDefinitionKind::Struct { fields } => Ok(TypeMetadata::Product {
                    fields: fields.iter().map(|field| field.ty).collect(),
                }),
                TypeDefinitionKind::Tuple { elements } => Ok(TypeMetadata::Product {
                    fields: elements.clone(),
                }),
                TypeDefinitionKind::Array { element, length } => {
                    let element_layout = value_layout_from(definitions, &aggregates, *element)?;
                    Ok(TypeMetadata::Array {
                        element: *element,
                        length: *length,
                        stride: align_up(element_layout.size, element_layout.align)?,
                    })
                }
                TypeDefinitionKind::Enum { variants } => Ok(TypeMetadata::Enum {
                    variants: variants
                        .iter()
                        .map(|variant| match &variant.fields {
                            EnumVariantFields::Unit => Vec::new(),
                            EnumVariantFields::Tuple(fields) => fields.clone(),
                            EnumVariantFields::Struct(fields) => {
                                fields.iter().map(|field| field.ty).collect()
                            }
                        })
                        .collect(),
                }),
                TypeDefinitionKind::Reference { target, .. }
                | TypeDefinitionKind::RawPointer { target, .. } => {
                    Ok(TypeMetadata::Pointer { target: *target })
                }
                TypeDefinitionKind::Slice { element, .. } => {
                    let element_layout = value_layout_from(definitions, &aggregates, *element)?;
                    Ok(TypeMetadata::Slice {
                        element: *element,
                        stride: align_up(element_layout.size, element_layout.align)?,
                    })
                }
                TypeDefinitionKind::Function {
                    parameters,
                    return_type,
                } => Ok(TypeMetadata::Function {
                    parameters: parameters.clone(),
                    return_type: *return_type,
                }),
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            aggregates,
            str_layout: fat_view_layout()?,
            metadata,
        })
    }

    /// Gets the layout of a composite type.
    ///
    /// # Errors
    ///
    /// Returns an error when `ty` is not composite or its definition is absent.
    pub fn aggregate(&self, ty: Type) -> Result<&AggregateLayout, String> {
        if ty == Type::Str {
            return Ok(&self.str_layout);
        }
        let id =
            composite_type_id(ty).ok_or_else(|| format!("type `{ty}` is not a composite type"))?;
        self.aggregates
            .get(type_index(id)?)
            .ok_or_else(|| format!("composite type {} has no native layout", id.0))
    }

    /// Gets the pointee type of a reference or raw pointer.
    #[must_use]
    pub fn pointer_target(&self, ty: Type) -> Option<Type> {
        let id = composite_type_id(ty)?;
        match self.metadata.get(type_index(id).ok()?)? {
            TypeMetadata::Pointer { target } => Some(*target),
            _ => None,
        }
    }

    /// Gets the element type and stride of a slice.
    #[must_use]
    pub fn slice_shape(&self, ty: Type) -> Option<(Type, u32)> {
        let id = composite_type_id(ty)?;
        match self.metadata.get(type_index(id).ok()?)? {
            TypeMetadata::Slice { element, stride } => Some((*element, *stride)),
            _ => None,
        }
    }

    /// Gets the element stride of a slice.
    #[must_use]
    pub fn slice_stride(&self, ty: Type) -> Option<u32> {
        self.slice_shape(ty).map(|(_, stride)| stride)
    }

    /// Gets the parameter and return types of a function pointer.
    #[must_use]
    pub fn function_shape(&self, ty: Type) -> Option<(&[Type], Type)> {
        let id = composite_type_id(ty)?;
        match self.metadata.get(type_index(id).ok()?)? {
            TypeMetadata::Function {
                parameters,
                return_type,
            } => Some((parameters, *return_type)),
            _ => None,
        }
    }

    /// Gets the field types of a struct or tuple.
    #[must_use]
    pub fn product_fields(&self, ty: Type) -> Option<&[Type]> {
        let id = composite_type_id(ty)?;
        match self.metadata.get(type_index(id).ok()?)? {
            TypeMetadata::Product { fields } => Some(fields),
            _ => None,
        }
    }

    /// Gets the element type, length, and stride of a fixed-size array.
    #[must_use]
    pub fn array_shape(&self, ty: Type) -> Option<(Type, u64, u32)> {
        let id = composite_type_id(ty)?;
        match self.metadata.get(type_index(id).ok()?)? {
            TypeMetadata::Array {
                element,
                length,
                stride,
            } => Some((*element, *length, *stride)),
            _ => None,
        }
    }

    /// Gets the field types of each enum variant.
    #[must_use]
    pub fn enum_variants(&self, ty: Type) -> Option<&[Vec<Type>]> {
        let id = composite_type_id(ty)?;
        match self.metadata.get(type_index(id).ok()?)? {
            TypeMetadata::Enum { variants } => Some(variants),
            _ => None,
        }
    }

    /// Gets the native size and alignment of any value type.
    ///
    /// # Errors
    ///
    /// Returns an error when a composite type has no definition.
    pub fn value_layout(&self, ty: Type) -> Result<ValueLayout, String> {
        if ty.is_composite() {
            Ok(self.aggregate(ty)?.value)
        } else {
            scalar_layout(ty)
        }
    }
}

fn build_aggregate_layout(
    definitions: &[TypeDefinition],
    cache: &mut [Option<AggregateLayout>],
    visiting: &mut [bool],
    id: TypeId,
) -> Result<AggregateLayout, String> {
    let index = type_index(id)?;
    if let Some(layout) = cache.get(index).and_then(Clone::clone) {
        return Ok(layout);
    }
    let is_visiting = visiting
        .get_mut(index)
        .ok_or_else(|| format!("type definition {} is missing", id.0))?;
    if *is_visiting {
        return Err(format!("composite type {} contains itself by value", id.0));
    }
    *is_visiting = true;
    let definition = definitions
        .get(index)
        .ok_or_else(|| format!("type definition {} is missing", id.0))?;
    let mut layout = match &definition.kind {
        TypeDefinitionKind::Struct { fields } => {
            let types = fields.iter().map(|field| field.ty).collect::<Vec<_>>();
            let (value, offsets) = layout_product(definitions, cache, visiting, &types)?;
            AggregateLayout {
                value,
                kind: AggregateLayoutKind::Product { offsets },
            }
        }
        TypeDefinitionKind::Tuple { elements } => {
            let (value, offsets) = layout_product(definitions, cache, visiting, elements)?;
            AggregateLayout {
                value,
                kind: AggregateLayoutKind::Product { offsets },
            }
        }
        TypeDefinitionKind::Array { element, length } => {
            let element_layout = build_value_layout(definitions, cache, visiting, *element)?;
            let stride = align_up(element_layout.size, element_layout.align)?;
            let size = u64::from(stride)
                .checked_mul(*length)
                .and_then(|size| u32::try_from(size).ok())
                .ok_or_else(|| "array layout exceeds the native stack-slot limit".to_owned())?;
            AggregateLayout {
                value: ValueLayout {
                    size,
                    align: element_layout.align,
                },
                kind: AggregateLayoutKind::Array {
                    stride,
                    length: *length,
                },
            }
        }
        TypeDefinitionKind::Enum { variants } => {
            layout_enum(definitions, cache, visiting, variants)?
        }
        TypeDefinitionKind::Reference { .. }
        | TypeDefinitionKind::RawPointer { .. }
        | TypeDefinitionKind::Function { .. } => AggregateLayout {
            value: ValueLayout {
                size: usize::BITS / 8,
                align: usize::BITS / 8,
            },
            kind: AggregateLayoutKind::Scalar,
        },
        TypeDefinitionKind::Slice { .. } => fat_view_layout()?,
    };
    if let Some(requested) = definition.alignment {
        layout.value.align = layout.value.align.max(requested);
        layout.value.size = align_up(layout.value.size, layout.value.align)?;
    }
    if layout.value.size > 2_147_483_647_u32 {
        return Err(format!(
            "composite type {} exceeds the native addressing limit",
            id.0
        ));
    }
    visiting[index] = false;
    cache[index] = Some(layout.clone());
    Ok(layout)
}

fn fat_view_layout() -> Result<AggregateLayout, String> {
    let pointer_size = usize::BITS / 8;
    let length_offset = align_up(pointer_size, pointer_size)?;
    let size = length_offset
        .checked_add(pointer_size)
        .ok_or_else(|| "fat view layout size overflowed".to_owned())?;
    Ok(AggregateLayout {
        value: ValueLayout {
            size,
            align: pointer_size,
        },
        kind: AggregateLayoutKind::Slice {
            data_offset: 0,
            length_offset,
        },
    })
}

fn layout_enum(
    definitions: &[TypeDefinition],
    cache: &mut [Option<AggregateLayout>],
    visiting: &mut [bool],
    variants: &[EnumVariant],
) -> Result<AggregateLayout, String> {
    let mut payload_align = 1;
    let mut payload_size = 0;
    let mut relative_offsets = Vec::with_capacity(variants.len());
    for variant in variants {
        let types = match &variant.fields {
            EnumVariantFields::Unit => Vec::new(),
            EnumVariantFields::Tuple(types) => types.clone(),
            EnumVariantFields::Struct(fields) => fields.iter().map(|field| field.ty).collect(),
        };
        let (layout, offsets) = layout_product(definitions, cache, visiting, &types)?;
        payload_align = payload_align.max(layout.align);
        payload_size = payload_size.max(layout.size);
        relative_offsets.push(offsets);
    }
    let payload_offset = align_up(4, payload_align)?;
    let align = payload_align.max(4);
    let size = align_up(
        payload_offset
            .checked_add(payload_size)
            .ok_or_else(|| "enum layout size overflowed".to_owned())?,
        align,
    )?;
    let variants = relative_offsets
        .into_iter()
        .map(|offsets| {
            offsets
                .into_iter()
                .map(|offset| {
                    payload_offset
                        .checked_add(offset)
                        .ok_or_else(|| "enum field offset overflowed".to_owned())
                })
                .collect()
        })
        .collect::<Result<_, _>>()?;
    Ok(AggregateLayout {
        value: ValueLayout { size, align },
        kind: AggregateLayoutKind::Enum { variants },
    })
}

fn layout_product(
    definitions: &[TypeDefinition],
    cache: &mut [Option<AggregateLayout>],
    visiting: &mut [bool],
    fields: &[Type],
) -> Result<(ValueLayout, Vec<u32>), String> {
    let mut offsets = Vec::with_capacity(fields.len());
    let mut offset = 0;
    let mut align = 1;
    for field in fields {
        let field_layout = build_value_layout(definitions, cache, visiting, *field)?;
        offset = align_up(offset, field_layout.align)?;
        offsets.push(offset);
        offset = offset
            .checked_add(field_layout.size)
            .ok_or_else(|| "aggregate layout size overflowed".to_owned())?;
        align = align.max(field_layout.align);
    }
    let size = align_up(offset, align)?;
    Ok((ValueLayout { size, align }, offsets))
}

fn build_value_layout(
    definitions: &[TypeDefinition],
    cache: &mut [Option<AggregateLayout>],
    visiting: &mut [bool],
    ty: Type,
) -> Result<ValueLayout, String> {
    if let Some(id) = composite_type_id(ty) {
        Ok(build_aggregate_layout(definitions, cache, visiting, id)?.value)
    } else {
        scalar_layout(ty)
    }
}

fn value_layout_from(
    definitions: &[TypeDefinition],
    aggregates: &[AggregateLayout],
    ty: Type,
) -> Result<ValueLayout, String> {
    if let Some(id) = composite_type_id(ty) {
        aggregates
            .get(type_index(id)?)
            .map(|layout| layout.value)
            .ok_or_else(|| {
                format!(
                    "type definition {} is missing from {} definitions",
                    id.0,
                    definitions.len()
                )
            })
    } else {
        scalar_layout(ty)
    }
}

fn scalar_layout(ty: Type) -> Result<ValueLayout, String> {
    let (size, align) = match ty {
        Type::I8 | Type::U8 | Type::Bool => (1, 1),
        Type::I16 | Type::U16 => (2, 2),
        Type::I32 | Type::U32 | Type::F32 | Type::Char => (4, 4),
        Type::I64 | Type::U64 | Type::F64 => (8, 8),
        Type::I128 | Type::U128 => (16, 16),
        Type::Isize
        | Type::Usize
        | Type::Reference(_)
        | Type::RawPointer(_)
        | Type::Function(_)
        | Type::CStr => (usize::BITS / 8, usize::BITS / 8),
        Type::Str => (2 * (usize::BITS / 8), usize::BITS / 8),
        Type::Unit | Type::Never => (0, 1),
        Type::Struct(_) | Type::Enum(_) | Type::Tuple(_) | Type::Array(_) | Type::Slice(_) => {
            return Err(format!("composite type `{ty}` requires its definition"));
        }
    };
    Ok(ValueLayout { size, align })
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

fn type_index(id: TypeId) -> Result<usize, String> {
    usize::try_from(id.0).map_err(|_| format!("type id {} does not fit this host", id.0))
}

fn align_up(value: u32, align: u32) -> Result<u32, String> {
    let mask = align
        .checked_sub(1)
        .ok_or_else(|| "type alignment cannot be zero".to_owned())?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or_else(|| "native layout size overflowed".to_owned())
}

#[cfg(test)]
mod tests {
    use reimer_diagnostics::Span;
    use reimer_hir::{TypeDefinition, TypeDefinitionKind, TypeField, TypeRepresentation};
    use reimer_types::{Type, TypeId};

    use super::{AggregateLayoutKind, Layouts, ValueLayout};

    #[test]
    fn build_should_apply_explicit_alignment_to_a_struct() {
        let definitions = [TypeDefinition {
            id: TypeId(0),
            name: Some("Header".to_owned()),
            kind: TypeDefinitionKind::Struct {
                fields: vec![
                    TypeField {
                        name: "kind".to_owned(),
                        is_public: false,
                        ty: Type::U32,
                        span: Span::empty(0),
                    },
                    TypeField {
                        name: "length".to_owned(),
                        is_public: false,
                        ty: Type::U32,
                        span: Span::empty(0),
                    },
                ],
            },
            representation: TypeRepresentation::Native,
            alignment: Some(16),
            derives: Vec::new(),
            must_use: false,
            span: Span::empty(0),
        }];

        let layouts = Layouts::build(&definitions).expect("valid struct layout");
        let layout = layouts
            .aggregate(Type::Struct(TypeId(0)))
            .expect("struct aggregate layout");

        assert_eq!(
            layout.value,
            ValueLayout {
                size: 16,
                align: 16
            }
        );
        assert_eq!(
            layout.kind,
            AggregateLayoutKind::Product {
                offsets: vec![0, 4]
            }
        );
    }

    #[test]
    fn build_should_reject_a_type_that_contains_itself_by_value() {
        let definitions = [TypeDefinition {
            id: TypeId(0),
            name: Some("Recursive".to_owned()),
            kind: TypeDefinitionKind::Struct {
                fields: vec![TypeField {
                    name: "next".to_owned(),
                    is_public: false,
                    ty: Type::Struct(TypeId(0)),
                    span: Span::empty(0),
                }],
            },
            representation: TypeRepresentation::Native,
            alignment: None,
            derives: Vec::new(),
            must_use: false,
            span: Span::empty(0),
        }];

        let Err(error) = Layouts::build(&definitions) else {
            panic!("recursive storage must fail");
        };

        assert!(error.contains("contains itself by value"));
    }
}
