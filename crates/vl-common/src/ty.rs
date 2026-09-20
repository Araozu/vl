//! Compiler-owned value types.
//!
//! These are the VL language types enforced by `vl-typecheck`. They are
//! deliberately distinct from any VM representation: backends map these
//! to target concepts (e.g. VL `string` -> Naravm blob + const ref,
//! VL `File` -> Naravm `ObjectFile`). The VM may be weakly typed; VL is not.

use std::fmt;
use std::str::FromStr;

/// VL primitive + object + generic types. No `Error` here: poisoning lives in
/// `vl-typecheck::Ty::Error` so earlier stages stay quiet downstream.
///
/// `Array[T]` is a fixed-length heap array of `T` (a reference type backed by
/// the target's memory container). `Param(name)` is a use of an enclosing
/// generic function's type parameter (e.g. `T` in `function id[T](x: T): T`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VlType {
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
    File,
    Array(Box<VlType>),
    Param(String),
    Void,
}

impl fmt::Display for VlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VlType::U64 => write!(f, "u64"),
            VlType::I64 => write!(f, "i64"),
            VlType::F64 => write!(f, "f64"),
            VlType::Bool => write!(f, "bool"),
            VlType::U8 => write!(f, "u8"),
            VlType::String => write!(f, "string"),
            VlType::File => write!(f, "File"),
            VlType::Array(elem) => write!(f, "Array[{elem}]"),
            VlType::Param(name) => write!(f, "{name}"),
            VlType::Void => write!(f, "void"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTyError(pub String);

impl fmt::Display for ParseTyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown type `{}` (have: u64, i64, f64, bool, u8, string, File, Array[T], void)",
            self.0
        )
    }
}

impl FromStr for VlType {
    type Err = ParseTyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "u64" => Ok(VlType::U64),
            "i64" => Ok(VlType::I64),
            "f64" => Ok(VlType::F64),
            "bool" => Ok(VlType::Bool),
            "u8" => Ok(VlType::U8),
            // Accept both casings for the string object; canonical is `string`.
            "string" | "String" => Ok(VlType::String),
            // Object types are capitalized (`File`); accept lowercase too.
            "File" | "file" => Ok(VlType::File),
            "void" => Ok(VlType::Void),
            // `Array` needs an element type (`Array[T]`); `T` alone is a type
            // parameter, which only the parser can resolve against an
            // enclosing `function f[T]` scope.
            other => Err(ParseTyError(other.to_string())),
        }
    }
}

impl VlType {
    /// `void` is not a value: it cannot be a parameter, a `let` binding, a
    /// call argument, or an operand. It may only appear as a function return
    /// (value discarded) or as a bare expression statement.
    pub fn is_void(&self) -> bool {
        matches!(self, VlType::Void)
    }

    /// Element type for `Array[T]`; `None` for everything else.
    pub fn array_elem(&self) -> Option<&VlType> {
        match self {
            VlType::Array(elem) => Some(elem),
            _ => None,
        }
    }
}

/// Bound on a generic type parameter (`T extends Numeric`).
///
/// Bounds enable useful generic algorithms without full subtyping: an
/// unconstrained `T` is fully opaque (no operators), `Numeric` allows
/// arithmetic (`+ - * /`), ordering (`< <= > >=`), and equality, while
/// `Comparable` allows equality (`== !=`) over numbers, bools, and strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenericBound {
    Numeric,
    Comparable,
}

impl fmt::Display for GenericBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenericBound::Numeric => write!(f, "Numeric"),
            GenericBound::Comparable => write!(f, "Comparable"),
        }
    }
}

impl FromStr for GenericBound {
    type Err = ParseTyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Numeric" => Ok(GenericBound::Numeric),
            "Comparable" => Ok(GenericBound::Comparable),
            other => Err(ParseTyError(other.to_string())),
        }
    }
}
