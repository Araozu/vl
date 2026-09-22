//! Compiler-owned value types.
//!
//! These are the VL language types enforced by `vl-typecheck`. They are
//! deliberately distinct from any VM representation: backends map these
//! to target concepts (e.g. VL `String` -> Naravm blob + const ref,
//! VL `File` -> Naravm `ObjectFile`). The VM may be weakly typed; VL is not.

use std::fmt;
use std::str::FromStr;

/// VL primitive + object + generic types. No `Error` here: poisoning lives in
/// `vl-typecheck::Ty::Error` so earlier stages stay quiet downstream.
///
/// `Array[T]` is a fixed-length heap array of `T` (a reference type backed by
/// the target's memory container). `Param(name)` is a use of an enclosing
/// generic function's type parameter (e.g. `T` in `fun id[T](x: T): T`).
///
/// `Mutable(inner)` is a mutable view of a GC-managed reference (`*Foo`,
/// `*Array[T]`). It is a capability qualifier, not a machine pointer: passing
/// or assigning either spelling copies the GC reference.
/// One tuple element: `T` (unnamed, `name` is `None`) or `name: T`
/// (named, `name` is `Some`). A tuple is uniformly named or unnamed;
/// mixing is a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TupleField {
    pub name: Option<String>,
    pub ty: Box<VlType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VlType {
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
    File,
    /// A user-defined nominal object type. Objects have reference semantics.
    Object(String),
    /// A nominal union instantiation (`Option`, `Option[u64]`). Reference
    /// semantics (heap-allocated tag + payload); always constructed through a
    /// variant (`Option.Some(...)`) and read through `match`.
    Union {
        name: String,
        args: Vec<VlType>,
    },
    /// Nullable sugar (`?T`): surface-only spelling of the builtin
    /// `Option` union with one argument. Parsers produce this; typechecking
    /// desugars it to `Union { name: "Option", args: [T] }` so LIR and
    /// backends only ever see the union representation ("sugar all the way").
    Nullable(Box<VlType>),
    Array(Box<VlType>),
    /// Fixed-arity heterogeneous tuple (`#(u64, String)` / `#(x: u64)`).
    /// Value semantics (copy on bind/assign); heap container on the target.
    /// `*` over a tuple is a mutable view granting element-wise mutation.
    Tuple(Vec<TupleField>),
    Param(String),
    Void,
    /// Mutable view (`*T`) of a GC-managed reference type.
    Mutable(Box<VlType>),
}

impl fmt::Display for VlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VlType::U64 => write!(f, "u64"),
            VlType::I64 => write!(f, "i64"),
            VlType::F64 => write!(f, "f64"),
            VlType::Bool => write!(f, "bool"),
            VlType::U8 => write!(f, "u8"),
            VlType::String => write!(f, "String"),
            VlType::File => write!(f, "File"),
            VlType::Object(name) => write!(f, "{name}"),
            VlType::Union { name, args } => {
                write!(f, "{name}")?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            VlType::Nullable(inner) => write!(f, "?{inner}"),
            VlType::Array(elem) => write!(f, "Array[{elem}]"),
            VlType::Tuple(fields) => {
                write!(f, "#(")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match &field.name {
                        Some(name) => write!(f, "{name}: {}", field.ty)?,
                        None => write!(f, "{}", field.ty)?,
                    }
                }
                write!(f, ")")
            }
            VlType::Param(name) => write!(f, "{name}"),
            VlType::Void => write!(f, "void"),
            VlType::Mutable(inner) => write!(f, "*{inner}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTyError(pub String);

impl fmt::Display for ParseTyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown type `{}` (have: u64, i64, f64, bool, u8, String, File, Array[T], object types, void)",
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
            // `String` is a reference type, so it is uppercase like `File`,
            // `Array`, and object types. Only value-semantics primitives
            // (`u64`, `i64`, `f64`, `bool`, `u8`) are lowercase.
            "String" => Ok(VlType::String),
            // Object types are capitalized (`File`); no lowercase fallback:
            // reference types are uppercase, value types are lowercase.
            "File" => Ok(VlType::File),
            "void" => Ok(VlType::Void),
            // `Array` needs an element type (`Array[T]`); `T` alone is a type
            // parameter, which only the parser can resolve against an
            // enclosing `fun f[T]` scope.
            other => Err(ParseTyError(other.to_string())),
        }
    }
}

