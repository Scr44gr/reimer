//! Canonical semantic types shared by Reimer's typed representations.

use std::fmt;

/// Stable index of a composite type definition in typed HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(pub u32);

/// A scalar or control-flow type known by the M2 type checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// Signed 64-bit integer.
    I64,
    /// Signed 128-bit integer.
    I128,
    /// Pointer-sized signed integer.
    Isize,
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer.
    U64,
    /// Unsigned 128-bit integer.
    U128,
    /// Pointer-sized unsigned integer.
    Usize,
    /// IEEE-754 32-bit floating-point number.
    F32,
    /// IEEE-754 64-bit floating-point number.
    F64,
    /// A logical value.
    Bool,
    /// A Unicode scalar value.
    Char,
    /// A named struct definition.
    Struct(TypeId),
    /// A named enum definition.
    Enum(TypeId),
    /// An interned tuple definition.
    Tuple(TypeId),
    /// An interned fixed-size array definition.
    Array(TypeId),
    /// An interned scoped reference definition.
    Reference(TypeId),
    /// An interned raw pointer definition.
    RawPointer(TypeId),
    /// An interned borrowed slice definition.
    Slice(TypeId),
    /// An interned function pointer signature.
    Function(TypeId),
    /// An immutable non-owning UTF-8 view.
    Str,
    /// A borrowed pointer to a NUL-terminated C byte string.
    CStr,
    /// The single unit value `()`.
    Unit,
    /// An expression that never reaches its continuation.
    Never,
}

impl Type {
    /// Returns whether `actual` can be used where `self` is expected.
    #[must_use]
    pub fn accepts(self, actual: Self) -> bool {
        self == actual || actual == Self::Never
    }

    /// Returns whether values of this type have a machine representation.
    #[must_use]
    pub const fn has_runtime_value(self) -> bool {
        !matches!(self, Self::Unit | Self::Never)
    }

    /// Returns whether this is any signed or unsigned integer type.
    #[must_use]
    pub const fn is_integer(self) -> bool {
        matches!(
            self,
            Self::I8
                | Self::I16
                | Self::I32
                | Self::I64
                | Self::I128
                | Self::Isize
                | Self::U8
                | Self::U16
                | Self::U32
                | Self::U64
                | Self::U128
                | Self::Usize
        )
    }

    /// Returns whether this is a signed integer type.
    #[must_use]
    pub const fn is_signed_integer(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::I128 | Self::Isize
        )
    }

    /// Returns whether this is an unsigned integer type.
    #[must_use]
    pub const fn is_unsigned_integer(self) -> bool {
        matches!(
            self,
            Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::U128 | Self::Usize
        )
    }

    /// Returns whether this is an IEEE-754 floating-point type.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    /// Returns whether arithmetic operators are available for this type.
    #[must_use]
    pub const fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Returns whether the type is represented by an aggregate definition.
    #[must_use]
    pub const fn is_composite(self) -> bool {
        matches!(
            self,
            Self::Struct(_)
                | Self::Enum(_)
                | Self::Tuple(_)
                | Self::Array(_)
                | Self::Slice(_)
                | Self::Str
        )
    }

    /// Returns whether this is a thin address value.
    #[must_use]
    pub const fn is_thin_pointer(self) -> bool {
        matches!(self, Self::Reference(_) | Self::RawPointer(_) | Self::CStr)
    }

    /// Returns the integer width for the current compilation host.
    #[must_use]
    pub const fn integer_bits(self) -> Option<u32> {
        match self {
            Self::I8 | Self::U8 => Some(8),
            Self::I16 | Self::U16 => Some(16),
            Self::I32 | Self::U32 => Some(32),
            Self::I64 | Self::U64 => Some(64),
            Self::I128 | Self::U128 => Some(128),
            Self::Isize | Self::Usize => Some(usize::BITS),
            _ => None,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::Isize => "isize",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::Usize => "usize",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::Str => "str",
            Self::CStr => "cstr",
            Self::Unit => "()",
            Self::Never => "never",
            Self::Struct(id) => return write!(formatter, "struct#{}", id.0),
            Self::Enum(id) => return write!(formatter, "enum#{}", id.0),
            Self::Tuple(id) => return write!(formatter, "tuple#{}", id.0),
            Self::Array(id) => return write!(formatter, "array#{}", id.0),
            Self::Reference(id) => return write!(formatter, "reference#{}", id.0),
            Self::RawPointer(id) => return write!(formatter, "pointer#{}", id.0),
            Self::Slice(id) => return write!(formatter, "slice#{}", id.0),
            Self::Function(id) => return write!(formatter, "function#{}", id.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Type;

    #[test]
    fn accepts_should_allow_never_where_a_value_is_expected() {
        assert!(Type::I32.accepts(Type::Never));
    }

    #[test]
    fn integer_bits_should_use_the_host_pointer_width_for_usize() {
        assert_eq!(Type::Usize.integer_bits(), Some(usize::BITS));
    }

    #[test]
    fn is_numeric_should_exclude_bool_and_char() {
        assert!(!Type::Char.is_numeric());
    }
}
