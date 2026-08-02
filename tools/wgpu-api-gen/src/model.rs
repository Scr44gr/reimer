//! In-memory representation of the wgpu-native C API.

/// Declarations collected from both standard and native headers.
#[derive(Debug, Default)]
pub(crate) struct Api {
    pub(crate) handles: Vec<Handle>,
    pub(crate) aliases: Vec<Alias>,
    pub(crate) enumerations: Vec<Enumeration>,
    pub(crate) constants: Vec<Constant>,
    pub(crate) callbacks: Vec<Callback>,
    pub(crate) structures: Vec<Structure>,
    pub(crate) functions: Vec<Function>,
}

#[derive(Debug)]
pub(crate) struct Handle {
    pub(crate) name: String,
    pub(crate) documentation: String,
}

#[derive(Debug)]
pub(crate) struct Alias {
    pub(crate) name: String,
    pub(crate) target: CType,
    pub(crate) documentation: String,
}

#[derive(Debug)]
pub(crate) struct Enumeration {
    pub(crate) name: String,
    pub(crate) documentation: String,
    pub(crate) entries: Vec<EnumEntry>,
}

#[derive(Debug)]
pub(crate) struct EnumEntry {
    pub(crate) name: String,
    pub(crate) value: u64,
    pub(crate) documentation: String,
}

#[derive(Debug)]
pub(crate) struct Constant {
    pub(crate) name: String,
    pub(crate) ty: CType,
    pub(crate) value: ConstantValue,
    pub(crate) documentation: String,
}

#[derive(Debug)]
pub(crate) enum ConstantValue {
    Integer(u64),
    FloatNaN,
}

#[derive(Debug)]
pub(crate) struct Callback {
    pub(crate) name: String,
    pub(crate) documentation: String,
}

#[derive(Debug)]
pub(crate) struct Structure {
    pub(crate) name: String,
    pub(crate) documentation: String,
    pub(crate) fields: Vec<Field>,
}

#[derive(Debug)]
pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) c_name: String,
    pub(crate) ty: CType,
    pub(crate) documentation: String,
}

#[derive(Debug)]
pub(crate) struct Function {
    pub(crate) name: String,
    pub(crate) return_type: CType,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) documentation: String,
    pub(crate) native_extension: bool,
}

#[derive(Debug)]
pub(crate) struct Parameter {
    pub(crate) name: String,
    pub(crate) ty: CType,
}

#[derive(Debug, Clone)]
pub(crate) struct CType {
    pub(crate) base: String,
    pub(crate) pointers: Vec<PointerKind>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PointerKind {
    Const,
    Mut,
}