impl VlType {
    /// `void` is not a value: it cannot be a parameter, a `var`/`val` binding, a
    /// call argument, or an operand. It may only appear as a function return
    /// (value discarded) or as a bare expression statement.
    /// Recurses through `Mutable`/`Array`/`Tuple` so `*void` still counts as void.
    pub fn is_void(&self) -> bool {
        match self {
            VlType::Void => true,
            VlType::Mutable(inner) => inner.is_void(),
            VlType::Nullable(inner) => inner.is_void(),
            VlType::Array(elem) => elem.is_void(),
            VlType::Tuple(fields) => fields.iter().any(|f| f.ty.is_void()),
            VlType::Union { args, .. } => args.iter().any(|a| a.is_void()),
            _ => false,
        }
    }

    /// Element type for `Array[T]`; `None` for everything else.
    /// Looks through an outer `*` so `*Array[T]` still yields `T`.
    pub fn array_elem(&self) -> Option<&VlType> {
        match self {
            VlType::Array(elem) => Some(elem),
            VlType::Mutable(inner) => inner.array_elem(),
            _ => None,
        }
    }

    /// Tuple element types in order (erased names); `None` for non-tuples.
    /// Looks through an outer `*` so `*#(...)` still yields its elements.
    pub fn tuple_elems(&self) -> Option<Vec<VlType>> {
        match self {
            VlType::Tuple(fields) => Some(fields.iter().map(|f| (*f.ty).clone()).collect()),
            VlType::Mutable(inner) => inner.tuple_elems(),
            _ => None,
        }
    }

    /// GC-managed reference types: `String`, `File`, user objects,
    /// `Array[T]`, and tuples (heap containers; value semantics via copy).
    /// A mutable view counts as a reference when its inner type
    /// is a reference.
    pub fn is_reference_type(&self) -> bool {
        match self {
            VlType::String | VlType::File => true,
            VlType::Object(_) => true,
            VlType::Union { .. } => true,
            VlType::Nullable(_) => true,
            VlType::Array(_) => true,
            VlType::Tuple(_) => true,
            VlType::Mutable(inner) => inner.is_reference_type(),
            _ => false,
        }
    }

    /// Inner type of `?T`; `None` for everything else.
    /// Looks through an outer `*` so `*?T` still yields `T`.
    pub fn nullable_inner(&self) -> Option<&VlType> {
        match self {
            VlType::Nullable(inner) => Some(inner),
            VlType::Mutable(inner) => inner.nullable_inner(),
            _ => None,
        }
    }

    /// True for `*T` (one outer mutable capability).
    pub fn is_mutable_view(&self) -> bool {
        matches!(self, VlType::Mutable(_))
    }

    /// Remove one outer mutable capability (`*Foo` -> `Foo`).
    /// Non-mutable types clone unchanged.
    pub fn readonly_view(&self) -> VlType {
        match self {
            VlType::Mutable(inner) => (**inner).clone(),
            _ => self.clone(),
        }
    }

    /// Recursively erase capability qualifiers before LIR/codegen
    /// (`*Array[*Foo]` -> `Array[Foo]`). Runtime representation is identical.
    pub fn erase_capability(&self) -> VlType {
        match self {
            VlType::Mutable(inner) => inner.erase_capability(),
            VlType::Nullable(inner) => VlType::Nullable(Box::new(inner.erase_capability())),
            VlType::Array(elem) => VlType::Array(Box::new(elem.erase_capability())),
            VlType::Union { name, args } => VlType::Union {
                name: name.clone(),
                args: args.iter().map(|a| a.erase_capability()).collect(),
            },
            VlType::Tuple(fields) => VlType::Tuple(
                fields
                    .iter()
                    .map(|f| TupleField {
                        name: f.name.clone(),
                        ty: Box::new(f.ty.erase_capability()),
                    })
                    .collect(),
            ),
            _ => self.clone(),
        }
    }

    /// Alias for [`VlType::erase_capability`].
    pub fn runtime_type(&self) -> VlType {
        self.erase_capability()
    }

    /// Well-formedness of `*` placement. Returns `None` when valid, else a
    /// human-readable reason. Rejects mutable scalars/void, nested `**T`,
    /// and `*T` over an unconstrained type parameter. Recurses into
    /// `Array[T]` and `Mutable` payloads.
    pub fn mutable_wellformed_error(&self) -> Option<String> {
        match self {
            VlType::Mutable(inner) => {
                // Nested `**T` is never valid.
                if inner.is_mutable_view() {
                    return Some(format!(
                        "repeated capability qualifier `*{inner}` (only one `*` is allowed)"
                    ));
                }
                match &**inner {
                    VlType::Mutable(_) => Some(format!(
                        "repeated capability qualifier `*{inner}` (only one `*` is allowed)"
                    )),
                    VlType::Param(name) => Some(format!(
                        "`*{name}` needs a reference-kind bound (unconstrained `T` cannot grant mutation authority)"
                    )),
                    VlType::Void => Some("`*void` is not a valid type".to_string()),
                    VlType::U64
                    | VlType::I64
                    | VlType::F64
                    | VlType::Bool
                    | VlType::U8 => Some(format!("`*{inner}` is not a reference type")),
                    VlType::String | VlType::File | VlType::Object(_) | VlType::Union { .. } | VlType::Nullable(_) | VlType::Array(_) | VlType::Tuple(_) => {
                        // The payload itself may still be malformed
                        // (e.g. `*Array[*u64]`).
                        inner.mutable_wellformed_error()
                    }
                }
            }
            VlType::Array(elem) => elem.mutable_wellformed_error(),
            VlType::Nullable(inner) => inner.mutable_wellformed_error(),
            VlType::Tuple(fields) => fields.iter().find_map(|f| f.ty.mutable_wellformed_error()),
            VlType::Union { args, .. } => args.iter().find_map(|a| a.mutable_wellformed_error()),
            _ => None,
        }
    }
}

/// Bound on a generic type parameter (`T extends Numeric`).
///
/// Bounds enable useful generic algorithms without full subtyping: an
/// unconstrained `T` is fully opaque (no operators), `Numeric` allows
/// arithmetic (`+ - * /`), ordering (`< <= > >=`), and equality, while
/// `Comparable` allows equality (`== !=`) over numbers, bools, and Strings.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(name: &str) -> VlType {
        VlType::Object(name.into())
    }

    #[test]
    fn nullable_display_and_predicates() {
        let t = VlType::Nullable(Box::new(VlType::U64));
        assert_eq!(t.to_string(), "?u64");
        assert_eq!(
            VlType::Nullable(Box::new(VlType::Nullable(Box::new(VlType::U64)))).to_string(),
            "??u64"
        );
        // Nullables are heap reference types (builtin `Option` union).
        assert!(t.is_reference_type());
        assert!(!t.is_void());
        assert_eq!(t.nullable_inner(), Some(&VlType::U64));
        assert_eq!(VlType::U64.nullable_inner(), None);
        // `*?T` looks through the capability.
        assert_eq!(
            VlType::Mutable(Box::new(t.clone())).nullable_inner(),
            Some(&VlType::U64)
        );
        // Capability erasure and `*` well-formedness recurse.
        assert_eq!(t.erase_capability(), t);
        assert_eq!(
            VlType::Mutable(Box::new(t)).mutable_wellformed_error(),
            None
        );
        assert!(VlType::Nullable(Box::new(VlType::Void)).is_void());
    }

    #[test]
    fn display_round_trips_mutable_spelling() {
        assert_eq!(VlType::Mutable(Box::new(obj("Foo"))).to_string(), "*Foo");
        assert_eq!(
            VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::U64)))).to_string(),
            "*Array[u64]"
        );
        assert_eq!(
            VlType::Array(Box::new(VlType::Mutable(Box::new(obj("Foo"))))).to_string(),
            "Array[*Foo]"
        );
        assert_eq!(
            VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::Mutable(
                Box::new(obj("Foo"))
            )))))
            .to_string(),
            "*Array[*Foo]"
        );
    }

    #[test]
    fn reference_and_mutable_predicates() {
        assert!(VlType::String.is_reference_type());
        assert!(VlType::File.is_reference_type());
        assert!(obj("Foo").is_reference_type());
        assert!(VlType::Array(Box::new(VlType::U64)).is_reference_type());
        assert!(!VlType::U64.is_reference_type());
        assert!(!VlType::Bool.is_reference_type());
        assert!(!VlType::Void.is_reference_type());
        assert!(!VlType::Param("T".into()).is_reference_type());

        let m = VlType::Mutable(Box::new(obj("Foo")));
        assert!(m.is_mutable_view());
        assert!(m.is_reference_type());
        assert!(!obj("Foo").is_mutable_view());
        assert_eq!(m.readonly_view(), obj("Foo"));
        assert_eq!(obj("Foo").readonly_view(), obj("Foo"));
    }

    #[test]
    fn erase_and_runtime_type() {
        let t = VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::Mutable(
            Box::new(obj("Foo")),
        )))));
        assert_eq!(t.erase_capability(), VlType::Array(Box::new(obj("Foo"))));
        assert_eq!(t.runtime_type(), VlType::Array(Box::new(obj("Foo"))));
        assert_eq!(VlType::U64.erase_capability(), VlType::U64);
    }

    #[test]
    fn array_elem_looks_through_mutable() {
        let inner = VlType::U64;
        let arr = VlType::Array(Box::new(inner.clone()));
        let marr = VlType::Mutable(Box::new(arr.clone()));
        assert_eq!(arr.array_elem(), Some(&inner));
        assert_eq!(marr.array_elem(), Some(&inner));
        assert_eq!(VlType::U64.array_elem(), None);
    }

    #[test]
    fn mutable_wellformedness() {
        // Valid reference capabilities.
        assert_eq!(
            VlType::Mutable(Box::new(obj("Foo"))).mutable_wellformed_error(),
            None
        );
        assert_eq!(
            VlType::Mutable(Box::new(VlType::String)).mutable_wellformed_error(),
            None
        );
        assert_eq!(
            VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::U64))))
                .mutable_wellformed_error(),
            None
        );
        assert_eq!(
            VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::Param("T".into())))))
                .mutable_wellformed_error(),
            None
        );
        assert_eq!(
            VlType::Array(Box::new(VlType::Mutable(Box::new(obj("Foo")))))
                .mutable_wellformed_error(),
            None
        );
        // Invalid shapes.
        assert!(VlType::Mutable(Box::new(VlType::U64))
            .mutable_wellformed_error()
            .is_some());
        assert!(VlType::Mutable(Box::new(VlType::Bool))
            .mutable_wellformed_error()
            .is_some());
        assert!(VlType::Mutable(Box::new(VlType::Void))
            .mutable_wellformed_error()
            .is_some());
        assert!(
            VlType::Mutable(Box::new(VlType::Mutable(Box::new(obj("Foo")))))
                .mutable_wellformed_error()
                .is_some()
        );
        assert!(VlType::Mutable(Box::new(VlType::Param("T".into())))
            .mutable_wellformed_error()
            .is_some());
        assert!(
            VlType::Array(Box::new(VlType::Mutable(Box::new(VlType::U64))))
                .mutable_wellformed_error()
                .is_some()
        );
        assert!(
            VlType::Mutable(Box::new(VlType::Array(Box::new(VlType::Mutable(
                Box::new(VlType::U64)
            )))))
            .mutable_wellformed_error()
            .is_some()
        );
    }

    #[test]
    fn tuple_display_and_predicates() {
        let unnamed = VlType::Tuple(vec![
            TupleField {
                name: None,
                ty: Box::new(VlType::U64),
            },
            TupleField {
                name: None,
                ty: Box::new(VlType::String),
            },
        ]);
        assert_eq!(unnamed.to_string(), "#(u64, String)");
        let named = VlType::Tuple(vec![TupleField {
            name: Some("x".into()),
            ty: Box::new(VlType::U64),
        }]);
        assert_eq!(named.to_string(), "#(x: u64)");
        // Tuples are heap containers (reference lane) with value copy semantics.
        assert!(unnamed.is_reference_type());
        assert!(!unnamed.is_void());
        assert!(VlType::Tuple(vec![TupleField {
            name: None,
            ty: Box::new(VlType::Void)
        }])
        .is_void());
        assert_eq!(
            unnamed.tuple_elems(),
            Some(vec![VlType::U64, VlType::String])
        );
        assert_eq!(
            VlType::Mutable(Box::new(unnamed.clone())).tuple_elems(),
            Some(vec![VlType::U64, VlType::String])
        );
        assert_eq!(VlType::U64.tuple_elems(), None);
        // Capability erasure recurses; `*` over tuples is well-formed.
        assert_eq!(
            VlType::Mutable(Box::new(unnamed.clone())).erase_capability(),
            unnamed
        );
        assert_eq!(
            VlType::Mutable(Box::new(unnamed)).mutable_wellformed_error(),
            None
        );
    }

    #[test]
    fn void_is_recursive() {
        assert!(VlType::Void.is_void());
        assert!(VlType::Mutable(Box::new(VlType::Void)).is_void());
        assert!(!obj("Foo").is_void());
        assert!(!VlType::U64.is_void());
    }
}
