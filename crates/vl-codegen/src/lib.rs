//! vl-codegen: backends. LIR -> target output.
//!
//! This crate exposes a stable [`Target`] trait and the Naravm backend:
//!
//! - [`NaraVmTarget`]: executable Naravm 0.2 vmfiles: `fun main()`,
//!   when present, gets an ordinary callable body plus a single `<entrypoint>`
//!   wrapper. Integer/float arithmetic, comparisons, and control flow
//!   plus `std.print` / `std.println` / `std.print_u64`, the `std.string`
//!   natives (`len`, `concat`, `eq`, `to_u64`, `hex_to_u64`), `std.math.mod_u64`,
//!   `std.fmt.u64_to_s`, and the checked fallible natives (`std.string`
//!   `byte_at`/`slice` as `StringError!T`, `std.fs.read_file` as
//!   `FsError!String`, each `std.net.tcp` native as `TcpError!T`: every
//!   `rv10` status becomes a typed error value), and user-function calls
//!   lower to `calli`.
//!
//! Rule: new targets = new types implementing [`Target`]. Never branch
//! the LIR or the driver on target names.

use vl_common::Scalar;
use vl_common::{Diagnostic, Span};
use vl_lir::{Instr, LirOp, LirProgram};

/// Compiled artifact: text plus the target that produced it.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub target: String,
    pub text: String,
    /// Binary payloads are used by targets whose output is not text.
    pub bytes: Option<Vec<u8>>,
}
/// Every backend implements this. Keep it object-safe (`&self`, no generics).
pub trait Target {
    fn name(&self) -> &'static str;
    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>);
}

/// Qualified identity of the TCP error set (`<module>.<name>`, matching the
/// resolver's canonicalization for `use std.net.tcp.{TcpError}`). Extern
/// signatures spell the set qualified so `try` compatibility compares the
/// same string the importer-side `TcpError!T` normalizes to.
pub const TCP_ERROR_SET: &str = "std.net.tcp.TcpError";

/// `std::net::tcp` status codes 1-7 in order, each with its `TcpError`
/// variant. Status 0 is success (never wrapped); anything else the VM may
/// report in the future surfaces as `IoError`.
pub const TCP_STATUS_VARIANTS: [(u64, &str); 7] = [
    (1, "InvalidArgument"),
    (2, "IoError"),
    (3, "InvalidHandle"),
    (4, "WrongHandleKind"),
    (5, "TooManyHandles"),
    (6, "OutOfMemory"),
    (7, "CapabilityUnavailable"),
];

/// Qualified identity of the string error set (`<module>.<name>`), matching
/// the resolver's canonicalization for `use std.string.{StringError}`.
/// Extern signatures spell the set qualified so `try` compatibility compares
/// the same string the importer-side `StringError!T` normalizes to.
pub const STRING_ERROR_SET: &str = "std.string.StringError";

/// `std::string` status codes with their `StringError` variants. Status 0
/// is success (never wrapped); anything else the VM may report in the
/// future surfaces as `OutOfBounds`.
pub const STRING_STATUS_VARIANTS: [(u64, &str); 3] =
    [(1, "OutOfBounds"), (2, "OutOfBounds"), (3, "InvalidRange")];

/// Qualified identity of the filesystem error set (`<module>.<name>`).
/// The VM reports a single failure code today, so every nonzero status
/// maps to `IoError` (finer codes are a VM follow-up).
pub const FS_ERROR_SET: &str = "std.fs.FsError";

/// `std::fs` status codes with their `FsError` variants. Status 0 is
/// success (never wrapped).
pub const FS_STATUS_VARIANTS: [(u64, &str); 1] = [(1, "IoError")];

/// Wrap an `ok` payload type in the named `TcpError` fallible shared by the
/// `std.net.tcp` externs.
fn tcp_fallible(ok: vl_common::VlType) -> vl_common::VlType {
    vl_common::VlType::Fallible {
        err: Some(TCP_ERROR_SET.into()),
        ok: Box::new(ok),
    }
}

/// Wrap an `ok` payload type in the named `StringError` fallible shared by
/// the checked `std.string` externs.
fn string_fallible(ok: vl_common::VlType) -> vl_common::VlType {
    vl_common::VlType::Fallible {
        err: Some(STRING_ERROR_SET.into()),
        ok: Box::new(ok),
    }
}

/// Wrap an `ok` payload type in the named `FsError` fallible shared by the
/// checked `std.fs` externs.
fn fs_fallible(ok: vl_common::VlType) -> vl_common::VlType {
    vl_common::VlType::Fallible {
        err: Some(FS_ERROR_SET.into()),
        ok: Box::new(ok),
    }
}

/// Modules known to the target environment. Frontend resolution consumes the
/// same catalog, so imports and emitted calls cannot drift apart.
///
/// This is where the language's extern type surface is *declared*: every
/// export carries VL-level param names/types and a return type. Backends map
/// these VL types to target concepts (e.g. VL `String` -> Naravm blob).
///
/// The `std.math` / `std.fmt` entries below cover only the
/// infallible-or-trapping VM natives (e.g. `mod_u64`, `to_u64`).
/// `std.string` mixes both: `len`/`concat`/`eq` stay infallible while
/// `byte_at`/`slice` are checked (`rv10` statuses become `StringError!T`,
/// like TCP). `std.net.tcp` is fully fallible, and `std.fs.read_file` is
/// checked too (`rv10` becomes `FsError!String`). Natives that report
/// errors through `rv10` without a typed wrapper yet (`to_u64`,
/// `hex_to_u64` trap instead of reporting) and the container bridges
/// (`bytes`, `from_container`) stay out until they get the same treatment;
/// likewise `std.fs` file handles (`get_stdout`, `write`) stay out until
/// `File` values have a checked story.
pub fn modules() -> Vec<vl_common::ModuleSpec> {
    use vl_common::VlType as T;
    let mut catalog = vec![
        vl_common::ModuleSpec::new(
            &["std"],
            &[
                ("print", &[("value", T::String)], T::Void),
                ("println", &[("value", T::String)], T::Void),
                ("print_u64", &[("value", T::U64)], T::Void),
            ],
        ),
        vl_common::ModuleSpec::new(
            &["std", "fs"],
            &[("read_file", &[("path", T::String)], fs_fallible(T::String))],
        ),
        vl_common::ModuleSpec::new(
            &["std", "string"],
            &[
                ("len", &[("value", T::String)], T::U64),
                ("concat", &[("a", T::String), ("b", T::String)], T::String),
                ("eq", &[("a", T::String), ("b", T::String)], T::Bool),
                ("to_u64", &[("value", T::String)], T::U64),
                ("hex_to_u64", &[("value", T::String)], T::U64),
                (
                    "byte_at",
                    &[("value", T::String), ("index", T::U64)],
                    string_fallible(T::U8),
                ),
                (
                    "slice",
                    &[("value", T::String), ("start", T::U64), ("end", T::U64)],
                    string_fallible(T::String),
                ),
            ],
        ),
        vl_common::ModuleSpec::new(
            &["std", "math"],
            &[("mod_u64", &[("a", T::U64), ("b", T::U64)], T::U64)],
        ),
        vl_common::ModuleSpec::new(
            &["std", "fmt"],
            &[("u64_to_s", &[("value", T::U64)], T::String)],
        ),
        vl_common::ModuleSpec::new(
            &["std", "net", "tcp"],
            &[
                (
                    "connect",
                    &[("host", T::String), ("port", T::U64)],
                    tcp_fallible(T::U64),
                ),
                (
                    "listen",
                    &[
                        ("address", T::String),
                        ("port", T::U64),
                        ("backlog", T::U64),
                    ],
                    tcp_fallible(T::Tuple(vec![
                        vl_common::TupleField {
                            name: None,
                            ty: Box::new(T::U64),
                        },
                        vl_common::TupleField {
                            name: None,
                            ty: Box::new(T::U64),
                        },
                    ])),
                ),
                ("accept", &[("listener", T::U64)], tcp_fallible(T::U64)),
                (
                    "read",
                    &[("socket", T::U64), ("max_bytes", T::U64)],
                    tcp_fallible(T::Tuple(vec![
                        vl_common::TupleField {
                            name: None,
                            ty: Box::new(T::String),
                        },
                        vl_common::TupleField {
                            name: None,
                            ty: Box::new(T::Bool),
                        },
                    ])),
                ),
                (
                    "write",
                    &[("socket", T::U64), ("data", T::String)],
                    tcp_fallible(T::U64),
                ),
                ("close", &[("handle", T::U64)], tcp_fallible(T::Void)),
            ],
        ),
    ];
    // The `TcpError` variants mirror the `std::net::tcp` status codes 1-7
    // one-to-one (see `TCP_STATUS_VARIANTS`); the backend maps `rv10` to the
    // matching variant's global error code.
    catalog
        .iter_mut()
        .find(|m| m.path.as_string() == "std.net.tcp")
        .expect("std.net.tcp declared above")
        .errors
        .push(vl_common::ErrorExport {
            name: "TcpError".into(),
            qualified: TCP_ERROR_SET.into(),
            variants: TCP_STATUS_VARIANTS
                .iter()
                .map(|(_, variant)| vl_common::ErrorVariantSig {
                    name: (*variant).into(),
                    payload: Vec::new(),
                })
                .collect(),
        });
    // `StringError` mirrors the `std::string` fallible natives (`byte_at`,
    // `slice`); several statuses share `OutOfBounds`, so the table is not
    // one-to-one (see `STRING_STATUS_VARIANTS`).
    catalog
        .iter_mut()
        .find(|m| m.path.as_string() == "std.string")
        .expect("std.string declared above")
        .errors
        .push(vl_common::ErrorExport {
            name: "StringError".into(),
            qualified: STRING_ERROR_SET.into(),
            variants: ["OutOfBounds", "InvalidRange"]
                .iter()
                .map(|variant| vl_common::ErrorVariantSig {
                    name: (*variant).into(),
                    payload: Vec::new(),
                })
                .collect(),
        });
    // `FsError` mirrors `std::fs.read_file` (one failure code today).
    catalog
        .iter_mut()
        .find(|m| m.path.as_string() == "std.fs")
        .expect("std.fs declared above")
        .errors
        .push(vl_common::ErrorExport {
            name: "FsError".into(),
            qualified: FS_ERROR_SET.into(),
            variants: ["IoError"]
                .iter()
                .map(|variant| vl_common::ErrorVariantSig {
                    name: (*variant).into(),
                    payload: Vec::new(),
                })
                .collect(),
        });
    catalog
}

/// Module surface available to a concrete backend. The broad `modules`
/// catalog remains useful to frontend/library tests; drivers should resolve
/// against this target-specific view so accepted calls are actually emit-able.
///
/// Every catalog module is emittable on Naravm: `std.fs.read_file` and the
/// checked `std.string` natives lower through the same `rv10`-status
/// machinery as `std.net.tcp`. Freestanding WASM hosts do not register
/// `std::net::tcp` (nor `std::fs`), so linked TCP/file programs need a
/// native host.
pub fn modules_for_target(target: &str) -> Vec<vl_common::ModuleSpec> {
    match target {
        "naravm" => modules(),
        _ => modules(),
    }
}

/// All backends the driver knows about.
pub fn all_targets() -> Vec<&'static str> {
    vec![NaraVmTarget.name()]
}

/// Look up a backend by `--target` flag value.
pub fn lookup(name: &str) -> Option<Box<dyn Target>> {
    match name {
        "naravm" => Some(Box::new(NaraVmTarget)),
        _ => None,
    }
}

// --------------------------------------------------------- Naravm ---

/// Naravm 0.2 executable vmfile backend: compiles `fun main()`, when
/// present, to an ordinary callable function plus a single `<entrypoint>`
/// wrapper and one Nara function per other user function (see the internals
/// book for the supported subset). A module
/// without `main` still compiles (a library); entrypoint presence is the
/// VM/loader's check, not the compiler's. Calls to
/// `std.print` / `std.println` / `std.print_u64`, the `std.string` natives,
/// `std.math.mod_u64`, `std.fmt.u64_to_s`, the checked `std.net.tcp` natives
/// (each `rv10` status becomes a `TcpError!T` value), and to user functions
/// lower to `calli`;
/// `Array[T]` values lower to memory containers (`create`/`getvat`/`setvat`
/// for value elements, `getrfat`/`setrfat` for reference elements);
/// modules without `main` retain their ordered global initializer as the
/// ordinary `<module-init>` function; anything else is a diagnostic.
pub struct NaraVmTarget;

impl Target for NaraVmTarget {
    fn name(&self) -> &'static str {
        "naravm"
    }

    fn emit(&self, prog: &LirProgram) -> (Option<Artifact>, Vec<Diagnostic>) {
        // No entrypoint requirement: snippets and libraries compile without
        // `main`. Whether a runnable module defines a usable entrypoint is
        // validated by a higher stage (the VM/loader), not the compiler.
        let mut diags = Vec::new();
        let bytes = match nara_vmfile(prog, &mut diags) {
            Some(bytes) if diags.iter().all(|d| !d.is_error()) => bytes,
            _ => return (None, diags),
        };
        (
            Some(Artifact {
                target: self.name().into(),
                text: String::new(),
                bytes: Some(bytes),
            }),
            diags,
        )
    }
}

#[derive(Clone)]
enum NaraConstant {
    Value { value_idx: usize },
    String { offset: usize, len: usize },
    Function { module: usize, function: usize },
}

/// Map a VL-level extern identity to the Naravm native it lowers to.
/// VL keeps dotted names (`std.string.concat`); the VM registers natives
/// under `::` modules (`std::string::concat`). `None` means the callee is
/// not a known VM native (user function or cross-module source call), in
/// which case the LIR spelling is interned verbatim.
fn nara_extern_target(module: &str, function: &str) -> Option<(&'static str, &'static str)> {
    match (module, function) {
        ("std.string", "len") => Some(("std::string", "byte_count")),
        ("std.string", "concat") => Some(("std::string", "concat")),
        ("std.string", "eq") => Some(("std::string", "eq")),
        ("std.string", "to_u64") => Some(("std::string", "to_u64")),
        ("std.string", "hex_to_u64") => Some(("std::string", "hex_to_u64")),
        ("std.string", "byte_at") => Some(("std::string", "byte_at")),
        ("std.string", "slice") => Some(("std::string", "slice")),
        ("std.fs", "read_file") => Some(("std::fs", "read_file")),
        ("std.math", "mod_u64") => Some(("std::math", "mod_u64")),
        ("std.fmt", "u64_to_s") => Some(("std::fmt", "u64_to_s")),
        ("std.net.tcp", "connect") => Some(("std::net::tcp", "connect")),
        ("std.net.tcp", "listen") => Some(("std::net::tcp", "listen")),
        ("std.net.tcp", "accept") => Some(("std::net::tcp", "accept")),
        ("std.net.tcp", "read") => Some(("std::net::tcp", "read")),
        ("std.net.tcp", "write") => Some(("std::net::tcp", "write")),
        ("std.net.tcp", "close") => Some(("std::net::tcp", "close")),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NaraKind {
    U64,
    I64,
    F64,
    Bool,
    U8,
    String,
    File,
    Object(String),
    /// Fixed-length heap array (a memory container). The payload is the
    /// element kind: value elements use `vat` ops, reference elements
    /// (`String`, `File`, nested arrays) use `rfat` ops.
    Array(Box<NaraKind>),
    /// Fixed-arity heterogeneous tuple (a memory container). Each position
    /// maps to a value or reference slot by its own element kind; the
    /// vector holds the erased element kinds in source order.
    Tuple(Vec<NaraKind>),
    /// Fallible value (`E!T`, a memory container): value slot 0 holds the
    /// tag (0 = ok, 1 = error), value slot 1 holds the error code on the
    /// err path or the first value-kind payload slot on the ok path, and
    /// the remaining slots hold the `ok` payload's own value/reference
    /// slots in order. Error payloads share the tail past the code (value
    /// slots from 2, reference slots from 0) in a region spanning the
    /// program's maximum lanes. The payload is `Void` for `E!void` (tag-only).
    Fallible(Box<NaraKind>),
    /// Union value (a memory container): value slot 0 holds the `u64`
    /// discriminant tag, remaining value slots hold value-kind payloads in
    /// order, and reference slots hold reference-kind payloads in order.
    /// The payload layout is per construction site (see `NewVariant`).
    Union(String),
}

impl NaraKind {
    /// Map a VL-level type to its register file. `None` for `Void`/`Error`/
    /// `Param`, which never reach codegen through the driver (frontends
    /// reject them; instances are always concrete).
    fn of_ty(ty: &vl_typecheck::Ty) -> Option<Self> {
        match ty {
            // Untyped integer literals use the target's unsigned value lane
            // until a signed/byte context has selected a concrete type.
            vl_typecheck::Ty::Int => Some(NaraKind::U64),
            vl_typecheck::Ty::U64 => Some(NaraKind::U64),
            vl_typecheck::Ty::I64 => Some(NaraKind::I64),
            vl_typecheck::Ty::F64 => Some(NaraKind::F64),
            vl_typecheck::Ty::Bool => Some(NaraKind::Bool),
            vl_typecheck::Ty::U8 => Some(NaraKind::U8),
            vl_typecheck::Ty::String => Some(NaraKind::String),
            vl_typecheck::Ty::File => Some(NaraKind::File),
            vl_typecheck::Ty::Object(o) => {
                if o.args.is_empty() {
                    Some(NaraKind::Object(o.name.clone()))
                } else {
                    // Generic instantiations use mangled layouts (`List$u64`),
                    // matching LIR `NewObject` (see `vl-lir::lower`).
                    Some(NaraKind::Object(vl_typecheck::mangle(&o.name, &o.args)))
                }
            }
            // Union values are heap tag+payload containers (reference lane).
            // Type arguments are erased here: each construction site carries
            // its concrete payload kinds on the instruction.
            vl_typecheck::Ty::Union(u) => Some(NaraKind::Union(u.name.clone())),
            vl_typecheck::Ty::Array(elem) => Some(NaraKind::Array(Box::new(Self::of_ty(elem)?))),
            vl_typecheck::Ty::Tuple(fields) => {
                let mut kinds = Vec::with_capacity(fields.len());
                for (_, ty) in fields {
                    kinds.push(Self::of_ty(ty)?);
                }
                Some(NaraKind::Tuple(kinds))
            }
            // Error sets are global `u64` codes (value lane); set
            // membership is a static constraint with no runtime trace.
            vl_typecheck::Ty::ErrorSet(_) => Some(NaraKind::U64),
            vl_typecheck::Ty::Fallible(f) => {
                Some(NaraKind::Fallible(Box::new(Self::of_ok(&f.ok)?)))
            }
            // Capability-only: same representation as the read-only view.
            vl_typecheck::Ty::Mutable(inner) => Self::of_ty(inner),
            vl_typecheck::Ty::Param(_) | vl_typecheck::Ty::Void | vl_typecheck::Ty::Error => None,
        }
    }

    /// Reference kinds live in `rf`, everything else in `rv`.
    /// Tuples, unions, and fallibles are heap containers (hence `rf`) with
    /// value copy semantics at the language level for tuples (deep-copied
    /// on `Copy`/param entry); unions and fallibles copy as shared
    /// references like objects.
    fn is_ref(&self) -> bool {
        matches!(
            self,
            NaraKind::String
                | NaraKind::File
                | NaraKind::Object(_)
                | NaraKind::Array(_)
                | NaraKind::Tuple(_)
                | NaraKind::Union(_)
                | NaraKind::Fallible(_)
        )
    }

    /// Erased lane kind of a fallible `ok` payload. `Void` (`E!void`)
    /// maps to a dummy value lane: the container keeps its code slot on
    /// every path, and the ok path simply never reads it.
    fn of_ok(ok: &vl_typecheck::Ty) -> Option<Self> {
        match ok {
            vl_typecheck::Ty::Void => Some(NaraKind::U64),
            _ => Self::of_ty(ok),
        }
    }

    fn of_scalar(value: Scalar) -> Self {
        match value {
            Scalar::Int(_) => NaraKind::U64,
            Scalar::U64(_) => NaraKind::U64,
            Scalar::I64(_) => NaraKind::I64,
            Scalar::F64(_) => NaraKind::F64,
            Scalar::Bool(_) => NaraKind::Bool,
            Scalar::U8(_) => NaraKind::U8,
        }
    }

    fn scalar_bits(value: Scalar) -> u64 {
        match value {
            Scalar::Int(v) => v as u64,
            Scalar::U64(v) => v,
            Scalar::I64(v) => v as u64,
            Scalar::F64(v) => v,
            Scalar::Bool(v) => u64::from(v),
            Scalar::U8(v) => u64::from(v),
        }
    }

    fn is_integer(&self) -> bool {
        matches!(self, NaraKind::U64 | NaraKind::I64 | NaraKind::U8)
    }
}

/// Reserved callee-saved reference register for module state (global storage
/// container). Removed from the ordinary allocator, initialized at the
/// entrypoint before any initializer runs, and preserved across calls
/// (callee-saved) including recursion and extern calls.
const MODULE_STATE_RF: u8 = 0x3F;

/// Return whether a field uses the reference lane, its lane-local slot, and
/// its VL type. Naravm containers keep value and reference fields in separate
/// arrays, so a declaration's source index is not the runtime index.
fn object_slot<'a>(
    def: &'a vl_lir::ObjectDef,
    name: &str,
) -> Option<(bool, usize, &'a vl_typecheck::Ty)> {
    let mut value_slot = 0;
    let mut ref_slot = 0;
    for (field, ty) in &def.fields {
        let kind = NaraKind::of_ty(ty)?;
        let (is_ref, slot) = if kind.is_ref() {
            let slot = ref_slot;
            ref_slot += 1;
            (true, slot)
        } else {
            let slot = value_slot;
            value_slot += 1;
            (false, slot)
        };
        if field == name {
            return Some((is_ref, slot, ty));
        }
    }
    None
}

/// Tuple position -> (uses-reference-lane, lane-local slot).
/// Naravm containers keep value and reference fields in separate arrays,
/// so the source index is not the runtime index: count preceding elements
/// of the same lane.
fn tuple_slot(kinds: &[NaraKind], index: usize) -> Option<(bool, usize)> {
    let kind = kinds.get(index)?;
    let want_ref = kind.is_ref();
    let mut slot = 0;
    for (i, k) in kinds.iter().enumerate() {
        if i == index {
            return Some((want_ref, slot));
        }
        if k.is_ref() == want_ref {
            slot += 1;
        }
    }
    None
}

/// Lane counts for a tuple layout (for `createi`).
fn tuple_lanes(kinds: &[NaraKind]) -> (usize, usize) {
    let mut values = 0;
    let mut refs = 0;
    for k in kinds {
        if k.is_ref() {
            refs += 1;
        } else {
            values += 1;
        }
    }
    (values, refs)
}

/// Erased element kinds for a tuple instruction's `tys` list.
fn nara_tuple_kinds(
    tys: &[vl_typecheck::Ty],
    span: Span,
    e: &mut NaraEmit,
) -> Option<Vec<NaraKind>> {
    let mut kinds = Vec::with_capacity(tys.len());
    for ty in tys {
        match NaraKind::of_ty(ty) {
            Some(k) => kinds.push(k),
            None => {
                e.diags.push(
                    Diagnostic::error("Naravm backend found a non-runtime tuple element type")
                        .with_label(span, "tuple emitted here")
                        .with_code("E500"),
                );
                return None;
            }
        }
    }
    Some(kinds)
}

/// Erased payload kinds for a variant instruction's `tys` list.
fn nara_variant_kinds(
    tys: &[vl_typecheck::Ty],
    span: Span,
    e: &mut NaraEmit,
) -> Option<Vec<NaraKind>> {
    let mut kinds = Vec::with_capacity(tys.len());
    for ty in tys {
        match NaraKind::of_ty(ty) {
            Some(k) => kinds.push(k),
            None => {
                e.diags.push(
                    Diagnostic::error("Naravm backend found a non-runtime variant payload type")
                        .with_label(span, "variant emitted here")
                        .with_code("E500"),
                );
                return None;
            }
        }
    }
    Some(kinds)
}

/// Payload position -> (uses-reference-lane, lane-local slot) for a variant
/// container. Value slot 0 holds the discriminant tag, so value-kind
/// payloads start at slot 1; reference-kind payloads start at slot 0.
fn variant_payload_slot(kinds: &[NaraKind], index: usize) -> Option<(bool, usize)> {
    let (is_ref, lane_slot) = tuple_slot(kinds, index)?;
    Some(if is_ref {
        (true, lane_slot)
    } else {
        (false, lane_slot + 1)
    })
}

/// Deep-copy a tuple container (`src_rf`) into `dst_rf` (already allocated).
/// Value semantics: every element is copied slot-to-slot so the destination
/// owns an independent container. Plain reference elements (`String`,
/// arrays, objects) share their referents (like object fields); nested
/// tuples recurse so no tuple container is ever aliased.
fn nara_tuple_copy_into(
    e: &mut NaraEmit,
    dst_rf: u8,
    src_rf: u8,
    kinds: &[NaraKind],
    span: Span,
) -> bool {
    let (values, refs) = tuple_lanes(kinds);
    if values > u8::MAX as usize || refs > u8::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm tuple has more than 255 elements in one register lane")
                .with_label(span, "tuple copied here")
                .with_note("split the tuple into smaller tuples")
                .with_code("E404"),
        );
        return false;
    }
    e.bytecode
        .extend_from_slice(&[0x27, dst_rf, values as u8, refs as u8]); // createi
                                                                       // One scratch per lane, reused across elements; freed after the copy.
    let scratch_rv = if values > 0 {
        match e.fresh_rv(span) {
            Some(rv) => Some(rv),
            None => return false,
        }
    } else {
        None
    };
    let scratch_rf = if refs > 0 {
        match e.fresh_rf(span) {
            Some(rf) => Some(rf),
            None => {
                if let Some(rv) = scratch_rv {
                    e.free_rv.push(rv);
                }
                return false;
            }
        }
    } else {
        None
    };
    for (i, kind) in kinds.iter().enumerate() {
        let Some((is_ref, slot)) = tuple_slot(kinds, i) else {
            if let Some(rv) = scratch_rv {
                e.free_rv.push(rv);
            }
            if let Some(rf) = scratch_rf {
                e.free_rf.push(rf);
            }
            return false;
        };
        let Ok(slot) = u8::try_from(slot) else {
            e.diags.push(
                Diagnostic::error("Naravm tuple slot is out of range (compiler bug)")
                    .with_label(span, "tuple copied here")
                    .with_code("E500"),
            );
            if let Some(rv) = scratch_rv {
                e.free_rv.push(rv);
            }
            if let Some(rf) = scratch_rf {
                e.free_rf.push(rf);
            }
            return false;
        };
        if is_ref || kind.is_ref() {
            // Nested tuples recurse: the nested container is duplicated so
            // the copy owns every tuple level. Other references share.
            if let NaraKind::Tuple(nested) = kind {
                let nested = nested.clone();
                let (Some(nested_src), Some(nested_dst)) = (e.fresh_rf(span), e.fresh_rf(span))
                else {
                    if let Some(rv) = scratch_rv {
                        e.free_rv.push(rv);
                    }
                    if let Some(rf) = scratch_rf {
                        e.free_rf.push(rf);
                    }
                    return false;
                };
                e.bytecode
                    .extend_from_slice(&[0x2e, nested_src, src_rf, slot]); // getrfati
                if !nara_tuple_copy_into(e, nested_dst, nested_src, &nested, span) {
                    e.free_rf.push(nested_src);
                    e.free_rf.push(nested_dst);
                    if let Some(rv) = scratch_rv {
                        e.free_rv.push(rv);
                    }
                    if let Some(rf) = scratch_rf {
                        e.free_rf.push(rf);
                    }
                    return false;
                }
                e.bytecode
                    .extend_from_slice(&[0x2f, dst_rf, slot, nested_dst]); // setrfati
                e.free_rf.push(nested_src);
                e.free_rf.push(nested_dst);
                continue;
            }
            let tmp = scratch_rf.expect("ref scratch exists when a ref element is copied");
            e.bytecode.extend_from_slice(&[0x2e, tmp, src_rf, slot]); // getrfati
            e.bytecode.extend_from_slice(&[0x2f, dst_rf, slot, tmp]); // setrfati
        } else {
            let tmp = scratch_rv.expect("value scratch exists when a value element is copied");
            e.bytecode.extend_from_slice(&[0x2c, tmp, src_rf, slot]); // getvati
            e.bytecode.extend_from_slice(&[0x2d, dst_rf, slot, tmp]); // setvati
        }
    }
    if let Some(rv) = scratch_rv {
        e.free_rv.push(rv);
    }
    if let Some(rf) = scratch_rf {
        e.free_rf.push(rf);
    }
    true
}

/// Container lane sizes for a fallible value: `(values, refs)`. Value
/// slot 0 is the tag (0 = ok, 1 = error); value slot 1 is the error code
/// on the err path or the first value-kind payload slot on the ok path.
/// Error payloads live past the code (value slots from 2, reference slots
/// from 0) in a region spanning the program's maximum lanes, so `try`
/// forwards any variant blindly.
fn nara_fallible_lanes(ok: &NaraKind, err_lanes: (usize, usize)) -> (usize, usize) {
    let (v, r) = match ok {
        NaraKind::Tuple(kinds) => tuple_lanes(kinds),
        kind if kind.is_ref() => (0, 1),
        _ => (1, 0),
    };
    ((1 + v).max(2 + err_lanes.0), r.max(err_lanes.1))
}

/// Payload position -> (uses-reference-lane, lane-local slot) for a
/// fallible error region. Value slot 0 holds the tag and slot 1 the code,
/// so value-kind payloads start at slot 2; reference-kind payloads start
/// at slot 0.
fn fallible_err_slot(kinds: &[NaraKind], index: usize) -> Option<(bool, usize)> {
    let (is_ref, lane_slot) = tuple_slot(kinds, index)?;
    Some(if is_ref {
        (true, lane_slot)
    } else {
        (false, lane_slot + 2)
    })
}

/// Allocate a fallible container (`createi`) sized for an `ok` payload
/// kind into an already-reserved register. E404 when a lane exceeds 255
/// slots. Both arms of a checked call build into the same register so the
/// join sees one value.
fn nara_fallible_create_into(
    e: &mut NaraEmit,
    into: u8,
    ok: &NaraKind,
    err_lanes: (usize, usize),
    span: Span,
) -> bool {
    let (values, refs) = nara_fallible_lanes(ok, err_lanes);
    if values > u8::MAX as usize || refs > u8::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm fallible value has more than 255 slots in one register lane")
                .with_label(span, "fallible allocated here")
                .with_note("split the payload into smaller tuples")
                .with_code("E404"),
        );
        return false;
    }
    e.bytecode
        .extend_from_slice(&[0x27, into, values as u8, refs as u8]); // createi
    true
}

/// Allocate a fallible container (`createi`) sized for an `ok` payload
/// kind. E404 when a lane exceeds 255 slots.
fn nara_fallible_create(
    e: &mut NaraEmit,
    ok: &NaraKind,
    err_lanes: (usize, usize),
    span: Span,
) -> Option<u8> {
    let (values, refs) = nara_fallible_lanes(ok, err_lanes);
    if values > u8::MAX as usize || refs > u8::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm fallible value has more than 255 slots in one register lane")
                .with_label(span, "fallible allocated here")
                .with_note("split the payload into smaller tuples")
                .with_code("E404"),
        );
        return None;
    }
    let rf = e.fresh_rf(span)?;
    e.bytecode
        .extend_from_slice(&[0x27, rf, values as u8, refs as u8]); // createi
    Some(rf)
}

/// Store the tag (0 = ok, 1 = error) into value slot 0 of a fallible
/// container.
fn nara_fallible_tag(e: &mut NaraEmit, dst: u8, tag: u64, span: Span) -> bool {
    let Some(idx) = e.add_value(tag, span) else {
        return false;
    };
    let Some(rv) = e.fresh_rv(span) else {
        return false;
    };
    e.load_constant(false, rv, idx);
    e.bytecode.extend_from_slice(&[0x2d, dst, 0, rv]); // setvati
    e.free_rv.push(rv);
    true
}

/// Store an ok payload into a fallible container (ok path). `payload` is
/// a value reg for value-kind singles, a ref reg for reference singles, or
/// a container for tuples. Value payload slot `j` lands in container value
/// slot `j + 1` (slot 0 is the tag); reference slots keep their index.
fn nara_fallible_store(e: &mut NaraEmit, dst: u8, payload: u8, ok: &NaraKind, span: Span) -> bool {
    if let NaraKind::Tuple(nested) = ok {
        let nested = nested.clone();
        let (values, refs) = tuple_lanes(&nested);
        let scratch_rv = if values > 0 {
            match e.fresh_rv(span) {
                Some(rv) => Some(rv),
                None => return false,
            }
        } else {
            None
        };
        let scratch_rf = if refs > 0 {
            match e.fresh_rf(span) {
                Some(rf) => Some(rf),
                None => {
                    if let Some(rv) = scratch_rv {
                        e.free_rv.push(rv);
                    }
                    return false;
                }
            }
        } else {
            None
        };
        let mut failed = false;
        for (i, kind) in nested.iter().enumerate() {
            let Some((is_ref, slot)) = tuple_slot(&nested, i) else {
                failed = true;
                break;
            };
            let Ok(slot) = u8::try_from(slot) else {
                e.diags.push(
                    Diagnostic::error("Naravm tuple slot is out of range (compiler bug)")
                        .with_label(span, "fallible payload stored here")
                        .with_code("E500"),
                );
                failed = true;
                break;
            };
            if let NaraKind::Tuple(inner) = kind {
                // Nested tuples duplicate so the fallible owns every level.
                let inner = inner.clone();
                let (Some(nested_src), Some(nested_dst)) = (e.fresh_rf(span), e.fresh_rf(span))
                else {
                    failed = true;
                    break;
                };
                e.bytecode
                    .extend_from_slice(&[0x2e, nested_src, payload, slot]); // getrfati
                if !nara_tuple_copy_into(e, nested_dst, nested_src, &inner, span) {
                    e.free_rf.push(nested_src);
                    e.free_rf.push(nested_dst);
                    failed = true;
                    break;
                }
                e.bytecode.extend_from_slice(&[0x2f, dst, slot, nested_dst]); // setrfati
                e.free_rf.push(nested_src);
                e.free_rf.push(nested_dst);
                continue;
            }
            if is_ref {
                let Some(tmp) = scratch_rf else {
                    failed = true;
                    break;
                };
                e.bytecode.extend_from_slice(&[0x2e, tmp, payload, slot]); // getrfati
                e.bytecode.extend_from_slice(&[0x2f, dst, slot, tmp]); // setrfati
            } else {
                let Some(tmp) = scratch_rv else {
                    failed = true;
                    break;
                };
                // Value payload slot `j` lands in container value slot
                // `j + 1` (slot 0 is the tag); lanes are ≤ 255 by
                // construction, so the shift always fits.
                let dst_slot = slot.saturating_add(1);
                e.bytecode.extend_from_slice(&[0x2c, tmp, payload, slot]); // getvati
                e.bytecode.extend_from_slice(&[0x2d, dst, dst_slot, tmp]); // setvati
            }
        }
        if let Some(rv) = scratch_rv {
            e.free_rv.push(rv);
        }
        if let Some(rf) = scratch_rf {
            e.free_rf.push(rf);
        }
        return !failed;
    }
    if ok.is_ref() {
        e.bytecode.extend_from_slice(&[0x2f, dst, 0, payload]); // setrfati
    } else {
        e.bytecode.extend_from_slice(&[0x2d, dst, 1, payload]); // setvati
    }
    true
}

/// Store error payload registers into a fallible container's error region
/// (err path): value payloads land in value slots from 2 (slots 0-1 are
/// the tag and code), reference payloads in reference slots from 0.
/// `kinds` are the erased payload kinds in order (from the instruction's
/// `tys`). Nested tuples duplicate so the container owns every level.
fn nara_fallible_store_err(
    e: &mut NaraEmit,
    dst: u8,
    args: &[vl_lir::Reg],
    kinds: &[NaraKind],
    span: Span,
) -> bool {
    for (i, arg) in args.iter().enumerate() {
        let Some(kind) = kinds.get(i).cloned() else {
            e.diags.push(
                Diagnostic::error("Naravm error payload index out of range (compiler bug)")
                    .with_label(span, "error wrapped here")
                    .with_code("E500"),
            );
            return false;
        };
        let Some((is_ref, slot)) = fallible_err_slot(kinds, i) else {
            e.diags.push(
                Diagnostic::error("Naravm error payload index out of range (compiler bug)")
                    .with_label(span, "error wrapped here")
                    .with_code("E500"),
            );
            return false;
        };
        let Ok(slot) = u8::try_from(slot) else {
            e.diags.push(
                Diagnostic::error("Naravm error payload slot is out of range (compiler bug)")
                    .with_label(span, "error wrapped here")
                    .with_code("E500"),
            );
            return false;
        };
        if is_ref {
            let Some(v) = e.ref_reg(*arg, span) else {
                return false;
            };
            if let NaraKind::Tuple(nested) = kind {
                let Some(tmp) = e.fresh_rf(span) else {
                    return false;
                };
                if !nara_tuple_copy_into(e, tmp, v, &nested, span) {
                    e.free_rf.push(tmp);
                    return false;
                }
                e.bytecode.extend_from_slice(&[0x2f, dst, slot, tmp]); // setrfati
                e.free_rf.push(tmp);
            } else {
                e.bytecode.extend_from_slice(&[0x2f, dst, slot, v]); // setrfati
            }
        } else {
            let Some(v) = e.value_reg(*arg, span) else {
                return false;
            };
            e.bytecode.extend_from_slice(&[0x2d, dst, slot, v]); // setvati
        }
    }
    true
}

/// Load an ok payload from a fallible container (ok path). Returns the
/// lane and the fresh register holding the payload: a value reg, a ref
/// reg, or (tuples) a freshly allocated container. The dummy `U64` lane
/// covers `E!void` (unreachable live; keeps the instruction shape).
fn nara_fallible_load(
    e: &mut NaraEmit,
    scrut: u8,
    ok: &NaraKind,
    span: Span,
) -> Option<(bool, u8)> {
    if let NaraKind::Tuple(nested) = ok {
        let nested = nested.clone();
        let (values, refs) = tuple_lanes(&nested);
        if values > u8::MAX as usize || refs > u8::MAX as usize {
            e.diags.push(
                Diagnostic::error(
                    "Naravm fallible value has more than 255 slots in one register lane",
                )
                .with_label(span, "fallible payload read here")
                .with_code("E404"),
            );
            return None;
        }
        let dst = e.fresh_rf(span)?;
        e.bytecode
            .extend_from_slice(&[0x27, dst, values as u8, refs as u8]); // createi
        let scratch_rv = if values > 0 { e.fresh_rv(span) } else { None };
        let scratch_rf = if refs > 0 { e.fresh_rf(span) } else { None };
        if (values > 0 && scratch_rv.is_none()) || (refs > 0 && scratch_rf.is_none()) {
            if let Some(rv) = scratch_rv {
                e.free_rv.push(rv);
            }
            if let Some(rf) = scratch_rf {
                e.free_rf.push(rf);
            }
            e.free_rf.push(dst);
            return None;
        }
        let mut failed = false;
        for (i, kind) in nested.iter().enumerate() {
            let (is_ref, slot) = match tuple_slot(&nested, i) {
                Some(slot) => slot,
                None => {
                    failed = true;
                    break;
                }
            };
            let Ok(slot) = u8::try_from(slot) else {
                failed = true;
                break;
            };
            if let NaraKind::Tuple(inner) = kind {
                let inner = inner.clone();
                let (Some(nested_src), Some(nested_dst)) = (e.fresh_rf(span), e.fresh_rf(span))
                else {
                    failed = true;
                    break;
                };
                e.bytecode
                    .extend_from_slice(&[0x2e, nested_src, scrut, slot]); // getrfati
                if !nara_tuple_copy_into(e, nested_dst, nested_src, &inner, span) {
                    e.free_rf.push(nested_src);
                    e.free_rf.push(nested_dst);
                    failed = true;
                    break;
                }
                e.bytecode.extend_from_slice(&[0x2f, dst, slot, nested_dst]); // setrfati
                e.free_rf.push(nested_src);
                e.free_rf.push(nested_dst);
                continue;
            }
            if is_ref {
                let tmp = scratch_rf.expect("ref scratch exists for ref payloads");
                e.bytecode.extend_from_slice(&[0x2e, tmp, scrut, slot]); // getrfati
                e.bytecode.extend_from_slice(&[0x2f, dst, slot, tmp]); // setrfati
            } else {
                let tmp = scratch_rv.expect("value scratch exists for value payloads");
                let src_slot = slot.saturating_add(1);
                e.bytecode.extend_from_slice(&[0x2c, tmp, scrut, src_slot]); // getvati
                e.bytecode.extend_from_slice(&[0x2d, dst, slot, tmp]); // setvati
            }
        }
        if let Some(rv) = scratch_rv {
            e.free_rv.push(rv);
        }
        if let Some(rf) = scratch_rf {
            e.free_rf.push(rf);
        }
        if failed {
            e.free_rf.push(dst);
            return None;
        }
        return Some((true, dst));
    }
    if ok.is_ref() {
        let dst = e.fresh_rf(span)?;
        e.bytecode.extend_from_slice(&[0x2e, dst, scrut, 0]); // getrfati
        return Some((true, dst));
    }
    let dst = e.fresh_rv(span)?;
    e.bytecode.extend_from_slice(&[0x2c, dst, scrut, 1]); // getvati
    Some((false, dst))
}

struct NaraEmit {
    blob: Vec<u8>,
    values: Vec<u64>,
    value_index: std::collections::HashMap<u64, usize>,
    constants: Vec<NaraConstant>,
    /// Shared native constant for String equality operators across functions.
    string_eq_fn_idx: Option<usize>,
    bytecode: Vec<u8>,
    diags: Vec<Diagnostic>,
    rv_map: std::collections::HashMap<vl_lir::Reg, u8>,
    rf_map: std::collections::HashMap<vl_lir::Reg, u8>,
    kinds: std::collections::HashMap<vl_lir::Reg, NaraKind>,
    invalid: std::collections::HashSet<vl_lir::Reg>,
    next_rv: u8,
    next_rf: u8,
    /// Recycled machine registers whose LIR value is dead past its last use.
    free_rv: Vec<u8>,
    free_rf: Vec<u8>,
    /// LIR reg -> index of its last use. After emitting that use the
    /// machine register is recycled. Loop back edges extend liveness to the
    /// jump (see `nara_last_use`), so mid-loop frees never clobber values
    /// that re-execute.
    last_use: std::collections::HashMap<vl_lir::Reg, usize>,
    label_pos: std::collections::HashMap<u32, usize>,
    patches: Vec<NaraPatch>,
    one_rv: Option<u8>,
    bias_rv: Option<u8>,
    zero_rv: Option<u8>,
    /// Next userland parameter slots for the function being emitted. Value
    /// and reference parameters count independently from `rv11` / `rf31`
    /// (matching the Naravm native convention).
    param_vi: u8,
    param_ri: u8,
}

struct NaraPatch {
    pos: usize,
    len: usize,
    target: u32,
    span: Span,
}

/// Stage call arguments on Naravm's separate value/reference stacks before
/// writing ABI registers, so one destination cannot destroy a later source.
fn nara_stage_call_args(e: &mut NaraEmit, actuals: &[(bool, u8)]) {
    let mut values = 0u8;
    let mut refs = 0u8;
    for (is_ref, src) in actuals {
        if *is_ref {
            e.bytecode.extend_from_slice(&[0x08, *src]);
            refs += 1;
        } else {
            e.bytecode.extend_from_slice(&[0x06, *src]);
            values += 1;
        }
    }
    for (is_ref, _) in actuals.iter().rev() {
        if *is_ref {
            refs -= 1;
            e.bytecode.extend_from_slice(&[0x09, 0x31 + refs]);
        } else {
            values -= 1;
            e.bytecode.extend_from_slice(&[0x07, 0x11 + values]);
        }
    }
}

/// Capture fixed native result registers before any temporary allocation can
/// reuse one of them. Values and references use independent VM stacks.
fn nara_stage_native_results(
    e: &mut NaraEmit,
    results: &[(bool, u8)],
    span: Span,
) -> Option<Vec<u8>> {
    for (is_ref, src) in results {
        e.bytecode
            .extend_from_slice(&[if *is_ref { 0x08 } else { 0x06 }, *src]);
    }
    let mut staged = Vec::with_capacity(results.len());
    for (is_ref, _) in results {
        staged.push(if *is_ref {
            e.fresh_rf(span)?
        } else {
            e.fresh_rv(span)?
        });
    }
    for (index, (is_ref, _)) in results.iter().enumerate().rev() {
        e.bytecode
            .extend_from_slice(&[if *is_ref { 0x09 } else { 0x07 }, staged[index]]);
    }
    Some(staged)
}

fn nara_spill_allocated(e: &mut NaraEmit) -> Vec<NaraSpill> {
    let mut rvs: Vec<u8> = e.rv_map.values().copied().collect();
    rvs.sort_unstable();
    rvs.dedup();
    let mut rfs: Vec<u8> = e.rf_map.values().copied().collect();
    rfs.sort_unstable();
    rfs.dedup();
    let mut spills = Vec::new();
    for rv in rvs {
        e.bytecode.extend_from_slice(&[0x06, rv]);
        spills.push(NaraSpill::V(rv));
    }
    for rv in [e.one_rv, e.bias_rv, e.zero_rv].into_iter().flatten() {
        if spills
            .iter()
            .any(|spill| matches!(spill, NaraSpill::V(saved) if *saved == rv))
        {
            continue;
        }
        e.bytecode.extend_from_slice(&[0x06, rv]);
        spills.push(NaraSpill::V(rv));
    }
    for rf in rfs {
        e.bytecode.extend_from_slice(&[0x08, rf]);
        spills.push(NaraSpill::F(rf));
    }
    spills
}

fn nara_restore_spills(e: &mut NaraEmit, spills: &[NaraSpill]) {
    for spill in spills.iter().rev() {
        match spill {
            NaraSpill::V(rv) => e.bytecode.extend_from_slice(&[0x07, *rv]),
            NaraSpill::F(rf) => e.bytecode.extend_from_slice(&[0x09, *rf]),
        }
    }
}

impl NaraEmit {
    /// Reset per-function state before emitting the next Nara function. The
    /// constant/blob/value pools (and diagnostics) are shared across the
    /// whole program; everything else starts over.
    fn reset_fn(&mut self, last_use: std::collections::HashMap<vl_lir::Reg, usize>) {
        self.bytecode = Vec::new();
        self.rv_map.clear();
        self.rf_map.clear();
        self.kinds.clear();
        self.invalid.clear();
        self.next_rv = 0;
        self.next_rf = 0x20;
        self.free_rv.clear();
        self.free_rf.clear();
        self.last_use = last_use;
        self.label_pos.clear();
        self.patches.clear();
        self.one_rv = None;
        self.bias_rv = None;
        self.zero_rv = None;
        self.param_vi = 0;
        self.param_ri = 0;
    }

    fn fresh_rv(&mut self, span: Span) -> Option<u8> {
        if let Some(rv) = self.free_rv.pop() {
            return Some(rv);
        }
        loop {
            let rv = self.next_rv;
            if rv > 0x1f {
                self.diags.push(reg_oob(span, rv as u32));
                return None;
            }
            self.next_rv += 1;
            // Reserve rv10 (error channel) and rv11 (call argument) for the
            // calling convention instead of general allocation.
            if rv == 0x10 || rv == 0x11 {
                continue;
            }
            return Some(rv);
        }
    }

    fn fresh_rf(&mut self, span: Span) -> Option<u8> {
        if let Some(rf) = self.free_rf.pop() {
            // Never recycle the reserved module-state register.
            if rf == MODULE_STATE_RF {
                return self.fresh_rf(span);
            }
            return Some(rf);
        }
        loop {
            let rf = self.next_rf;
            if rf > 0x3f {
                self.diags.push(reg_oob(span, rf as u32));
                return None;
            }
            self.next_rf += 1;
            // Reserve rf31 (userland 0x31) as the single reference argument
            // register for calls, and rf3F (0x3F, callee-saved) for module
            // state (global storage container).
            if rf == 0x31 || rf == MODULE_STATE_RF {
                continue;
            }
            return Some(rf);
        }
    }

    fn add_string(&mut self, value: &[u8], span: Span) -> Option<usize> {
        let offset = self.blob.len();
        self.blob.extend_from_slice(value);
        let idx = self.constants.len();
        self.constants.push(NaraConstant::String {
            offset,
            len: value.len(),
        });
        if idx > u16::MAX as usize {
            self.diags.push(
                Diagnostic::error("Naravm constant pool has more than 65536 entries")
                    .with_label(span, "defined here")
                    .with_note("String references use a 16-bit constant index")
                    .with_code("E405"),
            );
            return None;
        }
        Some(idx)
    }

    fn add_value(&mut self, bits: u64, span: Span) -> Option<usize> {
        if let Some(idx) = self.value_index.get(&bits) {
            return Some(*idx);
        }
        let value_idx = self.values.len();
        self.values.push(bits);
        let idx = self.constants.len();
        self.constants.push(NaraConstant::Value { value_idx });
        self.value_index.insert(bits, idx);
        if idx > u16::MAX as usize {
            self.diags.push(
                Diagnostic::error("Naravm constant pool has more than 65536 entries")
                    .with_label(span, "defined here")
                    .with_note("value references use a 16-bit constant index")
                    .with_code("E405"),
            );
            return None;
        }
        Some(idx)
    }

    /// Use compact loads for low indices and the VM's big-endian u16 loads
    /// for the rest. Constant insertion validates the index before emission.
    fn load_constant(&mut self, is_ref: bool, reg: u8, idx: usize) {
        if let Ok(idx) = u8::try_from(idx) {
            self.bytecode
                .extend_from_slice(&[if is_ref { 0x03 } else { 0x02 }, reg, idx]);
        } else {
            let idx = u16::try_from(idx).expect("constant insertion validates the u16 index");
            self.bytecode
                .extend_from_slice(&[if is_ref { 0x0d } else { 0x0c }, reg]);
            put_u16(&mut self.bytecode, idx);
        }
    }

    fn ensure_one(&mut self, span: Span) -> Option<u8> {
        let idx = self.add_value(1, span)?;
        let rv = match self.one_rv {
            Some(rv) => rv,
            None => self.fresh_rv(span)?,
        };
        // Reload at each use: the first use may be in a skipped branch.
        self.load_constant(false, rv, idx);
        self.one_rv = Some(rv);
        Some(rv)
    }

    /// A cached zero register, for container `create` calls that need an
    /// explicit "0 references" count operand.
    fn ensure_zero(&mut self, span: Span) -> Option<u8> {
        let idx = self.add_value(0, span)?;
        let rv = match self.zero_rv {
            Some(rv) => rv,
            None => self.fresh_rv(span)?,
        };
        // Reload at each use: the first use may be in a skipped branch.
        self.load_constant(false, rv, idx);
        self.zero_rv = Some(rv);
        Some(rv)
    }

    fn ensure_bias(&mut self, span: Span) -> Option<u8> {
        let idx = self.add_value(0x8000_0000_0000_0000, span)?;
        let rv = match self.bias_rv {
            Some(rv) => rv,
            None => self.fresh_rv(span)?,
        };
        // Reload at each use: the first use may be in a skipped branch.
        self.load_constant(false, rv, idx);
        self.bias_rv = Some(rv);
        Some(rv)
    }

    fn value_reg(&mut self, reg: vl_lir::Reg, span: Span) -> Option<u8> {
        if let Some(rv) = self.rv_map.get(&reg).copied() {
            return Some(rv);
        }
        if self.invalid.contains(&reg) {
            return None;
        }
        self.diags.push(
            Diagnostic::error("Naravm backend could not resolve a value register")
                .with_label(span, "used here")
                .with_code("E500"),
        );
        None
    }

    fn ref_reg(&mut self, reg: vl_lir::Reg, span: Span) -> Option<u8> {
        if let Some(rf) = self.rf_map.get(&reg).copied() {
            return Some(rf);
        }
        if self.invalid.contains(&reg) {
            return None;
        }
        self.diags.push(
            Diagnostic::error("Naravm backend requires a String argument to std.print")
                .with_label(span, "unsupported argument")
                .with_code("E402"),
        );
        None
    }

    /// Resolve an `Array[T]` operand to its reference register plus the
    /// element kind (which selects `vat` vs `rfat` ops). Types were enforced
    /// upstream, so anything unresolvable here is either poisoned (quiet) or
    /// a compiler bug (loud E500).
    fn array_reg(&mut self, reg: vl_lir::Reg, span: Span) -> Option<(u8, NaraKind)> {
        if let Some(NaraKind::Array(elem)) = self.kinds.get(&reg).cloned() {
            if let Some(rf) = self.rf_map.get(&reg).copied() {
                return Some((rf, *elem));
            }
        } else if !self.invalid.contains(&reg) && self.kinds.contains_key(&reg) {
            self.diags.push(
                Diagnostic::error("Naravm backend found a non-array where an array was expected")
                    .with_label(span, "emitted here")
                    .with_code("E500"),
            );
            return None;
        }
        if self.invalid.contains(&reg) {
            return None;
        }
        self.diags.push(
            Diagnostic::error("Naravm backend could not resolve an array register (compiler bug)")
                .with_label(span, "emitted here")
                .with_code("E500"),
        );
        None
    }
}

/// Per-function emission context: the LIR signature plus program-wide
/// callee tables built by the pre-pass.
#[derive(Clone, Copy)]
struct NaraFnCtx<'a> {
    func: &'a vl_lir::Function,
    is_main: bool,
    sigs: &'a std::collections::HashMap<&'a str, &'a vl_lir::Function>,
    fn_consts: &'a std::collections::HashMap<String, usize>,
    imported_fn_consts: &'a std::collections::HashMap<vl_lir::FunctionRef, usize>,
    imports: &'a std::collections::HashMap<vl_lir::FunctionRef, vl_lir::FunctionImport>,
    module: &'a str,
    objects: &'a std::collections::HashMap<&'a str, &'a vl_lir::ObjectDef>,
    print_fn_idx: usize,
    print_u64_fn_idx: usize,
    /// Stable global ID -> (is_ref lane, slot in container).
    global_slots: &'a std::collections::HashMap<u32, (bool, u8)>,
    /// Stable global ID -> runtime-erased global type (for dst kinds).
    global_tys: &'a std::collections::HashMap<u32, vl_typecheck::Ty>,
    /// Program-wide maximum error-payload lanes (value slots, reference
    /// slots): sizes every fallible container's error region.
    err_lanes: (usize, usize),
}

/// Last-use map for one initializer fragment (straight-line temps die at
/// their last textual use; branchy `&&`/`||` inits use offset labels above).
/// The fragment `result` counts as used at the end (by the store), so its
/// defining `Const`/`StringConst`/`NewArray` allocates a register.
fn nara_last_use_fragment(
    instrs: &[Instr],
    result: &vl_lir::Reg,
) -> std::collections::HashMap<vl_lir::Reg, usize> {
    use vl_lir::Instr as I;
    let mut uses: std::collections::HashMap<vl_lir::Reg, Vec<usize>> =
        std::collections::HashMap::new();
    let mut touch = |reg: vl_lir::Reg, idx: usize| {
        uses.entry(reg).or_default().push(idx);
    };
    for (idx, ins) in instrs.iter().enumerate() {
        match ins {
            I::BinOp { lhs, rhs, .. } => {
                touch(*lhs, idx);
                touch(*rhs, idx);
            }
            I::Copy { src, .. }
            | I::Cast { src, .. }
            | I::Not { src, .. }
            | I::GlobalStore { src, .. }
            | I::BranchIfFalse { cond: src, .. }
            | I::Ret { src, .. } => {
                touch(*src, idx);
            }
            I::Call { args, .. } | I::ArrayLit { elems: args, .. } => {
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::NewVariant { args, .. } => {
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::WrapOk { value, .. } => {
                touch(*value, idx);
            }
            I::WrapErr { code, args, .. } => {
                touch(*code, idx);
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::RewrapErr { scrut, .. } => {
                touch(*scrut, idx);
            }
            I::UnwrapOk { scrut, .. } | I::UnwrapErr { scrut, .. } => {
                touch(*scrut, idx);
            }
            I::TagOf { scrut, .. } => {
                touch(*scrut, idx);
            }
            I::PayloadGet { scrut, .. } | I::ErrPayloadGet { scrut, .. } => {
                touch(*scrut, idx);
            }
            I::TupleLit { elems, .. } => {
                for arg in elems {
                    touch(*arg, idx);
                }
            }
            I::TupleGet { tuple, .. } => {
                touch(*tuple, idx);
            }
            I::TupleSet { tuple, value, .. } => {
                touch(*tuple, idx);
                touch(*value, idx);
            }
            I::NewArray { len, .. } => {
                touch(*len, idx);
            }
            I::ArrayGet { array, index, .. } => {
                touch(*array, idx);
                touch(*index, idx);
            }
            I::ArrayLen { array, .. } => touch(*array, idx),
            I::ArraySet {
                array,
                index,
                value,
                ..
            } => {
                touch(*array, idx);
                touch(*index, idx);
                touch(*value, idx);
            }
            I::NewObject { fields, .. } => {
                for (_, value) in fields {
                    touch(*value, idx);
                }
            }
            I::ObjectGet { object, .. } => touch(*object, idx),
            I::ObjectSet { object, value, .. } => {
                touch(*object, idx);
                touch(*value, idx);
            }
            I::Const { .. }
            | I::StringConst { .. }
            | I::Param { .. }
            | I::GlobalLoad { .. }
            | I::Jump { .. }
            | I::Label { .. } => {}
        }
    }
    // The fragment result is consumed by the store after the last
    // instruction; without this, terminal `Const`/`NewArray`/etc. look dead
    // and allocate no register, failing the store with E500.
    if !instrs.is_empty() {
        touch(*result, instrs.len());
    }
    let mut last = std::collections::HashMap::new();
    for (reg, idxs) in uses {
        if let Some(end) = idxs.iter().copied().max() {
            last.insert(reg, end);
        }
    }
    last
}

/// Remap branch labels in one initializer fragment by `base` so inlined
/// initializers never collide with each other or the main body.
fn remap_labels(ins: &Instr, base: u32) -> Instr {
    use vl_lir::Instr as I;
    match ins {
        I::BranchIfFalse { cond, target, span } => I::BranchIfFalse {
            cond: *cond,
            target: target + base,
            span: *span,
        },
        I::Jump { target, span } => I::Jump {
            target: target + base,
            span: *span,
        },
        I::Label { id, span } => I::Label {
            id: id + base,
            span: *span,
        },
        _ => ins.clone(),
    }
}

/// Free every machine register used by one initializer fragment after its
/// result was stored, so the next initializer and the main body reuse them.
fn free_fragment_regs(e: &mut NaraEmit, instrs: &[Instr], result: &vl_lir::Reg) {
    use vl_lir::Instr as I;
    let mut lir_regs: std::collections::HashSet<vl_lir::Reg> = std::collections::HashSet::new();
    lir_regs.insert(*result);
    for ins in instrs {
        match ins {
            I::Const { dst, .. }
            | I::StringConst { dst, .. }
            | I::Param { dst, .. }
            | I::Copy { dst, .. }
            | I::Not { dst, .. }
            | I::BinOp { dst, .. }
            | I::Call { dst, .. }
            | I::NewArray { dst, .. }
            | I::ArrayLit { dst, .. }
            | I::ArrayGet { dst, .. }
            | I::ArrayLen { dst, .. }
            | I::TupleLit { dst, .. }
            | I::TupleGet { dst, .. }
            | I::NewVariant { dst, .. }
            | I::WrapOk { dst, .. }
            | I::WrapErr { dst, .. }
            | I::RewrapErr { dst, .. }
            | I::UnwrapOk { dst, .. }
            | I::UnwrapErr { dst, .. }
            | I::TagOf { dst, .. }
            | I::PayloadGet { dst, .. }
            | I::ErrPayloadGet { dst, .. }
            | I::GlobalLoad { dst, .. }
            | I::Cast { dst, .. } => {
                lir_regs.insert(*dst);
            }
            I::ObjectGet { dst, .. } => {
                lir_regs.insert(*dst);
            }
            _ => {}
        }
    }
    for reg in lir_regs {
        if let Some(rv) = e.rv_map.remove(&reg) {
            if rv != 0x10 && rv != 0x11 {
                e.free_rv.push(rv);
            }
        }
        if let Some(rf) = e.rf_map.remove(&reg) {
            if rf != 0x31 && rf != MODULE_STATE_RF {
                e.free_rf.push(rf);
            }
        }
        e.kinds.remove(&reg);
        e.invalid.remove(&reg);
    }
    // Fragment branch labels live in `label_pos`/`patches` by remapped ID;
    // they stay (positions are absolute) and never collide thanks to `base`.
}

fn nara_vmfile(prog: &LirProgram, diags: &mut Vec<Diagnostic>) -> Option<Vec<u8>> {
    // Target-neutral LIR must already be runtime-erased; a leaked `*`,
    // `Param`, `int`, or `Error` is an internal compiler bug, not a silent
    // representation choice.
    if let Some(bad) = prog.validate_runtime() {
        diags.push(Diagnostic::error(format!("internal compiler error: {bad}")).with_code("E500"));
        return None;
    }
    let mut e = NaraEmit {
        blob: Vec::new(),
        values: Vec::new(),
        value_index: std::collections::HashMap::new(),
        constants: Vec::new(),
        string_eq_fn_idx: None,
        bytecode: Vec::new(),
        diags: Vec::new(),
        rv_map: std::collections::HashMap::new(),
        rf_map: std::collections::HashMap::new(),
        kinds: std::collections::HashMap::new(),
        invalid: std::collections::HashSet::new(),
        next_rv: 0,
        next_rf: 0x20,
        free_rv: Vec::new(),
        free_rf: Vec::new(),
        last_use: std::collections::HashMap::new(),
        label_pos: std::collections::HashMap::new(),
        patches: Vec::new(),
        one_rv: None,
        bias_rv: None,
        zero_rv: None,
        param_vi: 0,
        param_ri: 0,
    };
    // Module-state layout: separate value and reference slots in one GC
    // container. Stable global IDs map to (lane, slot); counts size the
    // `createi` at the entrypoint.
    let mut global_slots: std::collections::HashMap<u32, (bool, u8)> =
        std::collections::HashMap::new();
    let mut global_tys: std::collections::HashMap<u32, vl_typecheck::Ty> =
        std::collections::HashMap::new();
    let mut value_count: u8 = 0;
    let mut ref_count: u8 = 0;
    for g in &prog.globals {
        let Some(kind) = NaraKind::of_ty(&g.ty) else {
            diags.push(
                Diagnostic::error(format!(
                    "internal compiler error: global `{}` has non-runtime type `{}`",
                    g.name, g.ty
                ))
                .with_code("E500"),
            );
            return None;
        };
        let is_ref = kind.is_ref();
        if is_ref {
            if ref_count == u8::MAX {
                diags.push(
                    Diagnostic::error("too many reference globals for module state")
                        .with_code("E500"),
                );
                return None;
            }
            global_slots.insert(g.id, (true, ref_count));
            ref_count += 1;
        } else {
            if value_count == u8::MAX {
                diags.push(
                    Diagnostic::error("too many value globals for module state").with_code("E500"),
                );
                return None;
            }
            global_slots.insert(g.id, (false, value_count));
            value_count += 1;
        }
        global_tys.insert(g.id, g.ty.clone());
    }
    // Pre-intern the module name, entrypoint, and std exports. These occupy
    // the first constant slots; user strings/values follow.
    let module_idx = e
        .add_string(prog.module.as_bytes(), Span::empty(0))
        .unwrap_or(0);
    let owns_entrypoint =
        prog.entrypoint && prog.entrypoint_module.as_deref() == Some(prog.module.as_str());
    let entry_name_idx = if owns_entrypoint && prog.functions.iter().any(|f| f.name == "main") {
        e.add_string(b"<entrypoint>", Span::empty(0)).unwrap_or(0)
    } else {
        0
    };
    let std_idx = e.add_string(b"std", Span::empty(0)).unwrap_or(0);
    let print_idx = e.add_string(b"print", Span::empty(0)).unwrap_or(0);
    let print_u64_idx = e.add_string(b"print_u64", Span::empty(0)).unwrap_or(0);
    let print_fn_idx = e.constants.len();
    e.constants.push(NaraConstant::Function {
        module: std_idx,
        function: print_idx,
    });
    let print_u64_fn_idx = e.constants.len();
    e.constants.push(NaraConstant::Function {
        module: std_idx,
        function: print_u64_idx,
    });

    // Pre-pass: intern every user function name plus a function constant so
    // recursive calls resolve even when the callee is emitted later. `main`
    // is always ordinary callable code; the separate `<entrypoint>` wrapper
    // is the only VM entrypoint.
    let has_globals = !prog.globals.is_empty();
    let has_main = prog.functions.iter().any(|f| f.name == "main");
    // Keep library initialization as ordinary module metadata. A library must
    // not claim the VM's unique entrypoint; a future loader can invoke this
    // function before exposing the library's other functions.
    let has_entrypoint = owns_entrypoint && has_main;
    let module_init_name_idx = if !has_entrypoint && has_globals {
        e.add_string(b"<module-init>", Span::empty(0))
    } else {
        None
    };
    let mut fn_consts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut fn_names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in &prog.functions {
        if fn_consts.contains_key(&f.name) {
            continue;
        }
        if f.name == "main" {
            let Some(main_idx) = e.add_string(b"main", Span::empty(0)) else {
                continue;
            };
            if let Some(idx) = nara_push_fn_const(&mut e, module_idx, main_idx) {
                fn_consts.insert(f.name.clone(), idx);
                fn_names.insert(f.name.clone(), main_idx);
            }
            // Entrypoint const (not in fn_consts; startup only).
            if has_entrypoint {
                let _ = nara_push_fn_const(&mut e, module_idx, entry_name_idx);
            }
            continue;
        }
        let Some(name_idx) = e.add_string(f.name.as_bytes(), Span::empty(0)) else {
            continue;
        };
        if let Some(idx) = nara_push_fn_const(&mut e, module_idx, name_idx) {
            fn_consts.insert(f.name.clone(), idx);
            fn_names.insert(f.name.clone(), name_idx);
        }
    }
    let imports: std::collections::HashMap<vl_lir::FunctionRef, vl_lir::FunctionImport> = prog
        .imports
        .iter()
        .cloned()
        .map(|i| (i.symbol.clone(), i))
        .collect();
    let mut imported_fn_consts = std::collections::HashMap::new();
    let mut ordered_imports = imports.values().collect::<Vec<_>>();
    ordered_imports.sort_by(|a, b| {
        a.symbol
            .module
            .cmp(&b.symbol.module)
            .then_with(|| a.symbol.function.cmp(&b.symbol.function))
    });
    for import in ordered_imports {
        // Known VM natives are interned under their native module/function
        // names (`std::string::concat`); everything else keeps its LIR
        // spelling (user functions, cross-module source calls).
        let (vm_module, vm_function) =
            nara_extern_target(&import.symbol.module, &import.symbol.function)
                .map(|(m, f)| (m.to_owned(), f.to_owned()))
                .unwrap_or_else(|| (import.symbol.module.clone(), import.symbol.function.clone()));
        let Some(module) = e.add_string(vm_module.as_bytes(), Span::empty(0)) else {
            continue;
        };
        let Some(function) = e.add_string(vm_function.as_bytes(), Span::empty(0)) else {
            continue;
        };
        if let Some(idx) = nara_push_fn_const(&mut e, module, function) {
            imported_fn_consts.insert(import.symbol.clone(), idx);
        }
    }
    if e.diags.iter().any(|d| d.is_error()) {
        diags.append(&mut e.diags);
        return None;
    }

    let mut sigs: std::collections::HashMap<&str, &vl_lir::Function> =
        std::collections::HashMap::new();
    for f in &prog.functions {
        sigs.entry(f.name.as_str()).or_insert(f);
    }
    let objects: std::collections::HashMap<&str, &vl_lir::ObjectDef> = prog
        .objects
        .iter()
        .map(|object| (object.name.as_str(), object))
        .collect();

    let mut functions_out: Vec<(usize, Vec<u8>)> = Vec::new();
    for f in &prog.functions {
        let is_main = f.name == "main";
        // Entry function initializes module state before anything else, so
        // initializers may call functions that read earlier globals.
        // Liveness covers the main body; initializer temps below are emitted
        // inline and freed explicitly after each store.
        e.reset_fn(nara_last_use(f));
        let ctx = NaraFnCtx {
            func: f,
            is_main,
            sigs: &sigs,
            fn_consts: &fn_consts,
            imported_fn_consts: &imported_fn_consts,
            imports: &imports,
            module: &prog.module,
            objects: &objects,
            print_fn_idx,
            print_u64_fn_idx,
            global_slots: &global_slots,
            global_tys: &global_tys,
            err_lanes: prog.err_lanes,
        };
        if is_main {
            // Allocate the module-state container first (exact sizes).
            // Initializers run once at startup before the main body.
            // Note: a pathological recursive `main()` call would re-enter the
            // entrypoint and re-run initializers; no program does this.
            e.bytecode
                .extend_from_slice(&[0x27, MODULE_STATE_RF, value_count, ref_count]); // createi rf3F, V, R
                                                                                      // Run initializers in source order, storing each result.
            for (gi, g) in prog.globals.iter().enumerate() {
                let Some((is_ref, slot)) = global_slots.get(&g.id).copied() else {
                    continue;
                };
                // Remap initializer labels to avoid colliding with the main
                // body's 0-based labels or other initializers'. Stride 1M
                // exceeds any realistic label count (instr count bounded).
                let base = ((gi as u32) + 1) * 1_000_000;
                // Emit init body with a scratch last-use (straight-line temps
                // die at their last textual use inside this fragment).
                let frag_last = nara_last_use_fragment(&g.init, &g.result);
                let saved_last = std::mem::replace(&mut e.last_use, frag_last);
                for (frag_idx, ins) in g.init.iter().enumerate() {
                    let remapped = remap_labels(ins, base);
                    nara_instr(&mut e, &remapped, &ctx);
                    nara_free_unused_result(&mut e, &remapped);
                    // Free against the fragment's liveness, not the main's.
                    // (Uses `e.last_use` currently holding the fragment map;
                    // indices below are fragment-relative, which is fine for
                    // straight-line temps. Branchy inits use offset labels.)
                    nara_free_dead(&mut e, &remapped, frag_idx);
                    if e.diags.iter().any(|d| d.is_error()) {
                        break;
                    }
                }
                e.last_use = saved_last;
                if e.diags.iter().any(|d| d.is_error()) {
                    break;
                }
                // Store the computed result into its container slot.
                // (Empty/poisoned initializers have no result; lowering is
                // blocked on prior errors and the driver never emits here.)
                if g.init.is_empty() {
                    continue;
                }
                if is_ref {
                    let Some(rf) = e.rf_map.get(&g.result).copied() else {
                        e.diags.push(
                            Diagnostic::error(
                                "internal compiler error: reference global init did not produce a reference",
                            )
                            .with_code("E500"),
                        );
                        break;
                    };
                    e.bytecode
                        .extend_from_slice(&[0x2f, MODULE_STATE_RF, slot, rf]); // setrfati
                } else {
                    let Some(rv) = e.rv_map.get(&g.result).copied() else {
                        e.diags.push(
                            Diagnostic::error(
                                "internal compiler error: value global init did not produce a value",
                            )
                            .with_code("E500"),
                        );
                        break;
                    };
                    e.bytecode
                        .extend_from_slice(&[0x2d, MODULE_STATE_RF, slot, rv]); // setvati
                }
                // Free this fragment's temps so the next initializer and the
                // main body reuse machine registers.
                free_fragment_regs(&mut e, &g.init, &g.result);
            }
            if e.diags.iter().any(|d| d.is_error()) {
                break;
            }
        }
        // With globals, `main` calls resolve to the separate `main` body
        // below (no re-init); the `<entrypoint>` above runs inits once at
        // startup then calls it. Without globals, `main` IS the entrypoint.
        if is_main && has_entrypoint {
            // Call `main` (zero args, void) then bare return.
            if let Some(main_const) = fn_consts.get("main").copied() {
                nara_calli(&mut e, main_const, Span::empty(0));
            } else {
                e.diags.push(
                    Diagnostic::error("internal compiler error: missing main function constant")
                        .with_code("E500"),
                );
                break;
            }
            e.bytecode.push(0x00); // ret (entrypoint, void)
            if e.diags.iter().any(|d| d.is_error()) {
                break;
            }
            if !nara_resolve_jumps(&mut e) {
                break;
            }
            functions_out.push((entry_name_idx, std::mem::take(&mut e.bytecode)));
            // Now emit the separate `main` body (no init) for calls.
            e.reset_fn(nara_last_use(f));
            let ctx = NaraFnCtx {
                func: f,
                is_main: false,
                sigs: &sigs,
                fn_consts: &fn_consts,
                imported_fn_consts: &imported_fn_consts,
                imports: &imports,
                module: &prog.module,
                objects: &objects,
                print_fn_idx,
                print_u64_fn_idx,
                global_slots: &global_slots,
                global_tys: &global_tys,
                err_lanes: prog.err_lanes,
            };
            for (idx, ins) in f.instrs.iter().enumerate() {
                nara_instr(&mut e, ins, &ctx);
                nara_free_unused_result(&mut e, ins);
                nara_free_dead(&mut e, ins, idx);
                if e.diags.iter().any(|d| d.is_error()) {
                    break;
                }
            }
            if e.diags.iter().any(|d| d.is_error()) {
                break;
            }
            if !nara_resolve_jumps(&mut e) {
                break;
            }
            let main_idx = fn_names.get("main").copied().unwrap_or(entry_name_idx);
            functions_out.push((main_idx, std::mem::take(&mut e.bytecode)));
            continue;
        }
        for (idx, ins) in f.instrs.iter().enumerate() {
            nara_instr(&mut e, ins, &ctx);
            nara_free_unused_result(&mut e, ins);
            nara_free_dead(&mut e, ins, idx);
            if e.diags.iter().any(|d| d.is_error()) {
                break;
            }
        }
        if e.diags.iter().any(|d| d.is_error()) {
            break;
        }
        if !nara_resolve_jumps(&mut e) {
            break;
        }
        let name_idx = if is_main && has_entrypoint {
            entry_name_idx
        } else {
            fn_names.get(&f.name).copied().unwrap_or(entry_name_idx)
        };
        functions_out.push((name_idx, std::mem::take(&mut e.bytecode)));
    }
    // Libraries retain their initializer in a named ordinary function. It is
    // deliberately not `<entrypoint>`: Naravm allows only one entrypoint when
    // loading multiple modules, while a future loader can invoke this
    // metadata function before exposing the library's other functions.
    if !has_entrypoint && !prog.globals.is_empty() {
        e.reset_fn(std::collections::HashMap::new());
        // Stub function for init-only emission when no user function exists.
        let stub;
        let first: &vl_lir::Function = match prog.functions.first() {
            Some(f) => f,
            None => {
                stub = vl_lir::Function {
                    name: "<entrypoint>".into(),
                    param_tys: vec![],
                    ret: vl_typecheck::Ty::Void,
                    instrs: vec![],
                };
                &stub
            }
        };
        {
            let ctx = NaraFnCtx {
                func: first,
                is_main: false,
                sigs: &sigs,
                fn_consts: &fn_consts,
                imported_fn_consts: &imported_fn_consts,
                imports: &imports,
                module: &prog.module,
                objects: &objects,
                print_fn_idx,
                print_u64_fn_idx,
                global_slots: &global_slots,
                global_tys: &global_tys,
                err_lanes: prog.err_lanes,
            };
            e.bytecode
                .extend_from_slice(&[0x27, MODULE_STATE_RF, value_count, ref_count]);
            for (gi, g) in prog.globals.iter().enumerate() {
                let Some((is_ref, slot)) = global_slots.get(&g.id).copied() else {
                    continue;
                };
                if g.init.is_empty() {
                    continue;
                }
                let base = ((gi as u32) + 1) * 1_000_000;
                let frag_last = nara_last_use_fragment(&g.init, &g.result);
                let saved_last = std::mem::replace(&mut e.last_use, frag_last);
                for (frag_idx, ins) in g.init.iter().enumerate() {
                    let remapped = remap_labels(ins, base);
                    nara_instr(&mut e, &remapped, &ctx);
                    nara_free_unused_result(&mut e, &remapped);
                    nara_free_dead(&mut e, &remapped, frag_idx);
                    if e.diags.iter().any(|d| d.is_error()) {
                        break;
                    }
                }
                e.last_use = saved_last;
                if e.diags.iter().any(|d| d.is_error()) {
                    break;
                }
                if is_ref {
                    if let Some(rf) = e.rf_map.get(&g.result).copied() {
                        e.bytecode
                            .extend_from_slice(&[0x2f, MODULE_STATE_RF, slot, rf]);
                    }
                } else if let Some(rv) = e.rv_map.get(&g.result).copied() {
                    e.bytecode
                        .extend_from_slice(&[0x2d, MODULE_STATE_RF, slot, rv]);
                }
                free_fragment_regs(&mut e, &g.init, &g.result);
            }
            // Void return for the module initializer (bare `ret`).
            e.bytecode.push(0x00);
            if !e.diags.iter().any(|d| d.is_error()) && nara_resolve_jumps(&mut e) {
                if let Some(name_idx) = module_init_name_idx {
                    functions_out.push((name_idx, std::mem::take(&mut e.bytecode)));
                }
            }
        }
    }
    if e.diags.iter().any(|d| d.is_error()) {
        diags.append(&mut e.diags);
        return None;
    }
    Some(serialize_nara(
        &e.values,
        &e.blob,
        &e.constants,
        &functions_out,
        module_idx,
    ))
}

/// Push a `Function{module, function}` constant with the shared pool-limit
/// diagnostic (E405) instead of silently overflowing the 16-bit index space.
fn nara_push_fn_const(e: &mut NaraEmit, module: usize, function: usize) -> Option<usize> {
    let idx = e.constants.len();
    e.constants
        .push(NaraConstant::Function { module, function });
    if idx > u16::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm constant pool has more than 65536 entries")
                .with_label(Span::empty(0), "defined here")
                .with_note("function references use a 16-bit constant index")
                .with_code("E405"),
        );
        return None;
    }
    Some(idx)
}

/// Resolve jump targets for the function in `e.bytecode`. Offsets are
/// relative to the end of their own instruction (matches the VM's `rip`
/// after reading the offset).
fn nara_resolve_jumps(e: &mut NaraEmit) -> bool {
    for patch in &e.patches {
        let Some(target_pos) = e.label_pos.get(&patch.target).copied() else {
            e.diags.push(
                Diagnostic::error(format!(
                    "codegen: unknown label L{} (compiler bug)",
                    patch.target
                ))
                .with_label(patch.span, "jump emitted here")
                .with_code("E500"),
            );
            continue;
        };
        let offset = target_pos as isize - (patch.pos + patch.len) as isize;
        let Ok(offset) = i16::try_from(offset) else {
            e.diags.push(
                Diagnostic::error("Naravm jump offset out of range (function too large)")
                    .with_label(patch.span, "jump emitted here")
                    .with_code("E500"),
            );
            continue;
        };
        let bytes = offset.to_be_bytes();
        e.bytecode[patch.pos + patch.len - 2] = bytes[0];
        e.bytecode[patch.pos + patch.len - 1] = bytes[1];
    }
    !e.diags.iter().any(|d| d.is_error())
}

/// Scan one function for the last use of every LIR register. Uses are
/// operand positions (BinOp sides, copy sources, call args, branch
/// conditions, return values); definitions do not count.
///
/// A purely textual scan is unsound for loops: a register last used *inside*
/// a loop body re-executes that use on every back edge, so freeing its
/// machine register mid-loop lets a later temporary clobber a still-live
/// value (e.g. a loop-invariant parameter). Backward jumps extend values
/// defined before the loop; temporaries defined inside it are recreated on
/// each iteration and can die at their last use.
fn nara_last_use(func: &vl_lir::Function) -> std::collections::HashMap<vl_lir::Reg, usize> {
    use vl_lir::Instr as I;
    let mut uses: std::collections::HashMap<vl_lir::Reg, Vec<usize>> =
        std::collections::HashMap::new();
    let mut touch = |reg: vl_lir::Reg, idx: usize| {
        uses.entry(reg).or_default().push(idx);
    };
    let mut label_pos: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    let mut first_def = std::collections::HashMap::new();
    for (idx, ins) in func.instrs.iter().enumerate() {
        let dst = match ins {
            I::Const { dst, .. }
            | I::StringConst { dst, .. }
            | I::Param { dst, .. }
            | I::Copy { dst, .. }
            | I::Cast { dst, .. }
            | I::Not { dst, .. }
            | I::BinOp { dst, .. }
            | I::Call { dst, .. }
            | I::NewArray { dst, .. }
            | I::ArrayLit { dst, .. }
            | I::ArrayGet { dst, .. }
            | I::ArrayLen { dst, .. }
            | I::NewObject { dst, .. }
            | I::ObjectGet { dst, .. }
            | I::TupleLit { dst, .. }
            | I::TupleGet { dst, .. }
            | I::NewVariant { dst, .. }
            | I::TagOf { dst, .. }
            | I::PayloadGet { dst, .. }
            | I::WrapOk { dst, .. }
            | I::WrapErr { dst, .. }
            | I::RewrapErr { dst, .. }
            | I::UnwrapOk { dst, .. }
            | I::UnwrapErr { dst, .. }
            | I::ErrPayloadGet { dst, .. }
            | I::GlobalLoad { dst, .. } => Some(*dst),
            _ => None,
        };
        if let Some(dst) = dst {
            first_def.entry(dst).or_insert(idx);
        }
        if let I::Label { id, .. } = ins {
            label_pos.insert(*id, idx);
        }
        match ins {
            I::BinOp { lhs, rhs, .. } => {
                touch(*lhs, idx);
                touch(*rhs, idx);
            }
            I::Copy { src, .. } => {
                touch(*src, idx);
            }
            I::Cast { src, .. } => {
                touch(*src, idx);
            }
            I::Not { src, .. } => {
                touch(*src, idx);
            }
            I::Call { args, .. } => {
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::NewArray { len, .. } => {
                touch(*len, idx);
            }
            I::ArrayLit { elems, .. } => {
                for elem in elems {
                    touch(*elem, idx);
                }
            }
            I::TupleLit { elems, .. } => {
                for elem in elems {
                    touch(*elem, idx);
                }
            }
            I::TupleGet { tuple, .. } => {
                touch(*tuple, idx);
            }
            I::TupleSet { tuple, value, .. } => {
                touch(*tuple, idx);
                touch(*value, idx);
            }
            I::ArrayGet { array, index, .. } => {
                touch(*array, idx);
                touch(*index, idx);
            }
            I::ArrayLen { array, .. } => {
                touch(*array, idx);
            }
            I::ArraySet {
                array,
                index,
                value,
                ..
            } => {
                touch(*array, idx);
                touch(*index, idx);
                touch(*value, idx);
            }
            I::NewObject { fields, .. } => {
                for (_, value) in fields {
                    touch(*value, idx);
                }
            }
            I::NewVariant { args, .. } => {
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::WrapOk { value, .. } => touch(*value, idx),
            I::WrapErr { code, args, .. } => {
                touch(*code, idx);
                for arg in args {
                    touch(*arg, idx);
                }
            }
            I::RewrapErr { scrut, .. } => touch(*scrut, idx),
            I::UnwrapOk { scrut, .. } | I::UnwrapErr { scrut, .. } => touch(*scrut, idx),
            I::TagOf { scrut, .. } => touch(*scrut, idx),
            I::PayloadGet { scrut, .. } | I::ErrPayloadGet { scrut, .. } => touch(*scrut, idx),
            I::ObjectGet { object, .. } => touch(*object, idx),
            I::ObjectSet { object, value, .. } => {
                touch(*object, idx);
                touch(*value, idx);
            }
            I::BranchIfFalse { cond, .. } => {
                touch(*cond, idx);
            }
            I::Ret { src, .. } => {
                touch(*src, idx);
            }
            I::GlobalStore { src, .. } => {
                touch(*src, idx);
            }
            I::Const { .. }
            | I::StringConst { .. }
            | I::Param { .. }
            | I::GlobalLoad { .. }
            | I::Jump { .. }
            | I::Label { .. } => {}
        }
    }
    // Values defined before a loop and used inside it must survive its back
    // edge. Keeping loop-local temporaries live too needlessly exhausts the
    // machine registers in parsers and other arithmetic-heavy loops.
    let mut loops: Vec<(usize, usize)> = Vec::new();
    for (idx, ins) in func.instrs.iter().enumerate() {
        let target = match ins {
            I::Jump { target, .. } | I::BranchIfFalse { target, .. } => target,
            _ => continue,
        };
        if let Some(header) = label_pos.get(target).copied() {
            if header < idx {
                loops.push((header, idx));
            }
        }
    }
    let mut last = std::collections::HashMap::new();
    for (reg, idxs) in uses {
        let mut end = idxs.iter().copied().max().unwrap_or(0);
        for (header, jump) in &loops {
            if first_def.get(&reg).is_some_and(|def| *def < *header)
                && idxs.iter().any(|u| *header <= *u && *u <= *jump)
            {
                end = end.max(*jump);
            }
        }
        last.insert(reg, end);
    }
    last
}

/// Recycle machine registers whose LIR value dies at `idx`. `Copy`
/// destinations stay mapped (variable homes and `&&`/`||` join registers
/// remain live); internal one/bias registers are not LIR regs and are never
/// freed here.
fn nara_free_dead(e: &mut NaraEmit, ins: &Instr, idx: usize) {
    use vl_lir::Instr as I;
    let mut dead = Vec::new();
    match ins {
        I::BinOp { lhs, rhs, .. } => {
            dead.push(*lhs);
            dead.push(*rhs);
        }
        I::Copy { dst, src, .. } => {
            if dst != src {
                dead.push(*src);
            }
        }
        I::Cast { src, .. } => {
            dead.push(*src);
        }
        I::Not { src, .. } => {
            dead.push(*src);
        }
        I::Call { args, .. } => {
            dead.extend(args.iter().copied());
        }
        I::NewArray { len, .. } => {
            dead.push(*len);
        }
        I::ArrayLit { elems, .. } => {
            dead.extend(elems.iter().copied());
        }
        I::TupleLit { elems, .. } => {
            dead.extend(elems.iter().copied());
        }
        I::TupleGet { tuple, .. } => {
            dead.push(*tuple);
        }
        I::TupleSet { tuple, value, .. } => {
            dead.push(*tuple);
            dead.push(*value);
        }
        I::ArrayGet { array, index, .. } => {
            dead.push(*array);
            dead.push(*index);
        }
        I::ArrayLen { array, .. } => dead.push(*array),
        I::ArraySet {
            array,
            index,
            value,
            ..
        } => {
            dead.push(*array);
            dead.push(*index);
            dead.push(*value);
        }
        I::NewObject { fields, .. } => {
            dead.extend(fields.iter().map(|(_, value)| *value));
        }
        I::ObjectGet { object, .. } => dead.push(*object),
        I::ObjectSet { object, value, .. } => {
            dead.push(*object);
            dead.push(*value);
        }
        I::NewVariant { args, .. } => {
            dead.extend(args.iter().copied());
        }
        I::WrapOk { value, .. } => {
            dead.push(*value);
        }
        I::WrapErr { code, args, .. } => {
            dead.push(*code);
            dead.extend(args.iter().copied());
        }
        I::RewrapErr { scrut, .. } => {
            dead.push(*scrut);
        }
        I::UnwrapOk { scrut, .. } | I::UnwrapErr { scrut, .. } => {
            dead.push(*scrut);
        }
        I::TagOf { scrut, .. } => {
            dead.push(*scrut);
        }
        I::PayloadGet { scrut, .. } | I::ErrPayloadGet { scrut, .. } => {
            dead.push(*scrut);
        }
        I::BranchIfFalse { cond, .. } => {
            dead.push(*cond);
        }
        I::Ret { src, .. } => {
            dead.push(*src);
        }
        I::GlobalStore { src, .. } => {
            dead.push(*src);
        }
        I::Const { .. }
        | I::StringConst { .. }
        | I::Param { .. }
        | I::GlobalLoad { .. }
        | I::Jump { .. }
        | I::Label { .. } => {}
    }
    // A `Copy` destination is (re)defined here: its home register stays live
    // even when this is also its last textual use as a source elsewhere.
    let copy_dst = match ins {
        I::Copy { dst, .. } => Some(*dst),
        _ => None,
    };
    for reg in dead {
        if Some(reg) == copy_dst {
            continue;
        }
        if e.last_use.get(&reg).copied() != Some(idx) {
            continue;
        }
        if let Some(rv) = e.rv_map.remove(&reg) {
            e.free_rv.push(rv);
        }
        if let Some(rf) = e.rf_map.remove(&reg) {
            e.free_rf.push(rf);
        }
    }
}

/// Recycle a result register when its value is never used. The instruction
/// itself has already run, preserving traps and side effects.
fn nara_free_unused_result(e: &mut NaraEmit, ins: &Instr) {
    let dst = match ins {
        Instr::Const { dst, .. }
        | Instr::StringConst { dst, .. }
        | Instr::Param { dst, .. }
        | Instr::GlobalLoad { dst, .. }
        | Instr::Copy { dst, .. }
        | Instr::Cast { dst, .. }
        | Instr::Not { dst, .. }
        | Instr::BinOp { dst, .. }
        | Instr::Call { dst, .. }
        | Instr::NewArray { dst, .. }
        | Instr::ArrayLit { dst, .. }
        | Instr::ArrayLen { dst, .. }
        | Instr::ArrayGet { dst, .. }
        | Instr::TupleLit { dst, .. }
        | Instr::TupleGet { dst, .. }
        | Instr::NewObject { dst, .. }
        | Instr::ObjectGet { dst, .. }
        | Instr::NewVariant { dst, .. }
        | Instr::WrapOk { dst, .. }
        | Instr::WrapErr { dst, .. }
        | Instr::RewrapErr { dst, .. }
        | Instr::UnwrapOk { dst, .. }
        | Instr::UnwrapErr { dst, .. }
        | Instr::TagOf { dst, .. }
        | Instr::PayloadGet { dst, .. }
        | Instr::ErrPayloadGet { dst, .. } => *dst,
        _ => return,
    };
    if e.last_use.contains_key(&dst) {
        return;
    }
    if let Some(rv) = e.rv_map.remove(&dst) {
        e.free_rv.push(rv);
    }
    if let Some(rf) = e.rf_map.remove(&dst) {
        e.free_rf.push(rf);
    }
}

fn nara_instr(e: &mut NaraEmit, ins: &Instr, ctx: &NaraFnCtx) {
    match ins {
        Instr::Const { dst, value, span } => {
            let kind = NaraKind::of_scalar(*value);
            let bits = NaraKind::scalar_bits(*value);
            if !e.last_use.contains_key(dst) {
                // Dead definition (e.g. an unused binding): still intern the
                // value so pool-limit diagnostics stay accurate, but spend no
                // machine register on it.
                e.add_value(bits, *span);
                e.kinds.insert(*dst, kind);
                return;
            }
            let Some(idx) = e.add_value(bits, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(rv) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rv_map.insert(*dst, rv);
            e.kinds.insert(*dst, kind);
            e.load_constant(false, rv, idx);
        }
        Instr::StringConst { dst, value, span } => {
            if !e.last_use.contains_key(dst) {
                e.add_string(value, *span);
                e.kinds.insert(*dst, NaraKind::String);
                return;
            }
            let Some(idx) = e.add_string(value, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, NaraKind::String);
            e.load_constant(true, rf, idx);
        }
        Instr::Param { dst, index, span } => {
            if ctx.is_main {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend only supports `fun main()` with no parameters",
                    )
                    .with_label(*span, "parameter here")
                    .with_code("E403"),
                );
                return;
            }
            nara_param(e, ctx, *dst, *index, *span);
        }
        Instr::Copy { dst, src, span } => {
            if e.invalid.contains(src) {
                e.invalid.insert(*dst);
                return;
            }
            let Some(kind) = e.kinds.get(src).cloned() else {
                e.invalid.insert(*dst);
                return;
            };
            // Dead home (never read later, e.g. an unused binding): no machine
            // register or bytecode, like dead `Const`/`ArrayGet`.
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, kind);
                return;
            }
            if e.rv_map
                .get(dst)
                .copied()
                .is_some_and(|d| Some(d) == e.rv_map.get(src).copied())
                || e.rf_map
                    .get(dst)
                    .copied()
                    .is_some_and(|d| Some(d) == e.rf_map.get(src).copied())
            {
                e.kinds.insert(*dst, kind);
                return;
            }
            // Tuples copy by value: duplicate the container so later
            // element writes through one binding never affect the other.
            if let NaraKind::Tuple(kinds) = &kind {
                let kinds = kinds.clone();
                let (Some(s), Some(d)) = (
                    e.rf_map.get(src).copied(),
                    e.rf_map.get(dst).copied().or_else(|| {
                        let rf = e.fresh_rf(*span)?;
                        e.rf_map.insert(*dst, rf);
                        Some(rf)
                    }),
                ) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.kinds.insert(*dst, kind);
                if !nara_tuple_copy_into(e, d, s, &kinds, *span) {
                    e.invalid.insert(*dst);
                }
                return;
            }
            if kind.is_ref() {
                let (Some(s), Some(d)) = (
                    e.rf_map.get(src).copied(),
                    e.rf_map.get(dst).copied().or_else(|| {
                        let rf = e.fresh_rf(*span)?;
                        e.rf_map.insert(*dst, rf);
                        Some(rf)
                    }),
                ) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.kinds.insert(*dst, kind);
                e.bytecode.extend_from_slice(&[0x05, d, s]); // cprf
            } else {
                let (Some(s), Some(d)) = (
                    e.rv_map.get(src).copied(),
                    e.rv_map.get(dst).copied().or_else(|| {
                        let rv = e.fresh_rv(*span)?;
                        e.rv_map.insert(*dst, rv);
                        Some(rv)
                    }),
                ) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.kinds.insert(*dst, kind);
                e.bytecode.extend_from_slice(&[0x04, d, s]); // cpv
            }
        }
        Instr::Cast {
            dst,
            src,
            target,
            span,
        } => {
            // Explicit integer conversion: unchecked reinterpretation (no
            // trap, no wrap op). Literals were range-checked by typechecking;
            // variables keep their 64-bit payload and change lane.
            if e.invalid.contains(src) {
                e.invalid.insert(*dst);
                return;
            }
            let Some(target_kind) = NaraKind::of_ty(target) else {
                e.invalid.insert(*dst);
                return;
            };
            if target_kind.is_ref() {
                e.diags.push(
                    Diagnostic::error("Naravm backend does not support casts to reference types")
                        .with_label(*span, "unsupported cast")
                        .with_code("E404"),
                );
                e.invalid.insert(*dst);
                return;
            }
            let Some(s) = e.value_reg(*src, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(d) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rv_map.insert(*dst, d);
            e.kinds.insert(*dst, target_kind);
            if s != d {
                e.bytecode.extend_from_slice(&[0x04, d, s]); // cpv
            }
        }
        Instr::Not { dst, src, span } => {
            let Some(s) = e.value_reg(*src, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(one) = e.ensure_one(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(d) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rv_map.insert(*dst, d);
            e.kinds.insert(*dst, NaraKind::Bool);
            e.bytecode.extend_from_slice(&[0x12, d, s, one]); // xor
        }
        Instr::BinOp {
            dst,
            op,
            lhs,
            rhs,
            span,
        } => {
            if matches!(op, LirOp::Eq | LirOp::Ne) && e.kinds.get(lhs) == Some(&NaraKind::String) {
                nara_string_eq(e, ctx, *dst, *lhs, *rhs, *span);
                if *op == LirOp::Ne {
                    if let (Some(result), Some(one)) =
                        (e.rv_map.get(dst).copied(), e.ensure_one(*span))
                    {
                        e.bytecode.extend_from_slice(&[0x12, result, result, one]);
                    }
                }
            } else {
                nara_binop(e, *dst, *op, *lhs, *rhs, *span);
            }
        }
        Instr::Call {
            dst,
            callee,
            args,
            span,
        } => {
            // Checked TCP natives first: like `std.print`, they bypass the
            // single-result user-call path (their results live in several
            // registers behind an `rv10` status check).
            if callee.module == "std.net.tcp"
                && matches!(
                    callee.function.as_str(),
                    "connect" | "listen" | "accept" | "read" | "write" | "close"
                )
            {
                nara_tcp_call(e, ctx, *dst, callee, args, *span);
                return;
            }
            // Checked single-result natives (`std.string` bounds-checked
            // reads, `std.fs` file reads): same `rv10` treatment through
            // the shared emitter.
            if (callee.module == "std.string"
                && matches!(callee.function.as_str(), "byte_at" | "slice"))
                || (callee.module == "std.fs" && callee.function.as_str() == "read_file")
            {
                nara_checked_call(e, ctx, *dst, callee, args, *span);
                return;
            }
            // User functions first: a user function may share a bare name
            // with a std export, and the LIR callee spelling alone cannot
            // tell them apart (imports are resolved away before lowering).
            let target_name = callee
                .function
                .strip_prefix("std.")
                .unwrap_or(&callee.function);
            if ((callee.module == ctx.module && ctx.sigs.contains_key(callee.function.as_str()))
                || ctx.imports.contains_key(callee))
                && !(callee.module == "std"
                    && matches!(target_name, "print" | "println" | "print_u64"))
            {
                nara_user_call(e, ctx, *dst, callee, args, *span);
            } else if callee.module == "std" && target_name == "print" {
                if args.len() != 1 {
                    e.diags.push(
                        Diagnostic::error("std.print expects one String argument")
                            .with_label(*span, "invalid call")
                            .with_code("E401"),
                    );
                    e.invalid.insert(*dst);
                    return;
                }
                if e.invalid.contains(&args[0]) {
                    e.invalid.insert(*dst);
                    return;
                }
                let Some(s) = e.ref_reg(args[0], *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x05, 0x31, s]); // cprf rf31, src
                nara_calli(e, ctx.print_fn_idx, *span);
            } else if callee.module == "std" && target_name == "println" {
                if args.len() != 1 {
                    e.diags.push(
                        Diagnostic::error("std.println expects one String argument")
                            .with_label(*span, "invalid call")
                            .with_code("E401"),
                    );
                    e.invalid.insert(*dst);
                    return;
                }
                if e.invalid.contains(&args[0]) {
                    e.invalid.insert(*dst);
                    return;
                }
                let Some(s) = e.ref_reg(args[0], *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x05, 0x31, s]); // cprf rf31, src
                nara_calli(e, ctx.print_fn_idx, *span);
                // `println` is `print` plus a trailing newline. The VM has no
                // native newline call, so lower it to a second `print("\n")`.
                let (Some(idx), Some(nl)) = (e.add_string(b"\n", *span), e.fresh_rf(*span)) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.load_constant(true, nl, idx);
                e.bytecode.extend_from_slice(&[0x05, 0x31, nl]); // cprf rf31, nl
                nara_calli(e, ctx.print_fn_idx, *span);
                // The newline register is a backend temporary, not a LIR
                // value, so recycle it immediately for the next call.
                e.free_rf.push(nl);
            } else if callee.module == "std" && target_name == "print_u64" {
                if args.len() != 1 {
                    e.diags.push(
                        Diagnostic::error("std.print_u64 expects one u64 argument")
                            .with_label(*span, "invalid call")
                            .with_code("E401"),
                    );
                    e.invalid.insert(*dst);
                    return;
                }
                if e.invalid.contains(&args[0]) {
                    e.invalid.insert(*dst);
                    return;
                }
                match e.kinds.get(&args[0]).cloned() {
                    Some(NaraKind::U64) | Some(NaraKind::U8) => {}
                    _ => {
                        e.diags.push(
                            Diagnostic::error(
                                "Naravm backend requires a u64 argument to std.print_u64",
                            )
                            .with_label(*span, "unsupported argument")
                            .with_code("E402"),
                        );
                        e.invalid.insert(*dst);
                        return;
                    }
                }
                let Some(s) = e.value_reg(args[0], *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                if s != 0x11 {
                    e.bytecode.extend_from_slice(&[0x04, 0x11, s]); // cpv rv11, src
                }
                nara_calli(e, ctx.print_u64_fn_idx, *span);
            } else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "Naravm backend does not support call `{callee}` yet"
                    ))
                    .with_label(*span, "unsupported call")
                    .with_note(
                        "only `std.print`, `std.println`, `std.print_u64`, the `std.string`/`std.math`/`std.fmt`/`std.fs`/`std.net.tcp` natives, and user functions lower to Naravm calls",
                    )
                    .with_code("E404"),
                );
                e.invalid.insert(*dst);
            }
        }
        Instr::NewArray {
            dst,
            len,
            elem,
            span,
        } => {
            let Some(elem_kind) = NaraKind::of_ty(elem) else {
                e.invalid.insert(*dst);
                return;
            };
            let array_kind = NaraKind::Array(Box::new(elem_kind.clone()));
            if !e.last_use.contains_key(dst) {
                // Dead allocation (e.g. an unused binding): spend no machine
                // register on it.
                e.kinds.insert(*dst, array_kind);
                return;
            }
            if e.invalid.contains(len) {
                e.invalid.insert(*dst);
                return;
            }
            let Some(n) = e.value_reg(*len, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let one = e.ensure_one(*span);
            let value_count = if elem_kind.is_ref() {
                one
            } else {
                let (Some(one), Some(value_count)) = (one, e.fresh_rv(*span)) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x30, value_count, n, one]); // add_u64 len, 1
                let Some(wrapped) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode
                    .extend_from_slice(&[0x10, wrapped, value_count, n]); // ltu detects overflow
                e.bytecode.extend_from_slice(&[0x24, wrapped, 0, 3]); // jz over fallback copy
                e.bytecode.extend_from_slice(&[0x04, value_count, n]); // on overflow, request n (OOM)
                e.free_rv.push(wrapped);
                Some(value_count)
            };
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, array_kind);
            // Value slot 0 stores the logical length. Scalar elements begin
            // at value slot 1; reference elements retain their own zero-based
            // lane, so they need one value slot for the header.
            if elem_kind.is_ref() {
                let Some(values) = value_count else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x26, rf, values, n]); // create
            } else {
                let (Some(values), Some(zero)) = (value_count, e.ensure_zero(*span)) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x26, rf, values, zero]); // create
            }
            e.bytecode.extend_from_slice(&[0x2d, rf, 0, n]); // setvati length header
            if !elem_kind.is_ref() {
                if let Some(value_count) = value_count {
                    e.free_rv.push(value_count);
                }
            }
        }
        Instr::ArrayLit {
            dst,
            elems,
            elem,
            span,
        } => {
            let Some(elem_kind) = NaraKind::of_ty(elem) else {
                e.invalid.insert(*dst);
                return;
            };
            let array_kind = NaraKind::Array(Box::new(elem_kind.clone()));
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, array_kind);
                return;
            }
            for elem in elems {
                if e.invalid.contains(elem) {
                    e.invalid.insert(*dst);
                    return;
                }
            }
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            let is_ref = elem_kind.is_ref();
            // One scratch value register covers the dynamic length and every
            // dynamic index of oversized literals (> 255 elements); the
            // common path stays on the immediate `createi`/`setvati` forms.
            let scalar_count = elems.len().checked_add(1);
            let lane_count = if is_ref {
                Some(elems.len())
            } else {
                scalar_count
            };
            let count_is_immediate = lane_count.is_some_and(|n| n <= u8::MAX as usize);
            let scratch = if !count_is_immediate {
                let count = if is_ref {
                    elems.len()
                } else {
                    scalar_count.unwrap_or(usize::MAX)
                };
                let (Some(s), Some(len_idx), Some(zero)) = (
                    e.fresh_rv(*span),
                    e.add_value(count as u64, *span),
                    e.ensure_zero(*span),
                ) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.load_constant(false, s, len_idx);
                let (Some(value_count), Some(len)) =
                    (e.ensure_one(*span), e.add_value(elems.len() as u64, *span))
                else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Some(logical_len) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.load_constant(false, logical_len, len); // lv logical length
                if is_ref {
                    e.bytecode.extend_from_slice(&[0x26, rf, value_count, s]); // create
                } else {
                    e.bytecode.extend_from_slice(&[0x26, rf, s, zero]); // create
                }
                e.bytecode.extend_from_slice(&[0x2d, rf, 0, logical_len]); // set length header
                e.free_rv.push(logical_len);
                Some(s)
            } else if is_ref {
                e.bytecode
                    .extend_from_slice(&[0x27, rf, 1, elems.len() as u8]); // createi
                let Some(len_idx) = e.add_value(elems.len() as u64, *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Some(length) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.load_constant(false, length, len_idx); // lv length
                e.bytecode.extend_from_slice(&[0x2d, rf, 0, length]); // set length header
                e.free_rv.push(length);
                None
            } else {
                e.bytecode.extend_from_slice(&[
                    0x27,
                    rf,
                    scalar_count.expect("immediate count") as u8,
                    0,
                ]); // createi
                let Some(len_idx) = e.add_value(elems.len() as u64, *span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Some(length) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.load_constant(false, length, len_idx); // lv length
                e.bytecode.extend_from_slice(&[0x2d, rf, 0, length]); // set length header
                e.free_rv.push(length);
                None
            };
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, array_kind);
            // Tuple elements are value types: duplicate each element
            // container so the array owns its copies (other references share).
            let tuple_elem = match &elem_kind {
                NaraKind::Tuple(kinds) => Some(kinds.clone()),
                _ => None,
            };
            for (i, elem) in elems.iter().enumerate() {
                if is_ref {
                    let Some(v) = e.ref_reg(*elem, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    // Tuple elements copy into a temp owned by the array.
                    let (v, owned) = if let Some(nested) = &tuple_elem {
                        let Some(tmp) = e.fresh_rf(*span) else {
                            e.invalid.insert(*dst);
                            return;
                        };
                        if !nara_tuple_copy_into(e, tmp, v, nested, *span) {
                            e.free_rf.push(tmp);
                            e.invalid.insert(*dst);
                            return;
                        }
                        (tmp, true)
                    } else {
                        (v, false)
                    };
                    if i <= u8::MAX as usize {
                        e.bytecode.extend_from_slice(&[0x2f, rf, i as u8, v]); // setrfati
                    } else {
                        let s = scratch.expect("scratch exists when a literal index exceeds u8");
                        let Some(idx) = e.add_value(i as u64, *span) else {
                            e.invalid.insert(*dst);
                            return;
                        };
                        e.load_constant(false, s, idx);
                        e.bytecode.extend_from_slice(&[0x2b, rf, s, v]); // setrfat
                    }
                    // The array now references the copy; recycle the temp.
                    if owned {
                        e.free_rf.push(v);
                    }
                } else {
                    let Some(v) = e.value_reg(*elem, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    if i < u8::MAX as usize {
                        e.bytecode.extend_from_slice(&[0x2d, rf, (i + 1) as u8, v]);
                    // setvati
                    } else {
                        let s = scratch.expect("scratch exists when a literal index exceeds u8");
                        let Some(idx) = e.add_value(i as u64 + 1, *span) else {
                            e.invalid.insert(*dst);
                            return;
                        };
                        e.load_constant(false, s, idx);
                        e.bytecode.extend_from_slice(&[0x29, rf, s, v]); // setvat
                    }
                }
            }
            if let Some(scratch) = scratch {
                e.free_rv.push(scratch);
            }
        }
        Instr::NewObject {
            dst,
            name,
            fields,
            span,
        } => {
            let object_kind = NaraKind::Object(name.clone());
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, object_kind);
                return;
            }
            for (_, value) in fields {
                if e.invalid.contains(value) {
                    e.invalid.insert(*dst);
                    return;
                }
            }
            let Some(def) = ctx.objects.get(name.as_str()).copied() else {
                e.invalid.insert(*dst);
                e.diags.push(
                    Diagnostic::error(format!("unknown object layout `{name}` (compiler bug)"))
                        .with_label(*span, "object emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let mut value_count = 0usize;
            let mut ref_count = 0usize;
            for (_, ty) in &def.fields {
                match NaraKind::of_ty(ty) {
                    Some(kind) if kind.is_ref() => ref_count += 1,
                    Some(_) => value_count += 1,
                    None => {
                        e.invalid.insert(*dst);
                        return;
                    }
                }
            }
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            if value_count > u8::MAX as usize || ref_count > u8::MAX as usize {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm object has more than 255 fields in one register lane",
                    )
                    .with_label(*span, "object allocated here")
                    .with_note("split the object into smaller objects")
                    .with_code("E404"),
                );
                e.invalid.insert(*dst);
                return;
            }
            e.bytecode
                .extend_from_slice(&[0x27, rf, value_count as u8, ref_count as u8]);
            for (field, ty) in &def.fields {
                let Some((_, value)) = fields.iter().find(|(name, _)| name == field) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Some((is_ref, slot, _)) = object_slot(def, field) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Some(slot) = u8::try_from(slot).ok() else {
                    e.invalid.insert(*dst);
                    return;
                };
                if is_ref {
                    let Some(src) = e.ref_reg(*value, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    if let Some(NaraKind::Tuple(kinds)) = e.kinds.get(value).cloned() {
                        let Some(copy) = e.fresh_rf(*span) else {
                            e.invalid.insert(*dst);
                            return;
                        };
                        if !nara_tuple_copy_into(e, copy, src, &kinds, *span) {
                            e.invalid.insert(*dst);
                            return;
                        }
                        e.bytecode.extend_from_slice(&[0x2f, rf, slot, copy]);
                        e.free_rf.push(copy);
                    } else {
                        e.bytecode.extend_from_slice(&[0x2f, rf, slot, src]);
                    }
                } else {
                    let Some(src) = e.value_reg(*value, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    e.bytecode.extend_from_slice(&[0x2d, rf, slot, src]);
                }
                let _ = ty;
            }
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, object_kind);
        }
        Instr::ObjectGet {
            dst,
            object,
            name,
            ty,
            span,
        } => {
            let Some(NaraKind::Object(object_name)) = e.kinds.get(object).cloned() else {
                if !e.invalid.contains(object) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected an object reference")
                            .with_label(*span, "field read emitted here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            let Some(def) = ctx.objects.get(object_name.as_str()).copied() else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "unknown object layout `{object_name}` (compiler bug)"
                    ))
                    .with_label(*span, "field read emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some((is_ref, slot, field_ty)) = object_slot(def, name) else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "object `{object_name}` has no usable field `{name}` (compiler bug)"
                    ))
                    .with_label(*span, "field read emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(obj) = e.rf_map.get(object).copied() else {
                e.diags.push(
                    Diagnostic::error("Naravm backend could not resolve an object reference")
                        .with_label(*span, "field read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(kind) = NaraKind::of_ty(ty).or_else(|| NaraKind::of_ty(field_ty)) else {
                e.diags.push(
                    Diagnostic::error("Naravm backend could not resolve an object field type")
                        .with_label(*span, "field read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(slot) = u8::try_from(slot).ok() else {
                e.diags.push(
                    Diagnostic::error("Naravm object field slot is out of range (compiler bug)")
                        .with_label(*span, "field read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, kind);
                return;
            }
            if is_ref {
                let Some(d) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2e, d, obj, slot]);
                e.rf_map.insert(*dst, d);
            } else {
                let Some(d) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2c, d, obj, slot]);
                e.rv_map.insert(*dst, d);
            }
            e.kinds.insert(*dst, kind);
        }
        Instr::ObjectSet {
            object,
            name,
            value,
            ty,
            span,
        } => {
            if e.invalid.contains(object) || e.invalid.contains(value) {
                return;
            }
            let Some(NaraKind::Object(object_name)) = e.kinds.get(object).cloned() else {
                e.diags.push(
                    Diagnostic::error("Naravm backend expected an object reference")
                        .with_label(*span, "field write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let Some(def) = ctx.objects.get(object_name.as_str()).copied() else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "unknown object layout `{object_name}` (compiler bug)"
                    ))
                    .with_label(*span, "field write emitted here")
                    .with_code("E500"),
                );
                return;
            };
            let Some((is_ref, slot, field_ty)) = object_slot(def, name) else {
                e.diags.push(
                    Diagnostic::error(format!(
                        "object `{object_name}` has no usable field `{name}` (compiler bug)"
                    ))
                    .with_label(*span, "field write emitted here")
                    .with_code("E500"),
                );
                return;
            };
            let Some(obj) = e.rf_map.get(object).copied() else {
                e.diags.push(
                    Diagnostic::error("Naravm backend could not resolve an object reference")
                        .with_label(*span, "field write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let Some(slot) = u8::try_from(slot).ok() else {
                e.diags.push(
                    Diagnostic::error("Naravm object field slot is out of range (compiler bug)")
                        .with_label(*span, "field write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let kind = NaraKind::of_ty(ty).or_else(|| NaraKind::of_ty(field_ty));
            let Some(kind) = kind else {
                e.diags.push(
                    Diagnostic::error("Naravm backend could not resolve an object field type")
                        .with_label(*span, "field write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            if is_ref || kind.is_ref() {
                let Some(src) = e.ref_reg(*value, *span) else {
                    return;
                };
                if let Some(NaraKind::Tuple(kinds)) = e.kinds.get(value).cloned() {
                    let Some(copy) = e.fresh_rf(*span) else {
                        return;
                    };
                    if !nara_tuple_copy_into(e, copy, src, &kinds, *span) {
                        return;
                    }
                    e.bytecode.extend_from_slice(&[0x2f, obj, slot, copy]);
                    e.free_rf.push(copy);
                } else {
                    e.bytecode.extend_from_slice(&[0x2f, obj, slot, src]);
                }
            } else {
                let Some(src) = e.value_reg(*value, *span) else {
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2d, obj, slot, src]);
            }
        }
        Instr::ArrayLen { dst, array, span } => {
            let Some((a, _)) = e.array_reg(*array, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, NaraKind::U64);
                return;
            }
            let Some(d) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x2c, d, a, 0]); // getvati header
            e.rv_map.insert(*dst, d);
            e.kinds.insert(*dst, NaraKind::U64);
        }
        Instr::ArrayGet {
            dst,
            array,
            index,
            elem,
            span,
        } => {
            let Some(elem_kind) = NaraKind::of_ty(elem) else {
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, elem_kind);
                return;
            }
            let (Some((a, elem_kind)), Some(i)) =
                (e.array_reg(*array, *span), e.value_reg(*index, *span))
            else {
                e.invalid.insert(*dst);
                return;
            };
            let mut index_scratch = None;
            let i = if elem_kind.is_ref() {
                i
            } else {
                let Some(one) = e.ensure_one(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let (Some(shifted), Some(wrapped)) = (e.fresh_rv(*span), e.fresh_rv(*span)) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x30, shifted, i, one]); // add_u64 index, 1
                e.bytecode.extend_from_slice(&[0x10, wrapped, shifted, i]); // ltu detects wrap
                e.bytecode.extend_from_slice(&[0x24, wrapped, 0, 3]); // jz over fallback copy
                e.bytecode.extend_from_slice(&[0x04, shifted, i]); // preserve OOB on u64::MAX
                e.free_rv.push(wrapped);
                index_scratch = Some(shifted);
                shifted
            };
            if elem_kind.is_ref() {
                let Some(d) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.rf_map.insert(*dst, d);
                e.kinds.insert(*dst, elem_kind);
                e.bytecode.extend_from_slice(&[0x2a, d, a, i]); // getrfat
            } else {
                let Some(d) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.rv_map.insert(*dst, d);
                e.kinds.insert(*dst, elem_kind);
                e.bytecode.extend_from_slice(&[0x28, d, a, i]); // getvat
            }
            if let Some(shifted) = index_scratch {
                e.free_rv.push(shifted);
            }
        }
        Instr::ArraySet {
            array,
            index,
            value,
            elem: _,
            span,
        } => {
            // Statement-only: no destination to poison. Poisoned sides stay
            // quiet; unresolvable sides are already diagnosed by the helpers.
            if e.invalid.contains(array) || e.invalid.contains(index) || e.invalid.contains(value) {
                return;
            }
            let (Some((a, elem_kind)), Some(i)) =
                (e.array_reg(*array, *span), e.value_reg(*index, *span))
            else {
                return;
            };
            let mut index_scratch = None;
            let i = if elem_kind.is_ref() {
                i
            } else {
                let Some(one) = e.ensure_one(*span) else {
                    return;
                };
                let (Some(shifted), Some(wrapped)) = (e.fresh_rv(*span), e.fresh_rv(*span)) else {
                    return;
                };
                e.bytecode.extend_from_slice(&[0x30, shifted, i, one]); // add_u64 index, 1
                e.bytecode.extend_from_slice(&[0x10, wrapped, shifted, i]); // ltu detects wrap
                e.bytecode.extend_from_slice(&[0x24, wrapped, 0, 3]); // jz over fallback copy
                e.bytecode.extend_from_slice(&[0x04, shifted, i]); // preserve OOB on u64::MAX
                e.free_rv.push(wrapped);
                index_scratch = Some(shifted);
                shifted
            };
            if elem_kind.is_ref() {
                let Some(v) = e.ref_reg(*value, *span) else {
                    return;
                };
                // Tuple array elements copy: the array owns its element.
                if let NaraKind::Tuple(nested) = &elem_kind {
                    let nested = nested.clone();
                    let Some(tmp) = e.fresh_rf(*span) else {
                        return;
                    };
                    if !nara_tuple_copy_into(e, tmp, v, &nested, *span) {
                        e.free_rf.push(tmp);
                        return;
                    }
                    e.bytecode.extend_from_slice(&[0x2b, a, i, tmp]); // setrfat
                    e.free_rf.push(tmp);
                } else {
                    e.bytecode.extend_from_slice(&[0x2b, a, i, v]); // setrfat
                }
            } else {
                let Some(v) = e.value_reg(*value, *span) else {
                    return;
                };
                e.bytecode.extend_from_slice(&[0x29, a, i, v]); // setvat
            }
            if let Some(shifted) = index_scratch {
                e.free_rv.push(shifted);
            }
        }
        Instr::TupleLit {
            dst,
            elems,
            tys,
            span,
        } => {
            let Some(kinds) = nara_tuple_kinds(tys, *span, e) else {
                e.invalid.insert(*dst);
                return;
            };
            let tuple_kind = NaraKind::Tuple(kinds.clone());
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, tuple_kind);
                return;
            }
            for elem in elems {
                if e.invalid.contains(elem) {
                    e.invalid.insert(*dst);
                    return;
                }
            }
            let (values, refs) = tuple_lanes(&kinds);
            if values > u8::MAX as usize || refs > u8::MAX as usize {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm tuple has more than 255 elements in one register lane",
                    )
                    .with_label(*span, "tuple allocated here")
                    .with_note("split the tuple into smaller tuples")
                    .with_code("E404"),
                );
                e.invalid.insert(*dst);
                return;
            }
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.bytecode
                .extend_from_slice(&[0x27, rf, values as u8, refs as u8]); // createi
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, tuple_kind);
            for (i, elem) in elems.iter().enumerate() {
                let Some((is_ref, slot)) = tuple_slot(&kinds, i) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Ok(slot) = u8::try_from(slot) else {
                    e.invalid.insert(*dst);
                    return;
                };
                if is_ref {
                    let Some(v) = e.ref_reg(*elem, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    // Nested tuple elements are duplicated so the literal
                    // owns every tuple level; other references share.
                    if let Some(NaraKind::Tuple(nested)) = kinds.get(i).cloned() {
                        let Some(tmp) = e.fresh_rf(*span) else {
                            e.invalid.insert(*dst);
                            return;
                        };
                        if !nara_tuple_copy_into(e, tmp, v, &nested, *span) {
                            e.free_rf.push(tmp);
                            e.invalid.insert(*dst);
                            return;
                        }
                        e.bytecode.extend_from_slice(&[0x2f, rf, slot, tmp]); // setrfati
                        e.free_rf.push(tmp);
                    } else {
                        e.bytecode.extend_from_slice(&[0x2f, rf, slot, v]); // setrfati
                    }
                } else {
                    let Some(v) = e.value_reg(*elem, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    e.bytecode.extend_from_slice(&[0x2d, rf, slot, v]); // setvati
                }
            }
        }
        Instr::TupleGet {
            dst,
            tuple,
            index,
            tys,
            span,
        } => {
            let Some(kinds) = nara_tuple_kinds(tys, *span, e) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some((is_ref, slot)) = tuple_slot(&kinds, *index) else {
                e.diags.push(
                    Diagnostic::error("Naravm tuple index out of range (compiler bug)")
                        .with_label(*span, "tuple read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(elem_kind) = kinds.get(*index).cloned() else {
                e.invalid.insert(*dst);
                return;
            };
            let Ok(slot) = u8::try_from(slot) else {
                e.diags.push(
                    Diagnostic::error("Naravm tuple slot is out of range (compiler bug)")
                        .with_label(*span, "tuple read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, elem_kind);
                return;
            }
            let Some(obj) = e.rf_map.get(tuple).copied() else {
                if !e.invalid.contains(tuple) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a tuple reference")
                            .with_label(*span, "tuple read emitted here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            if is_ref {
                let Some(d) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2e, d, obj, slot]); // getrfati
                e.rf_map.insert(*dst, d);
            } else {
                let Some(d) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2c, d, obj, slot]); // getvati
                e.rv_map.insert(*dst, d);
            }
            e.kinds.insert(*dst, elem_kind);
        }
        Instr::TupleSet {
            tuple,
            index,
            value,
            tys,
            span,
        } => {
            if e.invalid.contains(tuple) || e.invalid.contains(value) {
                return;
            }
            let Some(kinds) = nara_tuple_kinds(tys, *span, e) else {
                return;
            };
            let Some((is_ref, slot)) = tuple_slot(&kinds, *index) else {
                e.diags.push(
                    Diagnostic::error("Naravm tuple index out of range (compiler bug)")
                        .with_label(*span, "tuple write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let Ok(slot) = u8::try_from(slot) else {
                e.diags.push(
                    Diagnostic::error("Naravm tuple slot is out of range (compiler bug)")
                        .with_label(*span, "tuple write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let Some(obj) = e.rf_map.get(tuple).copied() else {
                e.diags.push(
                    Diagnostic::error("Naravm backend expected a tuple reference")
                        .with_label(*span, "tuple write emitted here")
                        .with_code("E500"),
                );
                return;
            };
            let elem_kind = kinds.get(*index).cloned().unwrap_or(NaraKind::U64);
            if is_ref || elem_kind.is_ref() {
                let Some(src) = e.ref_reg(*value, *span) else {
                    return;
                };
                // Nested tuple values copy: the slot owns its container.
                if let NaraKind::Tuple(nested) = &elem_kind {
                    let nested = nested.clone();
                    let Some(tmp) = e.fresh_rf(*span) else {
                        return;
                    };
                    if !nara_tuple_copy_into(e, tmp, src, &nested, *span) {
                        e.free_rf.push(tmp);
                        return;
                    }
                    e.bytecode.extend_from_slice(&[0x2f, obj, slot, tmp]); // setrfati
                    e.free_rf.push(tmp);
                } else {
                    e.bytecode.extend_from_slice(&[0x2f, obj, slot, src]); // setrfati
                }
            } else {
                let Some(src) = e.value_reg(*value, *span) else {
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2d, obj, slot, src]); // setvati
            }
        }
        Instr::NewVariant {
            dst,
            union,
            tag,
            args,
            tys,
            span,
            ..
        } => {
            let Some(kinds) = nara_variant_kinds(tys, *span, e) else {
                e.invalid.insert(*dst);
                return;
            };
            let union_kind = NaraKind::Union(union.clone());
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, union_kind);
                return;
            }
            for arg in args {
                if e.invalid.contains(arg) {
                    e.invalid.insert(*dst);
                    return;
                }
            }
            let (values, refs) = tuple_lanes(&kinds);
            // Value slot 0 is the discriminant tag; payloads follow it.
            let values = values + 1;
            if values > u8::MAX as usize || refs > u8::MAX as usize {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm variant has more than 255 payloads in one register lane",
                    )
                    .with_label(*span, "variant allocated here")
                    .with_note("split the payload into smaller tuples")
                    .with_code("E404"),
                );
                e.invalid.insert(*dst);
                return;
            }
            let Some(rf) = e.fresh_rf(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.bytecode
                .extend_from_slice(&[0x27, rf, values as u8, refs as u8]); // createi
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, union_kind);
            // Tag first: intern the discriminant and store it in value slot 0.
            let Some(tag_idx) = e.add_value(u64::from(*tag), *span) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some(tag_rv) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.load_constant(false, tag_rv, tag_idx);
            e.bytecode.extend_from_slice(&[0x2d, rf, 0, tag_rv]); // setvati
            e.free_rv.push(tag_rv);
            for (i, arg) in args.iter().enumerate() {
                let Some((is_ref, slot)) = variant_payload_slot(&kinds, i) else {
                    e.invalid.insert(*dst);
                    return;
                };
                let Ok(slot) = u8::try_from(slot) else {
                    e.invalid.insert(*dst);
                    return;
                };
                if is_ref {
                    let Some(v) = e.ref_reg(*arg, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    // Nested tuple payloads are duplicated so the variant
                    // owns its container; other references share.
                    if let Some(NaraKind::Tuple(nested)) = kinds.get(i).cloned() {
                        let Some(tmp) = e.fresh_rf(*span) else {
                            e.invalid.insert(*dst);
                            return;
                        };
                        if !nara_tuple_copy_into(e, tmp, v, &nested, *span) {
                            e.free_rf.push(tmp);
                            e.invalid.insert(*dst);
                            return;
                        }
                        e.bytecode.extend_from_slice(&[0x2f, rf, slot, tmp]); // setrfati
                        e.free_rf.push(tmp);
                    } else {
                        e.bytecode.extend_from_slice(&[0x2f, rf, slot, v]); // setrfati
                    }
                } else {
                    let Some(v) = e.value_reg(*arg, *span) else {
                        e.invalid.insert(*dst);
                        return;
                    };
                    e.bytecode.extend_from_slice(&[0x2d, rf, slot, v]); // setvati
                }
            }
        }
        Instr::TagOf { dst, scrut, span } => {
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, NaraKind::U64);
                return;
            }
            let Some(obj) = e.rf_map.get(scrut).copied() else {
                if !e.invalid.contains(scrut) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a union reference")
                            .with_label(*span, "tag read emitted here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            let Some(d) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x2c, d, obj, 0]); // getvati (tag slot)
            e.rv_map.insert(*dst, d);
            e.kinds.insert(*dst, NaraKind::U64);
        }
        Instr::PayloadGet {
            dst,
            scrut,
            index,
            tys,
            span,
        } => {
            let Some(kinds) = nara_variant_kinds(tys, *span, e) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some((is_ref, slot)) = variant_payload_slot(&kinds, *index) else {
                e.diags.push(
                    Diagnostic::error("Naravm variant payload index out of range (compiler bug)")
                        .with_label(*span, "payload read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(elem_kind) = kinds.get(*index).cloned() else {
                e.invalid.insert(*dst);
                return;
            };
            let Ok(slot) = u8::try_from(slot) else {
                e.diags.push(
                    Diagnostic::error("Naravm variant payload slot is out of range (compiler bug)")
                        .with_label(*span, "payload read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, elem_kind);
                return;
            }
            let Some(obj) = e.rf_map.get(scrut).copied() else {
                if !e.invalid.contains(scrut) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a union reference")
                            .with_label(*span, "payload read emitted here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            if is_ref {
                let Some(d) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2e, d, obj, slot]); // getrfati
                e.rf_map.insert(*dst, d);
            } else {
                let Some(d) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2c, d, obj, slot]); // getvati
                e.rv_map.insert(*dst, d);
            }
            e.kinds.insert(*dst, elem_kind);
        }
        Instr::WrapOk {
            dst,
            value,
            ok,
            span,
        } => {
            let Some(ok_kind) = NaraKind::of_ok(ok) else {
                e.diags.push(
                    Diagnostic::error("Naravm backend found a non-runtime fallible payload")
                        .with_label(*span, "ok value wrapped here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, NaraKind::Fallible(Box::new(ok_kind)));
                return;
            }
            if e.invalid.contains(value) {
                e.invalid.insert(*dst);
                return;
            }
            // Tuples and reference payloads live in `rf`; value payloads
            // (and the `E!void` dummy) live in `rv`.
            let payload = if matches!(&ok_kind, NaraKind::Tuple(_)) || ok_kind.is_ref() {
                match e.rf_map.get(value).copied() {
                    Some(rf) => rf,
                    None => {
                        e.diags.push(
                            Diagnostic::error(
                                "Naravm backend could not resolve a fallible payload register",
                            )
                            .with_label(*span, "ok value wrapped here")
                            .with_code("E500"),
                        );
                        e.invalid.insert(*dst);
                        return;
                    }
                }
            } else {
                match e.rv_map.get(value).copied() {
                    Some(rv) => rv,
                    None => {
                        e.diags.push(
                            Diagnostic::error(
                                "Naravm backend could not resolve a fallible payload register",
                            )
                            .with_label(*span, "ok value wrapped here")
                            .with_code("E500"),
                        );
                        e.invalid.insert(*dst);
                        return;
                    }
                }
            };
            let Some(rf) = nara_fallible_create(e, &ok_kind, ctx.err_lanes, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            if !nara_fallible_tag(e, rf, 0, *span)
                || !nara_fallible_store(e, rf, payload, &ok_kind, *span)
            {
                e.invalid.insert(*dst);
                return;
            }
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, NaraKind::Fallible(Box::new(ok_kind)));
        }
        Instr::WrapErr {
            dst,
            code,
            ok,
            args,
            tys,
            span,
        } => {
            let Some(ok_kind) = NaraKind::of_ok(ok) else {
                e.diags.push(
                    Diagnostic::error("Naravm backend found a non-runtime fallible payload")
                        .with_label(*span, "error wrapped here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, NaraKind::Fallible(Box::new(ok_kind)));
                return;
            }
            if e.invalid.contains(code) {
                e.invalid.insert(*dst);
                return;
            }
            for arg in args {
                if e.invalid.contains(arg) {
                    e.invalid.insert(*dst);
                    return;
                }
            }
            let Some(kinds) = nara_variant_kinds(tys, *span, e) else {
                e.invalid.insert(*dst);
                return;
            };
            if kinds.len() != args.len() {
                e.diags.push(
                    Diagnostic::error("Naravm error payload arity drifted (compiler bug)")
                        .with_label(*span, "error wrapped here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            }
            let Some(code_rv) = e.rv_map.get(code).copied() else {
                e.diags.push(
                    Diagnostic::error("Naravm backend could not resolve an error code register")
                        .with_label(*span, "error wrapped here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(rf) = nara_fallible_create(e, &ok_kind, ctx.err_lanes, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            if !nara_fallible_tag(e, rf, 1, *span) {
                e.invalid.insert(*dst);
                return;
            }
            e.bytecode.extend_from_slice(&[0x2d, rf, 1, code_rv]); // setvati (code slot)
            if !nara_fallible_store_err(e, rf, args, &kinds, *span) {
                e.invalid.insert(*dst);
                return;
            }
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, NaraKind::Fallible(Box::new(ok_kind)));
        }
        Instr::RewrapErr {
            dst,
            scrut,
            ok,
            span,
        } => {
            let Some(ok_kind) = NaraKind::of_ok(ok) else {
                e.diags.push(
                    Diagnostic::error("Naravm backend found a non-runtime fallible payload")
                        .with_label(*span, "error forwarded here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, NaraKind::Fallible(Box::new(ok_kind)));
                return;
            }
            let Some(obj) = e.rf_map.get(scrut).copied() else {
                if !e.invalid.contains(scrut) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a fallible reference")
                            .with_label(*span, "error forwarded here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            let Some(rf) = nara_fallible_create(e, &ok_kind, ctx.err_lanes, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            if !nara_fallible_tag(e, rf, 1, *span) {
                e.invalid.insert(*dst);
                return;
            }
            // Copy the code plus the whole error region (value slots from
            // 2, reference slots from 0): every container shares the
            // program-wide error lanes, so the copy is always in range.
            let Some(tmp) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x2c, tmp, obj, 1]); // getvati (code slot)
            e.bytecode.extend_from_slice(&[0x2d, rf, 1, tmp]); // setvati (code slot)
            let mut failed = false;
            for slot in 0..ctx.err_lanes.0 {
                let (Ok(src), Ok(dst_slot)) = (u8::try_from(slot + 2), u8::try_from(slot + 2))
                else {
                    failed = true;
                    break;
                };
                e.bytecode.extend_from_slice(&[0x2c, tmp, obj, src]); // getvati
                e.bytecode.extend_from_slice(&[0x2d, rf, dst_slot, tmp]); // setvati
            }
            e.free_rv.push(tmp);
            if failed {
                e.invalid.insert(*dst);
                return;
            }
            if ctx.err_lanes.1 > 0 {
                let Some(rtmp) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                for slot in 0..ctx.err_lanes.1 {
                    let (Ok(s), Ok(d)) = (u8::try_from(slot), u8::try_from(slot)) else {
                        failed = true;
                        break;
                    };
                    e.bytecode.extend_from_slice(&[0x2e, rtmp, obj, s]); // getrfati
                    e.bytecode.extend_from_slice(&[0x2f, rf, d, rtmp]); // setrfati
                }
                e.free_rf.push(rtmp);
                if failed {
                    e.invalid.insert(*dst);
                    return;
                }
            }
            e.rf_map.insert(*dst, rf);
            e.kinds.insert(*dst, NaraKind::Fallible(Box::new(ok_kind)));
        }
        Instr::ErrPayloadGet {
            dst,
            scrut,
            index,
            tys,
            span,
        } => {
            let Some(kinds) = nara_variant_kinds(tys, *span, e) else {
                e.invalid.insert(*dst);
                return;
            };
            let Some((is_ref, slot)) = fallible_err_slot(&kinds, *index) else {
                e.diags.push(
                    Diagnostic::error("Naravm error payload index out of range (compiler bug)")
                        .with_label(*span, "payload read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(elem_kind) = kinds.get(*index).cloned() else {
                e.invalid.insert(*dst);
                return;
            };
            let Ok(slot) = u8::try_from(slot) else {
                e.diags.push(
                    Diagnostic::error("Naravm error payload slot is out of range (compiler bug)")
                        .with_label(*span, "payload read emitted here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, elem_kind);
                return;
            }
            let Some(obj) = e.rf_map.get(scrut).copied() else {
                if !e.invalid.contains(scrut) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a fallible reference")
                            .with_label(*span, "payload read emitted here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            if is_ref {
                let Some(d) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2e, d, obj, slot]); // getrfati
                e.rf_map.insert(*dst, d);
            } else {
                let Some(d) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode.extend_from_slice(&[0x2c, d, obj, slot]); // getvati
                e.rv_map.insert(*dst, d);
            }
            e.kinds.insert(*dst, elem_kind);
        }
        Instr::UnwrapOk {
            dst,
            scrut,
            ok,
            span,
        } => {
            let Some(ok_kind) = NaraKind::of_ok(ok) else {
                e.diags.push(
                    Diagnostic::error("Naravm backend found a non-runtime fallible payload")
                        .with_label(*span, "ok payload read here")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, ok_kind);
                return;
            }
            let Some(obj) = e.rf_map.get(scrut).copied() else {
                if !e.invalid.contains(scrut) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a fallible reference")
                            .with_label(*span, "ok payload read here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            let Some((is_ref, reg)) = nara_fallible_load(e, obj, &ok_kind, *span) else {
                e.invalid.insert(*dst);
                return;
            };
            if is_ref {
                e.rf_map.insert(*dst, reg);
            } else {
                e.rv_map.insert(*dst, reg);
            }
            e.kinds.insert(*dst, ok_kind);
        }
        Instr::UnwrapErr { dst, scrut, span } => {
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, NaraKind::U64);
                return;
            }
            let Some(obj) = e.rf_map.get(scrut).copied() else {
                if !e.invalid.contains(scrut) {
                    e.diags.push(
                        Diagnostic::error("Naravm backend expected a fallible reference")
                            .with_label(*span, "error code read here")
                            .with_code("E500"),
                    );
                }
                e.invalid.insert(*dst);
                return;
            };
            let Some(d) = e.fresh_rv(*span) else {
                e.invalid.insert(*dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x2c, d, obj, 1]); // getvati (code slot)
            e.rv_map.insert(*dst, d);
            e.kinds.insert(*dst, NaraKind::U64);
        }
        Instr::Ret { src, span } => nara_ret(e, ctx, *src, *span),
        Instr::GlobalLoad { dst, global, span } => {
            let Some((is_ref, slot)) = ctx.global_slots.get(global).copied() else {
                e.diags.push(
                    Diagnostic::error("internal compiler error: unknown global ID")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(gty) = ctx.global_tys.get(global).cloned() else {
                e.diags.push(
                    Diagnostic::error("internal compiler error: unknown global type")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            let Some(kind) = NaraKind::of_ty(&gty) else {
                e.diags.push(
                    Diagnostic::error("internal compiler error: global has non-runtime type")
                        .with_code("E500"),
                );
                e.invalid.insert(*dst);
                return;
            };
            // Discarded reads (`g;`) emit nothing, like dead `ArrayGet`.
            if !e.last_use.contains_key(dst) {
                e.kinds.insert(*dst, kind);
                return;
            }
            // Tuple globals copy by value on load so callee element
            // writes never mutate shared module state.
            if let NaraKind::Tuple(kinds) = &kind {
                let kinds = kinds.clone();
                if !e.last_use.contains_key(dst) {
                    e.kinds.insert(*dst, kind);
                    return;
                }
                let Some(tmp) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.bytecode
                    .extend_from_slice(&[0x2e, tmp, MODULE_STATE_RF, slot]); // getrfati
                let Some(rf) = e.fresh_rf(*span) else {
                    e.free_rf.push(tmp);
                    e.invalid.insert(*dst);
                    return;
                };
                e.rf_map.insert(*dst, rf);
                e.kinds.insert(*dst, kind);
                if !nara_tuple_copy_into(e, rf, tmp, &kinds, *span) {
                    e.invalid.insert(*dst);
                    e.free_rf.push(tmp);
                    return;
                }
                e.free_rf.push(tmp);
                return;
            }
            if is_ref {
                let Some(rf) = e.fresh_rf(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.rf_map.insert(*dst, rf);
                e.kinds.insert(*dst, kind);
                e.bytecode
                    .extend_from_slice(&[0x2e, rf, MODULE_STATE_RF, slot]); // getrfati
            } else {
                let Some(rv) = e.fresh_rv(*span) else {
                    e.invalid.insert(*dst);
                    return;
                };
                e.rv_map.insert(*dst, rv);
                e.kinds.insert(*dst, kind);
                e.bytecode
                    .extend_from_slice(&[0x2c, rv, MODULE_STATE_RF, slot]); // getvati
            }
        }
        Instr::GlobalStore { global, src, span } => {
            let Some((is_ref, slot)) = ctx.global_slots.get(global).copied() else {
                e.diags.push(
                    Diagnostic::error("internal compiler error: unknown global ID")
                        .with_code("E500"),
                );
                return;
            };
            // Tuple globals copy by value on store so later local element
            // writes never mutate the stored global.
            if let Some(gty) = ctx.global_tys.get(global).cloned() {
                if let Some(NaraKind::Tuple(kinds)) = NaraKind::of_ty(&gty) {
                    let Some(s) = e.ref_reg(*src, *span) else {
                        return;
                    };
                    let Some(tmp) = e.fresh_rf(*span) else {
                        return;
                    };
                    if !nara_tuple_copy_into(e, tmp, s, &kinds, *span) {
                        e.free_rf.push(tmp);
                        return;
                    }
                    e.bytecode
                        .extend_from_slice(&[0x2f, MODULE_STATE_RF, slot, tmp]); // setrfati
                    e.free_rf.push(tmp);
                    return;
                }
            }
            if is_ref {
                let Some(rf) = e.ref_reg(*src, *span) else {
                    return;
                };
                e.bytecode
                    .extend_from_slice(&[0x2f, MODULE_STATE_RF, slot, rf]); // setrfati
            } else {
                let Some(rv) = e.value_reg(*src, *span) else {
                    return;
                };
                e.bytecode
                    .extend_from_slice(&[0x2d, MODULE_STATE_RF, slot, rv]); // setvati
            }
        }
        Instr::BranchIfFalse { cond, target, span } => {
            let Some(c) = e.value_reg(*cond, *span) else {
                return;
            };
            let pos = e.bytecode.len();
            e.bytecode.extend_from_slice(&[0x24, c, 0, 0]); // jz
            e.patches.push(NaraPatch {
                pos,
                len: 4,
                target: *target,
                span: *span,
            });
        }
        Instr::Jump { target, span } => {
            let pos = e.bytecode.len();
            e.bytecode.extend_from_slice(&[0x22, 0, 0]); // jmp
            e.patches.push(NaraPatch {
                pos,
                len: 3,
                target: *target,
                span: *span,
            });
        }
        Instr::Label { id, .. } => {
            e.label_pos.insert(*id, e.bytecode.len());
        }
    }
}

fn nara_calli(e: &mut NaraEmit, fn_idx: usize, span: Span) {
    if fn_idx > u16::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm constant pool function index out of range")
                .with_label(span, "call emitted here")
                .with_code("E500"),
        );
        return;
    }
    e.bytecode
        .extend_from_slice(&[0x20, (fn_idx >> 8) as u8, fn_idx as u8]);
}

/// Callee prologue for one `Param`: copy the incoming argument register
/// (`rv11+i` for values, `rf31+j` for references — independent sequences)
/// into a fresh machine register. `Void`/`Error` params cannot occur past
/// the frontend; poison quietly instead of cascading.
fn nara_param(e: &mut NaraEmit, ctx: &NaraFnCtx, dst: vl_lir::Reg, index: usize, span: Span) {
    let ty = ctx
        .func
        .param_tys
        .get(index)
        .cloned()
        .unwrap_or(vl_typecheck::Ty::Error);
    let Some(kind) = NaraKind::of_ty(&ty) else {
        e.invalid.insert(dst);
        return;
    };
    if kind.is_ref() {
        if e.param_ri >= 9 {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend supports at most 9 reference parameters per function",
                )
                .with_label(span, "parameter here")
                .with_code("E404"),
            );
            e.invalid.insert(dst);
            return;
        }
        let src = 0x31 + e.param_ri;
        e.param_ri += 1;
        if !e.last_use.contains_key(&dst) {
            // Dead parameter: the slot is still consumed positionally, but
            // no machine register is spent on it.
            return;
        }
        let Some(rf) = e.fresh_rf(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.rf_map.insert(dst, rf);
        e.kinds.insert(dst, kind.clone());
        // Tuple parameters copy by value so callee element writes never
        // affect the caller's container.
        if let NaraKind::Tuple(kinds) = kind {
            if !nara_tuple_copy_into(e, rf, src, &kinds, span) {
                e.invalid.insert(dst);
            }
            return;
        }
        e.bytecode.extend_from_slice(&[0x05, rf, src]); // cprf
    } else {
        if e.param_vi >= 15 {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend supports at most 15 value parameters per function",
                )
                .with_label(span, "parameter here")
                .with_code("E404"),
            );
            e.invalid.insert(dst);
            return;
        }
        let src = 0x11 + e.param_vi;
        e.param_vi += 1;
        if !e.last_use.contains_key(&dst) {
            return;
        }
        let Some(rv) = e.fresh_rv(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.rv_map.insert(dst, rv);
        e.kinds.insert(dst, kind);
        e.bytecode.extend_from_slice(&[0x04, rv, src]); // cpv
    }
}

/// One spilled caller register: the value and reference stacks are
/// independent, so replaying the push sequence in reverse restores both.
enum NaraSpill {
    V(u8),
    F(u8),
}

/// Patch one forward jump (`jz`/`jmp`) once its target position is known.
/// Offsets are `i16` big-endian relative to the end of the instruction,
/// matching `nara_resolve_jumps`.
fn nara_patch_jump(e: &mut NaraEmit, pos: usize, len: usize, target: usize, span: Span) -> bool {
    let offset = target as isize - (pos + len) as isize;
    let Ok(offset) = i16::try_from(offset) else {
        e.diags.push(
            Diagnostic::error("Naravm jump offset out of range (function too large)")
                .with_label(span, "call emitted here")
                .with_code("E500"),
        );
        return false;
    };
    let bytes = offset.to_be_bytes();
    e.bytecode[pos + len - 2] = bytes[0];
    e.bytecode[pos + len - 1] = bytes[1];
    true
}

/// Emit `jz reg, <forward>` with a zero placeholder; returns the patch site.
fn nara_emit_jz(e: &mut NaraEmit, reg: u8) -> usize {
    let pos = e.bytecode.len();
    e.bytecode.extend_from_slice(&[0x24, reg, 0, 0]); // jz
    pos
}

/// Emit `jmp <forward>` with a zero placeholder; returns the patch site.
fn nara_emit_jmp(e: &mut NaraEmit) -> usize {
    let pos = e.bytecode.len();
    e.bytecode.extend_from_slice(&[0x22, 0, 0]); // jmp
    pos
}

/// Call a `std.net.tcp` native: stage actuals before filling the native slots
/// (`rv11+` for values, `rf31+` for references), preserve live registers,
/// `calli`, then
/// check the `rv10` status: 0 builds the `TcpError!T` ok container from the
/// result registers, nonzero maps to the matching `TcpError` variant code
/// and builds the error container. `dst` always names the container.
///
/// Result registers per native (see `docs/tcp-sockets.md` upstream):
/// `connect`/`accept` return the handle in `rv11`; `write` the byte count in
/// `rv11`; `close` returns nothing (`E!void` dummy payload); `listen`
/// returns `(listener, port)` in `rv11`/`rv12`; `read` returns
/// `(data, eof)` in `rf31`/`rv12` (the byte count equals the string length).
fn nara_tcp_call(
    e: &mut NaraEmit,
    ctx: &NaraFnCtx,
    dst: vl_lir::Reg,
    callee: &vl_lir::FunctionRef,
    args: &[vl_lir::Reg],
    span: Span,
) {
    let Some(import) = ctx.imports.get(callee) else {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: missing import for `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    let (param_tys, ret) = (import.param_tys.clone(), import.ret.clone());
    // Poisoned actuals stay quiet.
    for arg in args {
        if e.invalid.contains(arg) {
            e.invalid.insert(dst);
            return;
        }
    }
    if args.len() != param_tys.len() {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: arity mismatch calling `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    }
    // The import must be the declared fallible shape (`TcpError!T`); anything
    // else means `modules()` drifted from this emitter.
    let ok_ty = match &ret {
        vl_typecheck::Ty::Fallible(f) if f.err.as_deref() == Some(TCP_ERROR_SET) => f.ok.clone(),
        _ => {
            e.diags.push(
                Diagnostic::error(format!(
                    "codegen: `{callee}` is not a `TcpError!T` import (compiler bug)"
                ))
                .with_label(span, "call emitted here")
                .with_code("E500"),
            );
            e.invalid.insert(dst);
            return;
        }
    };
    let Some(ok_kind) = NaraKind::of_ok(&ok_ty) else {
        e.diags.push(
            Diagnostic::error("Naravm backend found a non-runtime TCP payload type")
                .with_label(span, "call emitted here")
                .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    // Native calls share the VM register file too; preserve live allocations.
    let spills = nara_spill_allocated(e);
    // Move actuals into the native slots, checking lanes against the import.
    let mut vi = 0u8;
    let mut ri = 0u8;
    for (ty, arg) in param_tys.iter().zip(args.iter()) {
        let Some(kind) = NaraKind::of_ty(ty) else {
            e.diags.push(
                Diagnostic::error("Naravm backend found a non-runtime TCP argument type")
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
            );
            e.invalid.insert(dst);
            return;
        };
        if kind.is_ref() {
            if kind != NaraKind::String {
                e.diags.push(
                    Diagnostic::error("Naravm backend requires a String argument to `std.net.tcp`")
                        .with_label(span, "unsupported argument")
                        .with_code("E402"),
                );
                e.invalid.insert(dst);
                return;
            }
            let Some(src) = e.rf_map.get(arg).copied() else {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve a TCP call argument (compiler bug)",
                    )
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x08, src]); // pushrf for staged arg
            ri += 1;
        } else {
            let Some(src) = e.rv_map.get(arg).copied() else {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve a TCP call argument (compiler bug)",
                    )
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x06, src]); // pushv for staged arg
            vi += 1;
        }
    }
    for ty in param_tys.iter().rev() {
        if NaraKind::of_ty(ty).is_some_and(|k| k.is_ref()) {
            ri -= 1;
            e.bytecode.extend_from_slice(&[0x09, 0x31 + ri]);
        } else {
            vi -= 1;
            e.bytecode.extend_from_slice(&[0x07, 0x11 + vi]);
        }
    }
    let Some(fn_idx) = ctx.imported_fn_consts.get(callee).copied() else {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: missing function constant for `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    nara_calli(e, fn_idx, span);
    if !e.last_use.contains_key(&dst) {
        // Dead result (e.g. a bare `tcp.close(h);` statement): the call's
        // side effects stand, but no container is built.
        e.kinds.insert(dst, NaraKind::Fallible(Box::new(ok_kind)));
        nara_restore_spills(e, &spills);
        return;
    }
    // Copy the status out of the reserved channel, then split ok/err.
    let status = 0x10; // native status is already in the reserved register
    let to_ok = nara_emit_jz(e, status);
    // Err path: select the variant code for statuses 1-7; anything else the
    // VM may report in the future surfaces as `IoError`.
    let (Some(code), Some(sc), Some(tt)) = (e.fresh_rv(span), e.fresh_rv(span), e.fresh_rv(span))
    else {
        e.invalid.insert(dst);
        return;
    };
    let mut to_have_code = Vec::with_capacity(TCP_STATUS_VARIANTS.len());
    for (status_value, variant) in TCP_STATUS_VARIANTS {
        let (Some(status_idx), Some(code_idx)) = (
            e.add_value(status_value, span),
            e.add_value(vl_hir::error_code(TCP_ERROR_SET, variant), span),
        ) else {
            e.invalid.insert(dst);
            return;
        };
        e.load_constant(false, sc, status_idx);
        e.bytecode.extend_from_slice(&[0x0a, tt, status, sc]); // eq
        let to_next = nara_emit_jz(e, tt);
        e.load_constant(false, code, code_idx);
        to_have_code.push(nara_emit_jmp(e));
        if !nara_patch_jump(e, to_next, 4, e.bytecode.len(), span) {
            e.invalid.insert(dst);
            return;
        }
    }
    let (_, fallthrough) = TCP_STATUS_VARIANTS
        .iter()
        .find(|(_, v)| *v == "IoError")
        .expect("IoError variant declared");
    let Some(fallthrough_idx) = e.add_value(vl_hir::error_code(TCP_ERROR_SET, fallthrough), span)
    else {
        e.invalid.insert(dst);
        return;
    };
    e.load_constant(false, code, fallthrough_idx);
    let have_code = e.bytecode.len();
    for jmp in to_have_code {
        if !nara_patch_jump(e, jmp, 3, have_code, span) {
            e.invalid.insert(dst);
            return;
        }
    }
    // One container register for both arms: only the taken arm's `createi`
    // executes, and the join below (plus all later uses of `dst`) sees it.
    let Some(cont) = e.fresh_rf(span) else {
        e.invalid.insert(dst);
        return;
    };
    if !nara_fallible_create_into(e, cont, &ok_kind, ctx.err_lanes, span) {
        e.invalid.insert(dst);
        return;
    }
    if !nara_fallible_tag(e, cont, 1, span) {
        e.invalid.insert(dst);
        return;
    }
    e.bytecode.extend_from_slice(&[0x2d, cont, 1, code]); // setvati (code slot)
    let to_end_err = nara_emit_jmp(e);
    // Ok path: collect the result registers into the payload, then wrap.
    let ok_pos = e.bytecode.len();
    if !nara_patch_jump(e, to_ok, 4, ok_pos, span) {
        e.invalid.insert(dst);
        return;
    }
    // Backend temporaries below die at the join; freed together there.
    let mut temps_rv: Vec<u8> = vec![code, sc, tt];
    let mut temps_rf: Vec<u8> = Vec::new();
    let ok_built = match callee.function.as_str() {
        // Single value result in `rv11` (handle or byte count).
        "connect" | "accept" | "write" => {
            if ok_kind != NaraKind::U64 {
                e.diags.push(
                    Diagnostic::error(format!(
                        "codegen: `{callee}` payload drifted from `u64` (compiler bug)"
                    ))
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            }
            let Some(payload) = e.fresh_rv(span) else {
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x04, payload, 0x11]); // cpv
            temps_rv.push(payload);
            nara_tcp_wrap_ok(e, dst, cont, payload, &ok_kind, ctx.err_lanes, span)
        }
        // No result (`E!void` keeps its dummy value lane).
        "close" => {
            if ok_ty != vl_typecheck::Ty::Void {
                e.diags.push(
                    Diagnostic::error(format!(
                        "codegen: `{callee}` payload drifted from `void` (compiler bug)"
                    ))
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            }
            let Some(zero) = e.ensure_zero(span) else {
                e.invalid.insert(dst);
                return;
            };
            nara_tcp_wrap_ok(e, dst, cont, zero, &ok_kind, ctx.err_lanes, span)
        }
        // `(listener, port)` in `rv11`/`rv12`.
        "listen" => nara_tcp_wrap_tuple(
            e,
            dst,
            cont,
            callee,
            span,
            &[(false, 0x11), (false, 0x12)],
            &ok_ty,
            &ok_kind,
            ctx.err_lanes,
            &mut temps_rv,
            &mut temps_rf,
        ),
        // `(data, eof)` in `rf31`/`rv12` (count equals the string length).
        "read" => nara_tcp_wrap_tuple(
            e,
            dst,
            cont,
            callee,
            span,
            &[(true, 0x31), (false, 0x12)],
            &ok_ty,
            &ok_kind,
            ctx.err_lanes,
            &mut temps_rv,
            &mut temps_rf,
        ),
        _ => {
            e.diags.push(
                Diagnostic::error(format!(
                    "Naravm backend does not support call `{callee}` yet"
                ))
                .with_label(span, "unsupported call")
                .with_code("E404"),
            );
            e.invalid.insert(dst);
            return;
        }
    };
    if !ok_built {
        e.invalid.insert(dst);
        return;
    }
    let end_pos = e.bytecode.len();
    if !nara_patch_jump(e, to_end_err, 3, end_pos, span) {
        e.invalid.insert(dst);
        return;
    }
    for rv in temps_rv {
        e.free_rv.push(rv);
    }
    for rf in temps_rf {
        e.free_rf.push(rf);
    }
    nara_restore_spills(e, &spills);
}

/// Wrap one value-reg payload into the `dst` fallible container (tag 0),
/// building into the pre-reserved `cont` register shared with the error arm.
fn nara_tcp_wrap_ok(
    e: &mut NaraEmit,
    dst: vl_lir::Reg,
    cont: u8,
    payload: u8,
    ok_kind: &NaraKind,
    err_lanes: (usize, usize),
    span: Span,
) -> bool {
    if !(nara_fallible_create_into(e, cont, ok_kind, err_lanes, span)
        && nara_fallible_tag(e, cont, 0, span)
        && nara_fallible_store(e, cont, payload, ok_kind, span))
    {
        return false;
    }
    e.rf_map.insert(dst, cont);
    e.kinds
        .insert(dst, NaraKind::Fallible(Box::new(ok_kind.clone())));
    true
}

/// Build a 2-tuple payload from fixed result registers, then wrap it into
/// the `dst` fallible container (tag 0), building into the pre-reserved
/// `cont` register shared with the error arm. `elems` is `(is_ref,
/// machine_reg)` per tuple position in order; the declared tuple shape is
/// validated against the import so `modules()` cannot drift from this
/// emitter.
#[allow(clippy::too_many_arguments)]
fn nara_tcp_wrap_tuple(
    e: &mut NaraEmit,
    dst: vl_lir::Reg,
    cont: u8,
    callee: &vl_lir::FunctionRef,
    span: Span,
    elems: &[(bool, u8)],
    ok_ty: &vl_typecheck::Ty,
    ok_kind: &NaraKind,
    err_lanes: (usize, usize),
    temps_rv: &mut Vec<u8>,
    temps_rf: &mut Vec<u8>,
) -> bool {
    let fields = match ok_ty {
        vl_typecheck::Ty::Tuple(fields) if fields.len() == elems.len() => fields.clone(),
        _ => {
            e.diags.push(
                Diagnostic::error(format!(
                    "codegen: `{callee}` payload drifted from its 2-tuple (compiler bug)"
                ))
                .with_label(span, "call emitted here")
                .with_code("E500"),
            );
            return false;
        }
    };
    let mut kinds = Vec::with_capacity(fields.len());
    for (_, ty) in &fields {
        let Some(kind) = NaraKind::of_ty(ty) else {
            e.diags.push(
                Diagnostic::error("Naravm backend found a non-runtime TCP payload type")
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
            );
            return false;
        };
        kinds.push(kind);
    }
    let (values, refs) = tuple_lanes(&kinds);
    if values > u8::MAX as usize || refs > u8::MAX as usize {
        e.diags.push(
            Diagnostic::error("Naravm TCP tuple has more than 255 elements in one register lane")
                .with_label(span, "call emitted here")
                .with_code("E404"),
        );
        return false;
    }
    let Some(staged) = nara_stage_native_results(e, elems, span) else {
        return false;
    };
    let Some(tup) = e.fresh_rf(span) else {
        return false;
    };
    e.bytecode
        .extend_from_slice(&[0x27, tup, values as u8, refs as u8]); // createi
    for (i, (want_ref, _src)) in elems.iter().enumerate() {
        let Some((is_ref, slot)) = tuple_slot(&kinds, i) else {
            return false;
        };
        if is_ref != *want_ref {
            e.diags.push(
                Diagnostic::error(format!(
                    "codegen: `{callee}` result lane drifted from its tuple (compiler bug)"
                ))
                .with_label(span, "call emitted here")
                .with_code("E500"),
            );
            return false;
        }
        let Ok(slot) = u8::try_from(slot) else {
            e.diags.push(
                Diagnostic::error("Naravm tuple slot is out of range (compiler bug)")
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
            );
            return false;
        };
        if is_ref {
            let tmp = staged[i];
            e.bytecode.extend_from_slice(&[0x2f, tup, slot, tmp]); // setrfati
            temps_rf.push(tmp);
        } else {
            let tmp = staged[i];
            e.bytecode.extend_from_slice(&[0x2d, tup, slot, tmp]); // setvati
            temps_rv.push(tmp);
        }
    }
    temps_rf.push(tup);
    let tuple_kind = NaraKind::Tuple(kinds);
    if tuple_kind != *ok_kind {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: `{callee}` payload kind drifted from its tuple (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        return false;
    }
    nara_tcp_wrap_ok(e, dst, cont, tup, ok_kind, err_lanes, span)
}

/// Call a single-result `rv10`-status native: move actuals into the native
/// slots (`rv11+` for values, `rf31+` for references — the same convention
/// as `nara_user_call`), preserving allocated registers around `calli`,
/// then check the `rv10` status: 0 builds the ok container from the result
/// register, nonzero maps through `statuses` to the matching error variant
/// code (unlisted statuses use `fallthrough`) and builds the error
/// container. `dst` always names the container.
///
/// Result registers per native: `std.string.byte_at` returns the byte in
/// `rv11`; `std.string.slice` and `std.fs.read_file` return the string in
/// `rf31`. Covered callees: `std.string::{byte_at, slice}`,
/// `std.fs::read_file`.
fn nara_checked_call(
    e: &mut NaraEmit,
    ctx: &NaraFnCtx,
    dst: vl_lir::Reg,
    callee: &vl_lir::FunctionRef,
    args: &[vl_lir::Reg],
    span: Span,
) {
    struct Spec {
        set: &'static str,
        statuses: &'static [(u64, &'static str)],
        fallthrough: &'static str,
        /// (is_ref, machine reg) holding the ok payload on success.
        result: (bool, u8),
    }
    let spec = match (callee.module.as_str(), callee.function.as_str()) {
        ("std.string", "byte_at") => Spec {
            set: STRING_ERROR_SET,
            statuses: &STRING_STATUS_VARIANTS,
            fallthrough: "OutOfBounds",
            result: (false, 0x11),
        },
        ("std.string", "slice") => Spec {
            set: STRING_ERROR_SET,
            statuses: &STRING_STATUS_VARIANTS,
            fallthrough: "OutOfBounds",
            result: (true, 0x31),
        },
        ("std.fs", "read_file") => Spec {
            set: FS_ERROR_SET,
            statuses: &FS_STATUS_VARIANTS,
            fallthrough: "IoError",
            result: (true, 0x31),
        },
        _ => {
            e.diags.push(
                Diagnostic::error(format!(
                    "Naravm backend does not support checked call `{callee}` yet"
                ))
                .with_label(span, "unsupported call")
                .with_code("E404"),
            );
            e.invalid.insert(dst);
            return;
        }
    };
    let Some(import) = ctx.imports.get(callee) else {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: missing import for `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    let (param_tys, ret) = (import.param_tys.clone(), import.ret.clone());
    for arg in args {
        if e.invalid.contains(arg) {
            e.invalid.insert(dst);
            return;
        }
    }
    if args.len() != param_tys.len() {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: arity mismatch calling `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    }
    // The import must be the declared fallible shape (`Set!T`); anything
    // else means `modules()` drifted from this emitter.
    let ok_ty = match &ret {
        vl_typecheck::Ty::Fallible(f) if f.err.as_deref() == Some(spec.set) => f.ok.clone(),
        _ => {
            e.diags.push(
                Diagnostic::error(format!(
                    "codegen: `{callee}` is not a `{}!T` import (compiler bug)",
                    spec.set
                ))
                .with_label(span, "call emitted here")
                .with_code("E500"),
            );
            e.invalid.insert(dst);
            return;
        }
    };
    let Some(ok_kind) = NaraKind::of_ok(&ok_ty) else {
        e.diags.push(
            Diagnostic::error("Naravm backend found a non-runtime checked payload type")
                .with_label(span, "call emitted here")
                .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    // Native calls share the VM register file too; preserve live allocations.
    let spills = nara_spill_allocated(e);
    // Move actuals into the native slots, checking lanes against the import.
    // All checked natives take `String` references plus `u64` values today.
    let mut vi = 0u8;
    let mut ri = 0u8;
    for (ty, arg) in param_tys.iter().zip(args.iter()) {
        let Some(kind) = NaraKind::of_ty(ty) else {
            e.diags.push(
                Diagnostic::error("Naravm backend found a non-runtime checked argument type")
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
            );
            e.invalid.insert(dst);
            return;
        };
        if kind.is_ref() {
            if kind != NaraKind::String {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend requires a String argument to a checked call",
                    )
                    .with_label(span, "unsupported argument")
                    .with_code("E402"),
                );
                e.invalid.insert(dst);
                return;
            }
            let Some(src) = e.rf_map.get(arg).copied() else {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve a checked call argument (compiler bug)",
                    )
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x08, src]); // pushrf for staged arg
            ri += 1;
        } else {
            let Some(src) = e.rv_map.get(arg).copied() else {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve a checked call argument (compiler bug)",
                    )
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x06, src]); // pushv for staged arg
            vi += 1;
        }
    }
    for ty in param_tys.iter().rev() {
        if NaraKind::of_ty(ty).is_some_and(|k| k.is_ref()) {
            ri -= 1;
            e.bytecode.extend_from_slice(&[0x09, 0x31 + ri]);
        } else {
            vi -= 1;
            e.bytecode.extend_from_slice(&[0x07, 0x11 + vi]);
        }
    }
    let Some(fn_idx) = ctx.imported_fn_consts.get(callee).copied() else {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: missing function constant for `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    nara_calli(e, fn_idx, span);
    if !e.last_use.contains_key(&dst) {
        // Dead result: the call's side effects stand, but no container is built.
        e.kinds.insert(dst, NaraKind::Fallible(Box::new(ok_kind)));
        nara_restore_spills(e, &spills);
        return;
    }
    // Copy the status out of the reserved channel, then split ok/err.
    let status = 0x10; // native status is already in the reserved register
    let to_ok = nara_emit_jz(e, status);
    // Err path: select the variant code per status; unlisted statuses use
    // the fallthrough variant.
    let (Some(code), Some(sc), Some(tt)) = (e.fresh_rv(span), e.fresh_rv(span), e.fresh_rv(span))
    else {
        e.invalid.insert(dst);
        return;
    };
    let mut to_have_code = Vec::with_capacity(spec.statuses.len());
    for (status_value, variant) in spec.statuses {
        let (Some(status_idx), Some(code_idx)) = (
            e.add_value(*status_value, span),
            e.add_value(vl_hir::error_code(spec.set, variant), span),
        ) else {
            e.invalid.insert(dst);
            return;
        };
        e.load_constant(false, sc, status_idx);
        e.bytecode.extend_from_slice(&[0x0a, tt, status, sc]); // eq
        let to_next = nara_emit_jz(e, tt);
        e.load_constant(false, code, code_idx);
        to_have_code.push(nara_emit_jmp(e));
        if !nara_patch_jump(e, to_next, 4, e.bytecode.len(), span) {
            e.invalid.insert(dst);
            return;
        }
    }
    let Some(fallthrough_idx) = e.add_value(vl_hir::error_code(spec.set, spec.fallthrough), span)
    else {
        e.invalid.insert(dst);
        return;
    };
    e.load_constant(false, code, fallthrough_idx);
    let have_code = e.bytecode.len();
    for jmp in to_have_code {
        if !nara_patch_jump(e, jmp, 3, have_code, span) {
            e.invalid.insert(dst);
            return;
        }
    }
    // One container register for both arms: only the taken arm's `createi`
    // executes, and the join below (plus all later uses of `dst`) sees it.
    let Some(cont) = e.fresh_rf(span) else {
        e.invalid.insert(dst);
        return;
    };
    if !nara_fallible_create_into(e, cont, &ok_kind, ctx.err_lanes, span) {
        e.invalid.insert(dst);
        return;
    }
    if !nara_fallible_tag(e, cont, 1, span) {
        e.invalid.insert(dst);
        return;
    }
    e.bytecode.extend_from_slice(&[0x2d, cont, 1, code]); // setvati (code slot)
    let to_end_err = nara_emit_jmp(e);
    // Ok path: collect the result register into the payload, then wrap.
    let ok_pos = e.bytecode.len();
    if !nara_patch_jump(e, to_ok, 4, ok_pos, span) {
        e.invalid.insert(dst);
        return;
    }
    let mut temps_rv: Vec<u8> = vec![code, sc, tt];
    let mut temps_rf: Vec<u8> = Vec::new();
    let (result_ref, result_reg) = spec.result;
    // The checked natives declare single-lane payloads; anything else means
    // `modules()` drifted from this emitter.
    let lane_ok = if result_ref {
        ok_kind.is_ref() && !matches!(ok_kind, NaraKind::Tuple(_))
    } else {
        !ok_kind.is_ref() && !matches!(ok_kind, NaraKind::Tuple(_))
    };
    if !lane_ok {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: `{callee}` payload drifted from its result lane (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    }
    let ok_built = if result_ref {
        let Some(tmp) = e.fresh_rf(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.bytecode.extend_from_slice(&[0x05, tmp, result_reg]); // cprf
        temps_rf.push(tmp);
        nara_tcp_wrap_ok(e, dst, cont, tmp, &ok_kind, ctx.err_lanes, span)
    } else {
        let Some(tmp) = e.fresh_rv(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.bytecode.extend_from_slice(&[0x04, tmp, result_reg]); // cpv
        temps_rv.push(tmp);
        nara_tcp_wrap_ok(e, dst, cont, tmp, &ok_kind, ctx.err_lanes, span)
    };
    if !ok_built {
        e.invalid.insert(dst);
        return;
    }
    let end_pos = e.bytecode.len();
    if !nara_patch_jump(e, to_end_err, 3, end_pos, span) {
        e.invalid.insert(dst);
        return;
    }
    for rv in temps_rv {
        e.free_rv.push(rv);
    }
    for rf in temps_rf {
        e.free_rf.push(rf);
    }
    nara_restore_spills(e, &spills);
}

/// Call a user function: spill live caller registers (the register file is
/// VM-global, shared across frames), move actuals into the callee's param
/// slots, `calli`, copy the return out, then restore the spills.
fn nara_user_call(
    e: &mut NaraEmit,
    ctx: &NaraFnCtx,
    dst: vl_lir::Reg,
    callee: &vl_lir::FunctionRef,
    args: &[vl_lir::Reg],
    span: Span,
) {
    let signature = if callee.module == ctx.module {
        ctx.sigs
            .get(callee.function.as_str())
            .map(|f| (f.param_tys.clone(), f.ret.clone()))
    } else {
        ctx.imports
            .get(callee)
            .map(|f| (f.param_tys.clone(), f.ret.clone()))
    };
    let Some((param_tys, ret)) = signature else {
        e.diags.push(
            Diagnostic::error(format!(
                "Naravm backend does not support call `{callee}` yet"
            ))
            .with_label(span, "unsupported call")
            .with_note(
                "only `std.print`, `std.println`, `std.print_u64`, the `std.string`/`std.math`/`std.fmt`/`std.net.tcp` natives, and user functions lower to Naravm calls",
            )
            .with_code("E404"),
        );
        e.invalid.insert(dst);
        return;
    };
    // Poisoned actuals stay quiet.
    for arg in args {
        if e.invalid.contains(arg) {
            e.invalid.insert(dst);
            return;
        }
    }
    if args.len() != param_tys.len() {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: arity mismatch calling `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    }
    // Partition actuals by the caller's tracked register kind with the same
    // two independent counters the prologue uses.
    let mut actuals: Vec<(bool, u8)> = Vec::with_capacity(args.len());
    let mut values = 0u8;
    let mut refs = 0u8;
    for arg in args {
        match e.kinds.get(arg).cloned() {
            Some(kind) if kind.is_ref() => {
                let Some(src) = e.rf_map.get(arg).copied() else {
                    e.diags.push(
                        Diagnostic::error(
                            "Naravm backend could not resolve a call argument (compiler bug)",
                        )
                        .with_label(span, "call emitted here")
                        .with_code("E500"),
                    );
                    e.invalid.insert(dst);
                    return;
                };
                refs += 1;
                actuals.push((true, src));
            }
            Some(_) => {
                let Some(src) = e.rv_map.get(arg).copied() else {
                    e.diags.push(
                        Diagnostic::error(
                            "Naravm backend could not resolve a call argument (compiler bug)",
                        )
                        .with_label(span, "call emitted here")
                        .with_code("E500"),
                    );
                    e.invalid.insert(dst);
                    return;
                };
                values += 1;
                actuals.push((false, src));
            }
            None => {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve a call argument (compiler bug)",
                    )
                    .with_label(span, "call emitted here")
                    .with_code("E500"),
                );
                e.invalid.insert(dst);
                return;
            }
        }
    }
    if values > 15 || refs > 9 {
        e.diags.push(
            Diagnostic::error(
                "Naravm backend supports at most 15 value and 9 reference arguments per call",
            )
            .with_label(span, "call emitted here")
            .with_code("E404"),
        );
        e.invalid.insert(dst);
        return;
    }
    // Reserve the return register before spilling so it cannot alias a live
    // caller register. This emits no code, only reserves a register number.
    let ret_kind = NaraKind::of_ty(&ret);
    let ret_rv = match &ret_kind {
        Some(kind) if !kind.is_ref() => match e.fresh_rv(span) {
            Some(rv) => Some(rv),
            None => {
                e.invalid.insert(dst);
                return;
            }
        },
        _ => None,
    };
    let ret_rf = match &ret_kind {
        Some(kind) if kind.is_ref() => match e.fresh_rf(span) {
            Some(rf) => Some(rf),
            None => {
                e.invalid.insert(dst);
                return;
            }
        },
        _ => None,
    };

    let mut rvs: Vec<u8> = e.rv_map.values().copied().collect();
    rvs.sort_unstable();
    let mut rfs: Vec<u8> = e.rf_map.values().copied().collect();
    rfs.sort_unstable();
    let mut spills: Vec<NaraSpill> = Vec::new();
    for rv in rvs {
        e.bytecode.extend_from_slice(&[0x06, rv]); // pushv
        spills.push(NaraSpill::V(rv));
    }
    // Cached comparison temporaries are live machine state too; without a
    // spill the callee (which allocates from register 0) would clobber them.
    if let Some(one) = e.one_rv {
        e.bytecode.extend_from_slice(&[0x06, one]);
        spills.push(NaraSpill::V(one));
    }
    if let Some(bias) = e.bias_rv {
        e.bytecode.extend_from_slice(&[0x06, bias]);
        spills.push(NaraSpill::V(bias));
    }
    if let Some(zero) = e.zero_rv {
        e.bytecode.extend_from_slice(&[0x06, zero]);
        spills.push(NaraSpill::V(zero));
    }
    for rf in rfs {
        e.bytecode.extend_from_slice(&[0x08, rf]); // pushrf
        spills.push(NaraSpill::F(rf));
    }
    nara_stage_call_args(e, &actuals);
    let fn_idx = if callee.module == ctx.module {
        ctx.fn_consts.get(&callee.function).copied()
    } else {
        ctx.imported_fn_consts.get(callee).copied()
    };
    let Some(fn_idx) = fn_idx else {
        e.diags.push(
            Diagnostic::error(format!(
                "codegen: missing function constant for `{callee}` (compiler bug)"
            ))
            .with_label(span, "call emitted here")
            .with_code("E500"),
        );
        e.invalid.insert(dst);
        return;
    };
    nara_calli(e, fn_idx, span);
    match ret_kind {
        // `Void`/`Error` returns map to nothing; the destination stays dead.
        None => {}
        Some(kind) if kind.is_ref() => {
            let rf = ret_rf.expect("reserved above");
            e.bytecode.extend_from_slice(&[0x05, rf, 0x31]); // cprf rf, rf31
            e.rf_map.insert(dst, rf);
            e.kinds.insert(dst, kind);
        }
        Some(kind) => {
            let rv = ret_rv.expect("reserved above");
            if rv != 0x11 {
                e.bytecode.extend_from_slice(&[0x04, rv, 0x11]); // cpv rv, rv11
            }
            e.rv_map.insert(dst, rv);
            e.kinds.insert(dst, kind);
        }
    }
    for spill in spills.iter().rev() {
        match spill {
            NaraSpill::V(rv) => e.bytecode.extend_from_slice(&[0x07, *rv]), // popv
            NaraSpill::F(rf) => e.bytecode.extend_from_slice(&[0x09, *rf]), // poprf
        }
    }
}

/// Function epilogue: move the explicit `return` value into the return slot
/// (`rv11` / `rf31` per the declared return kind), then `ret`. `main` and
/// `void` functions emit a bare `ret` as before.
fn nara_ret(e: &mut NaraEmit, ctx: &NaraFnCtx, src: vl_lir::Reg, span: Span) {
    if ctx.is_main {
        e.bytecode.push(0x00);
        return;
    }
    let ret = match NaraKind::of_ty(&ctx.func.ret) {
        None => {
            e.bytecode.push(0x00);
            return;
        }
        Some(kind) => kind,
    };
    if e.invalid.contains(&src) {
        e.bytecode.push(0x00);
        return;
    }
    if ret.is_ref() {
        match e.rf_map.get(&src).copied() {
            Some(s) => {
                e.bytecode.extend_from_slice(&[0x05, 0x31, s]); // cprf rf31, src
            }
            None => {
                e.diags.push(
                    Diagnostic::error(
                        "Naravm backend could not resolve the return register (compiler bug)",
                    )
                    .with_label(span, "return emitted here")
                    .with_code("E500"),
                );
            }
        }
        e.bytecode.push(0x00);
        return;
    }
    match e.rv_map.get(&src).copied() {
        Some(s) => {
            if s != 0x11 {
                e.bytecode.extend_from_slice(&[0x04, 0x11, s]); // cpv rv11, src
            }
        }
        None => {
            e.diags.push(
                Diagnostic::error(
                    "Naravm backend could not resolve the return register (compiler bug)",
                )
                .with_label(span, "return emitted here")
                .with_code("E500"),
            );
        }
    }
    e.bytecode.push(0x00);
}

/// String equality uses the existing byte-comparison native, with the same
/// argument spills as explicit calls so live rf32 values remain intact.
fn nara_string_eq(
    e: &mut NaraEmit,
    ctx: &NaraFnCtx,
    dst: vl_lir::Reg,
    lhs: vl_lir::Reg,
    rhs: vl_lir::Reg,
    span: Span,
) {
    let callee = vl_lir::FunctionRef {
        module: "std.string".into(),
        function: "eq".into(),
    };
    let fn_idx = if let Some(idx) = ctx.imported_fn_consts.get(&callee).copied() {
        idx
    } else if let Some(idx) = e.string_eq_fn_idx {
        idx
    } else {
        let (Some(module), Some(function)) = (
            e.add_string(b"std::string", span),
            e.add_string(b"eq", span),
        ) else {
            e.invalid.insert(dst);
            return;
        };
        let Some(idx) = nara_push_fn_const(e, module, function) else {
            e.invalid.insert(dst);
            return;
        };
        e.string_eq_fn_idx = Some(idx);
        idx
    };
    let mut imports = ctx.imports.clone();
    imports.insert(
        callee.clone(),
        vl_lir::FunctionImport {
            symbol: callee.clone(),
            param_tys: vec![vl_typecheck::Ty::String, vl_typecheck::Ty::String],
            ret: vl_typecheck::Ty::Bool,
        },
    );
    let mut imported_fn_consts = ctx.imported_fn_consts.clone();
    imported_fn_consts.insert(callee.clone(), fn_idx);
    let call_ctx = NaraFnCtx {
        imports: &imports,
        imported_fn_consts: &imported_fn_consts,
        ..*ctx
    };
    nara_user_call(e, &call_ctx, dst, &callee, &[lhs, rhs], span);
}

fn nara_binop(
    e: &mut NaraEmit,
    dst: vl_lir::Reg,
    op: LirOp,
    lhs: vl_lir::Reg,
    rhs: vl_lir::Reg,
    span: Span,
) {
    let fail = |e: &mut NaraEmit| {
        e.invalid.insert(dst);
    };
    // Resolve operand kinds before machine registers: String operands live in
    // reference registers, so resolving value registers first would misreport
    // them as a compiler bug instead of clean E404 diagnostics.
    let lkind = e.kinds.get(&lhs).cloned();
    let rkind = e.kinds.get(&rhs).cloned();
    if lkind != rkind || lkind.is_none() {
        if e.invalid.contains(&lhs) || e.invalid.contains(&rhs) {
            fail(e);
            return;
        }
        e.diags.push(
            Diagnostic::error("Naravm backend found mismatched operand types")
                .with_label(span, "emitted here")
                .with_code("E500"),
        );
        fail(e);
        return;
    }
    let kind = lkind.unwrap_or(NaraKind::I64);
    if kind.is_ref() {
        e.diags.push(
            Diagnostic::error(format!(
                "Naravm backend does not support `{op}` on `{kind:?}` yet"
            ))
            .with_label(span, "unsupported operation")
            .with_code("E404"),
        );
        fail(e);
        return;
    }
    let Some(l) = e.value_reg(lhs, span) else {
        fail(e);
        return;
    };
    let Some(r) = e.value_reg(rhs, span) else {
        fail(e);
        return;
    };
    let alloc = |e: &mut NaraEmit| {
        let d = e.fresh_rv(span)?;
        e.rv_map.insert(dst, d);
        Some(d)
    };
    match op {
        LirOp::Add | LirOp::Sub | LirOp::Mul | LirOp::Div => {
            let opcode = match (op, &kind) {
                (LirOp::Add, NaraKind::U64) | (LirOp::Add, NaraKind::U8) => 0x30,
                (LirOp::Add, NaraKind::I64) => 0x31,
                (LirOp::Add, NaraKind::F64) => 0x32,
                (LirOp::Sub, NaraKind::U64) | (LirOp::Sub, NaraKind::U8) => 0x33,
                (LirOp::Sub, NaraKind::I64) => 0x34,
                (LirOp::Sub, NaraKind::F64) => 0x35,
                (LirOp::Mul, NaraKind::U64) | (LirOp::Mul, NaraKind::U8) => 0x36,
                (LirOp::Mul, NaraKind::I64) => 0x37,
                (LirOp::Mul, NaraKind::F64) => 0x38,
                (LirOp::Div, NaraKind::U64) | (LirOp::Div, NaraKind::U8) => 0x39,
                (LirOp::Div, NaraKind::I64) => 0x3a,
                (LirOp::Div, NaraKind::F64) => 0x3b,
                _ => {
                    e.diags.push(
                        Diagnostic::error(format!(
                            "Naravm backend does not support `{op}` on `{kind:?}` yet"
                        ))
                        .with_label(span, "unsupported operation")
                        .with_code("E404"),
                    );
                    fail(e);
                    return;
                }
            };
            let Some(d) = alloc(e) else {
                fail(e);
                return;
            };
            e.kinds.insert(dst, kind);
            e.bytecode.extend_from_slice(&[opcode, d, l, r]);
        }
        LirOp::Eq => {
            if !matches!(
                kind,
                NaraKind::U64 | NaraKind::I64 | NaraKind::F64 | NaraKind::Bool | NaraKind::U8
            ) {
                e.diags.push(
                    Diagnostic::error(format!(
                        "Naravm backend does not support equality on `{kind:?}` yet"
                    ))
                    .with_label(span, "unsupported operation")
                    .with_code("E404"),
                );
                fail(e);
                return;
            }
            let Some(d) = alloc(e) else {
                fail(e);
                return;
            };
            e.kinds.insert(dst, NaraKind::Bool);
            if kind == NaraKind::F64 {
                nara_float_eq(e, d, l, r, false, span);
            } else {
                e.bytecode.extend_from_slice(&[0x0a, d, l, r]); // eq
            }
        }
        LirOp::Ne => {
            if !matches!(
                kind,
                NaraKind::U64 | NaraKind::I64 | NaraKind::F64 | NaraKind::Bool | NaraKind::U8
            ) {
                e.diags.push(
                    Diagnostic::error(format!(
                        "Naravm backend does not support equality on `{kind:?}` yet"
                    ))
                    .with_label(span, "unsupported operation")
                    .with_code("E404"),
                );
                fail(e);
                return;
            }
            let (Some(d), Some(one)) = (alloc(e), e.ensure_one(span)) else {
                fail(e);
                return;
            };
            e.kinds.insert(dst, NaraKind::Bool);
            if kind == NaraKind::F64 {
                nara_float_eq(e, d, l, r, true, span);
            } else {
                e.bytecode.extend_from_slice(&[0x0a, d, l, r]); // eq
                e.bytecode.extend_from_slice(&[0x12, d, d, one]); // xor 1
            }
        }
        LirOp::Lt | LirOp::Le | LirOp::Gt | LirOp::Ge => {
            nara_compare(e, dst, op, kind, l, r, span);
        }
    }
}

/// IEEE equality from integer operations. `abs(bits)` is formed by subtracting
/// the sign bias only when the sign bit is set. NaNs have absolute bit patterns
/// greater than positive infinity; both signed zero patterns compare equal.
fn nara_float_eq(e: &mut NaraEmit, dst: u8, lhs: u8, rhs: u8, negate: bool, span: Span) {
    let (Some(bias), Some(zero), Some(one)) =
        (e.ensure_bias(span), e.ensure_zero(span), e.ensure_one(span))
    else {
        return;
    };
    let (Some(abs_a), Some(abs_b), Some(flag), Some(mask), Some(work)) = (
        e.fresh_rv(span),
        e.fresh_rv(span),
        e.fresh_rv(span),
        e.fresh_rv(span),
        e.fresh_rv(span),
    ) else {
        return;
    };
    // Start with bit equality, then compute absolute bit patterns.
    e.bytecode.extend_from_slice(&[0x0a, dst, lhs, rhs]);
    e.bytecode.extend_from_slice(&[0x10, flag, lhs, bias]); // positive iff lhs < sign bit
    e.bytecode.extend_from_slice(&[0x12, flag, flag, one]); // negative sign bit
    e.bytecode.extend_from_slice(&[0x36, mask, bias, flag]);
    e.bytecode.extend_from_slice(&[0x33, abs_a, lhs, mask]); // abs lhs bits
    e.bytecode.extend_from_slice(&[0x10, flag, rhs, bias]);
    e.bytecode.extend_from_slice(&[0x12, flag, flag, one]);
    e.bytecode.extend_from_slice(&[0x36, mask, bias, flag]);
    e.bytecode.extend_from_slice(&[0x33, abs_b, rhs, mask]); // abs rhs bits
    e.bytecode.extend_from_slice(&[0x0a, flag, abs_a, zero]);
    e.bytecode.extend_from_slice(&[0x0a, mask, abs_b, zero]);
    e.bytecode.extend_from_slice(&[0x36, work, flag, mask]); // both are zero
    e.bytecode.extend_from_slice(&[0x30, work, dst, work]);
    e.bytecode.extend_from_slice(&[0x10, dst, zero, work]); // raw equal OR both zero

    let Some(inf_idx) = e.add_value(0x7ff0_0000_0000_0000, span) else {
        return;
    };
    e.load_constant(false, mask, inf_idx);
    e.bytecode.extend_from_slice(&[0x10, flag, mask, abs_a]); // lhs abs > +infinity => NaN
    e.bytecode.extend_from_slice(&[0x12, flag, flag, one]); // lhs is not NaN
    e.bytecode.extend_from_slice(&[0x36, dst, dst, flag]);
    if negate {
        e.bytecode.extend_from_slice(&[0x12, dst, dst, one]);
    }
    for rv in [abs_a, abs_b, flag, mask, work] {
        e.free_rv.push(rv);
    }
}

/// Lower one ordering comparison. Unsigned kinds use `ltu` directly; signed
/// `i64` flips the sign bit on both sides first so the unsigned compare
/// yields signed order. `f64` and Strings have no ISA compare: clean error.
fn nara_compare(
    e: &mut NaraEmit,
    dst: vl_lir::Reg,
    op: LirOp,
    kind: NaraKind,
    l: u8,
    r: u8,
    span: Span,
) {
    if !kind.is_integer() {
        e.diags.push(
            Diagnostic::error(format!(
                "Naravm backend does not support `{op}` on `{kind:?}` yet"
            ))
            .with_label(span, "unsupported operation")
            .with_note("order comparisons lower for u64/i64/u8; f64 and Strings are rejected")
            .with_code("E404"),
        );
        e.invalid.insert(dst);
        return;
    }
    // Normalize to (ltu a b) optionally xored with 1:
    // Lt(a,b)=ltu(a,b); Gt(a,b)=ltu(b,a); Le=not Gt; Ge=not Lt.
    let (a, b, negate) = match op {
        LirOp::Lt => (l, r, false),
        LirOp::Gt => (r, l, false),
        LirOp::Le => (r, l, true),
        LirOp::Ge => (l, r, true),
        _ => unreachable!("ordering only"),
    };
    if kind == NaraKind::I64 {
        let (Some(bias), Some(d)) = (e.ensure_bias(span), e.fresh_rv(span)) else {
            e.invalid.insert(dst);
            return;
        };
        let (Some(ta), Some(tb)) = (e.fresh_rv(span), e.fresh_rv(span)) else {
            e.invalid.insert(dst);
            return;
        };
        e.rv_map.insert(dst, d);
        e.kinds.insert(dst, NaraKind::Bool);
        e.bytecode.extend_from_slice(&[0x12, ta, a, bias]); // xor sign
        e.bytecode.extend_from_slice(&[0x12, tb, b, bias]);
        e.bytecode.extend_from_slice(&[0x10, d, ta, tb]); // ltu
        e.free_rv.push(ta);
        e.free_rv.push(tb);
        if negate {
            let Some(one) = e.ensure_one(span) else {
                e.invalid.insert(dst);
                return;
            };
            e.bytecode.extend_from_slice(&[0x12, d, d, one]);
        }
        return;
    }
    let Some(d) = e.fresh_rv(span) else {
        e.invalid.insert(dst);
        return;
    };
    e.rv_map.insert(dst, d);
    e.kinds.insert(dst, NaraKind::Bool);
    e.bytecode.extend_from_slice(&[0x10, d, a, b]); // ltu
    if negate {
        let Some(one) = e.ensure_one(span) else {
            e.invalid.insert(dst);
            return;
        };
        e.bytecode.extend_from_slice(&[0x12, d, d, one]);
    }
}

fn serialize_nara(
    values: &[u64],
    blob: &[u8],
    constants: &[NaraConstant],
    functions: &[(usize, Vec<u8>)],
    module_idx: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"nara");
    put_u16(&mut out, 0);
    put_u16(&mut out, 2);
    put_u32(&mut out, module_idx as u32);
    put_u32(&mut out, values.len() as u32);
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
    put_u32(&mut out, blob.len() as u32);
    out.extend_from_slice(blob);
    pad4(&mut out);
    put_u32(&mut out, constants.len() as u32);
    for constant in constants {
        out.push(match constant {
            NaraConstant::Value { .. } => 1,
            NaraConstant::String { .. } => 2,
            NaraConstant::Function { .. } => 3,
        });
    }
    pad4(&mut out);
    for constant in constants {
        match constant {
            NaraConstant::Value { value_idx } => {
                put_u32(&mut out, *value_idx as u32);
            }
            NaraConstant::String { offset, len } => {
                put_u32(&mut out, *offset as u32);
                put_u32(&mut out, *len as u32);
            }
            NaraConstant::Function { module, function } => {
                put_u32(&mut out, *module as u32);
                put_u32(&mut out, *function as u32);
            }
        }
    }
    put_u32(&mut out, functions.len() as u32);
    for (name, code) in functions {
        put_u32(&mut out, *name as u32);
        put_u32(&mut out, code.len() as u32);
        out.extend_from_slice(code);
        pad4(&mut out);
    }
    out
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn pad4(out: &mut Vec<u8>) {
    while (out.len() & 3) != 0 {
        out.push(0xff);
    }
}

/// Out-of-range register ids would be a compiler bug; surface it loudly.
pub fn reg_oob(span: Span, reg: u32) -> Diagnostic {
    Diagnostic::error(format!(
        "codegen: register %{reg} out of range (compiler bug)"
    ))
    .with_label(span, "emitted here")
    .with_code("E500")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_emitter() -> NaraEmit {
        NaraEmit {
            blob: Vec::new(),
            values: Vec::new(),
            value_index: std::collections::HashMap::new(),
            constants: Vec::new(),
            string_eq_fn_idx: None,
            bytecode: Vec::new(),
            diags: Vec::new(),
            rv_map: std::collections::HashMap::new(),
            rf_map: std::collections::HashMap::new(),
            kinds: std::collections::HashMap::new(),
            invalid: std::collections::HashSet::new(),
            next_rv: 0,
            next_rf: 0x20,
            free_rv: Vec::new(),
            free_rf: Vec::new(),
            last_use: std::collections::HashMap::new(),
            label_pos: std::collections::HashMap::new(),
            patches: Vec::new(),
            one_rv: None,
            bias_rv: None,
            zero_rv: None,
            param_vi: 0,
            param_ri: 0,
        }
    }

    fn emit_test_instr(e: &mut NaraEmit, ins: &Instr) {
        let func = vl_lir::Function {
            name: "test".into(),
            param_tys: Vec::new(),
            ret: vl_typecheck::Ty::Void,
            instrs: Vec::new(),
        };
        let sigs = std::collections::HashMap::new();
        let fn_consts = std::collections::HashMap::new();
        let imported_fn_consts = std::collections::HashMap::new();
        let imports = std::collections::HashMap::new();
        let objects = std::collections::HashMap::new();
        let global_slots = std::collections::HashMap::new();
        let global_tys = std::collections::HashMap::new();
        let ctx = NaraFnCtx {
            func: &func,
            is_main: false,
            sigs: &sigs,
            fn_consts: &fn_consts,
            imported_fn_consts: &imported_fn_consts,
            imports: &imports,
            module: "test",
            objects: &objects,
            print_fn_idx: 0,
            print_u64_fn_idx: 0,
            global_slots: &global_slots,
            global_tys: &global_tys,
            err_lanes: (0, 0),
        };
        nara_instr(e, ins, &ctx);
    }

    #[test]
    fn naravm_array_header_and_scalar_index_guard_use_separate_slot_zero() {
        let span = Span::empty(0);
        let len = vl_lir::Reg(0);
        let array = vl_lir::Reg(1);
        let mut e = empty_emitter();
        e.rv_map.insert(len, 7);
        e.kinds.insert(len, NaraKind::U64);
        e.last_use.insert(array, 1);
        emit_test_instr(
            &mut e,
            &Instr::NewArray {
                dst: array,
                len,
                elem: vl_typecheck::Ty::U64,
                span,
            },
        );
        let array_rf = e.rf_map[&array];
        assert!(e
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x2d && i[1] == array_rf && i[2] == 0 && i[3] == 7));
        let add = e
            .bytecode
            .iter()
            .position(|op| *op == 0x30)
            .expect("length + 1");
        let shifted_len = e.bytecode[add + 1];
        assert!(e
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x10 && i[2] == shifted_len && i[3] == 7));
        assert!(e
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x24 && i[2] == 0 && i[3] == 3));

        let index = vl_lir::Reg(2);
        let dst = vl_lir::Reg(3);
        let mut e = empty_emitter();
        e.rf_map.insert(array, array_rf);
        e.kinds
            .insert(array, NaraKind::Array(Box::new(NaraKind::U64)));
        e.rv_map.insert(index, 8);
        e.kinds.insert(index, NaraKind::U64);
        e.last_use.insert(dst, 1);
        emit_test_instr(
            &mut e,
            &Instr::ArrayGet {
                dst,
                array,
                index,
                elem: vl_typecheck::Ty::U64,
                span,
            },
        );
        let add = e
            .bytecode
            .iter()
            .position(|op| *op == 0x30)
            .expect("index + 1");
        let shifted_index = e.bytecode[add + 1];
        assert_eq!(e.bytecode[add + 2], 8);
        assert!(e
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x10 && i[2] == shifted_index && i[3] == 8));
        assert!(e
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x24 && i[2] == 0 && i[3] == 3));
        assert!(e
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x28 && i[2] == array_rf && i[3] == shifted_index));
    }

    #[test]
    fn naravm_array_literal_immediate_limits_follow_element_lane() {
        let span = Span::empty(0);
        let elem = vl_lir::Reg(0);
        let dst = vl_lir::Reg(1);
        let mut scalar = empty_emitter();
        scalar.rv_map.insert(elem, 6);
        scalar.kinds.insert(elem, NaraKind::U64);
        scalar.last_use.insert(dst, 1);
        emit_test_instr(
            &mut scalar,
            &Instr::ArrayLit {
                dst,
                elems: vec![elem; 254],
                elem: vl_typecheck::Ty::U64,
                span,
            },
        );
        assert!(scalar
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x27 && i[2] == 255 && i[3] == 0));

        let mut scalar_large = empty_emitter();
        scalar_large.rv_map.insert(elem, 6);
        scalar_large.kinds.insert(elem, NaraKind::U64);
        scalar_large.last_use.insert(dst, 1);
        emit_test_instr(
            &mut scalar_large,
            &Instr::ArrayLit {
                dst,
                elems: vec![elem; 255],
                elem: vl_typecheck::Ty::U64,
                span,
            },
        );
        let create = scalar_large
            .bytecode
            .windows(4)
            .position(|i| i[0] == 0x26)
            .expect("dynamic scalar allocation");
        let dynamic_index = scalar_large.bytecode[create + 2];
        assert!(scalar_large.free_rv.contains(&dynamic_index));

        let mut refs = empty_emitter();
        refs.rf_map.insert(elem, 0x24);
        refs.kinds.insert(elem, NaraKind::String);
        refs.last_use.insert(dst, 1);
        emit_test_instr(
            &mut refs,
            &Instr::ArrayLit {
                dst,
                elems: vec![elem; 255],
                elem: vl_typecheck::Ty::String,
                span,
            },
        );
        assert!(refs
            .bytecode
            .windows(4)
            .any(|i| i[0] == 0x27 && i[2] == 1 && i[3] == 255));

        let mut refs_large = empty_emitter();
        refs_large.rf_map.insert(elem, 0x24);
        refs_large.kinds.insert(elem, NaraKind::String);
        refs_large.last_use.insert(dst, 1);
        emit_test_instr(
            &mut refs_large,
            &Instr::ArrayLit {
                dst,
                elems: vec![elem; 256],
                elem: vl_typecheck::Ty::String,
                span,
            },
        );
        let create = refs_large
            .bytecode
            .windows(4)
            .position(|i| i[0] == 0x26)
            .expect("dynamic reference allocation");
        let dynamic_index = refs_large.bytecode[create + 3];
        assert!(refs_large.free_rv.contains(&dynamic_index));
    }

    fn lir_of(src: &str) -> LirProgram {
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, _) = vl_semantic::resolve(&prog);
        let hir = vl_hir::lower(&prog, &res);
        let (typed, _) = vl_typecheck::check(&hir);
        vl_lir::lower(&hir, &typed)
    }

    #[test]
    fn string_equality_reuses_native_constant_and_preserves_reference_arguments() {
        let span = Span::empty(0);
        let lhs = vl_lir::Reg(0);
        let rhs = vl_lir::Reg(1);
        let dst = vl_lir::Reg(2);
        let mut e = empty_emitter();
        for _ in 0..2 {
            e.reset_fn(std::collections::HashMap::from([(dst, 1)]));
            e.rf_map.insert(lhs, 0x32);
            e.rf_map.insert(rhs, 0x33);
            e.kinds.insert(lhs, NaraKind::String);
            e.kinds.insert(rhs, NaraKind::String);
            emit_test_instr(
                &mut e,
                &Instr::BinOp {
                    dst,
                    op: LirOp::Eq,
                    lhs,
                    rhs,
                    span,
                },
            );
            assert!(e.diags.is_empty(), "{:?}", e.diags);
            assert_eq!(e.constants.len(), 3, "one shared native function constant");
            assert_eq!(e.kinds[&dst], NaraKind::Bool);
            assert!(
                e.bytecode.windows(2).any(|i| i == [0x08, 0x32]),
                "save rf32 before argument moves"
            );
            assert!(
                e.bytecode.windows(2).any(|i| i == [0x09, 0x32]),
                "restore live rf32 after comparison"
            );
        }
    }

    #[test]
    fn cached_constants_reload_at_every_control_flow_use() {
        let mut e = empty_emitter();
        for ensure in [
            NaraEmit::ensure_one,
            NaraEmit::ensure_zero,
            NaraEmit::ensure_bias,
        ] {
            let first = ensure(&mut e, Span::new(0, 0)).expect("constant register");
            let before = e.bytecode.len();
            let second = ensure(&mut e, Span::new(0, 0)).expect("cached register");
            assert_eq!(first, second);
            assert_eq!(&e.bytecode[before..before + 2], &[0x02, second]);
        }
    }

    #[test]
    fn loop_local_temporaries_are_recreated_without_exhausting_registers() {
        let body = "total = total + 1u64; ".repeat(40);
        let src = format!(
            "fun count(n: u64): u64 {{ var total = 0u64; var i = 0u64; while (i < n) {{ {body} i = i + 1u64; }} return total; }} fun main() {{ val n = count(3u64); n; }}"
        );
        let lir = lir_of(&src);
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(artifact.is_some());
        let count = lir
            .functions
            .iter()
            .find(|f| f.name == "count")
            .expect("count");
        let last = nara_last_use(count);
        let header = count
            .instrs
            .iter()
            .position(|i| matches!(i, Instr::Label { .. }))
            .expect("loop header");
        let back_edge = count
            .instrs
            .iter()
            .rposition(|i| matches!(i, Instr::Jump { .. }))
            .expect("back edge");
        let constant = count.instrs[header + 1..back_edge]
            .iter()
            .find_map(|i| match i {
                Instr::Const { dst, .. } => Some(dst),
                _ => None,
            })
            .expect("loop-local constant");
        assert!(
            last[constant] < back_edge,
            "loop-local constant dies before back edge"
        );
    }

    #[test]
    fn loop_invariant_registers_stay_live_across_back_edges() {
        // Regression test: the textual last use of `n` is the loop
        // condition, but its machine register must survive the back edge.
        // Freeing it mid-loop used to let a temporary clobber it, hanging
        // `sum` forever.
        let lir = lir_of("fun sum(a: Array[u64], n: u64): u64 { var t = 0u64; var i = 0u64; while (i < n) { t = t + a[i]; i = i + 1u64; } return t; } fun main() {}");
        let f = lir.functions.iter().find(|f| f.name == "sum").unwrap();
        let uses = nara_last_use(f);
        let n = match f.instrs[1] {
            Instr::Param { dst, .. } => dst,
            ref other => panic!("expected param, got {other:?}"),
        };
        let back_edge = f
            .instrs
            .iter()
            .rposition(|i| matches!(i, Instr::Jump { .. }))
            .expect("loop back edge");
        assert!(
            uses[&n] >= back_edge,
            "invariant must outlive the loop: last={} back_edge={}",
            uses[&n],
            back_edge
        );
    }

    #[test]
    fn naravm_emits_tuples_with_container_ops_and_value_copy() {
        // Unnamed + named literals, reads, writes, destructure, and a
        // rebinding copy (which must deep-copy the container, hence two
        // `createi` allocations for one literal shape).
        let lir = lir_of(
            "use std; fun main() { val t = #(1u64, \"a\"); val a = t.`0; val u = #(x = 1u64, y = 2u64); val x = u.x; var m = #(1u64, 2u64); m.`0 = 3u64; val #(p, q) = t; var b = m; b.`1 = 9u64; std.print_u64(a + p + m.`1 + b.`1); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // createi = 0x27 (literal + copy allocations), getvati = 0x2c,
        // setvati = 0x2d.
        for op in [0x27u8, 0x2c, 0x2du8] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_emits_fallible_with_container_ops() {
        // `E!u64` lowers to tag+payload containers: `createi` (0x27)
        // allocates, `getvati` (0x2c) reads the tag/payload/code,
        // `setvati` (0x2d) writes them. The error code rides the blob.
        let lir = lir_of(
            "type E = error { A, }; fun f(): E!u64 { return E.A; } fun g(): E!u64 { val x = try f(); return x; } fun main() { val v = g() catch 0u64; v; }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        for op in [0x27u8, 0x2c, 0x2du8] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_emits_fallible_string_and_void_payloads() {
        let lir = lir_of(
            "use std; type E = error { A, }; fun s(): E!String { return E.A; } fun t(): E!String { return \"hi\"; } fun v(): E!void { return; } fun main() { std.print(s() catch \"d\"); std.print(t() catch \"e\"); v() catch std.print(\"f\"); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // Reference payloads add `getrfati` (0x2e) / `setrfati` (0x2f).
        for op in [0x27u8, 0x2cu8, 0x2du8, 0x2eu8, 0x2fu8] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_emits_nested_tuples() {
        let lir = lir_of(
            "fun main() { var t: *#(u64, #(u64, u64)) = #(1u64, #(2u64, 3u64)); t.`1 = #(4u64, 5u64); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        for op in [0x27u8, 0x2c, 0x2d] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_emits_arrays_with_container_ops() {
        let lir = lir_of(
            "use std; fun get(a: Array[u64]): u64 { return a[0u64]; } fun main() { var a = Array.new::[u64](2u64); a[0u64] = 1u64; a[1u64] = 2u64; val b = [3u64, 4u64]; std.print_u64(get(a) + b[1u64]); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // create = 0x26 (Array.new), createi = 0x27 (literal),
        // getvat = 0x28 (reads), setvat/setvati = 0x29/0x2D (writes).
        for op in [0x26u8, 0x27, 0x28, 0x2du8] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_recycles_registers_across_long_global_initializers() {
        let lir = lir_of(
            "val g = 1u64 + 2u64 + 3u64 + 4u64 + 5u64 + 6u64 + 7u64 + 8u64 + 9u64 + 10u64 + 11u64 + 12u64 + 13u64 + 14u64 + 15u64 + 16u64 + 17u64 + 18u64 + 19u64 + 20u64; fun main() {}",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn naravm_preserves_library_initializers_without_claiming_entrypoint() {
        let lir = lir_of("val g = 7u64; fun read(): u64 { return g; }");
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert!(bytes
            .windows(b"<module-init>".len())
            .any(|w| w == b"<module-init>"));
        assert!(
            bytes.contains(&0x27),
            "no module-state allocation in {bytes:?}"
        );
    }

    #[test]
    fn naravm_keeps_main_callable_and_adds_one_designated_entrypoint() {
        let mut lir = lir_of("fun main() { }");
        lir.entrypoint = true;
        lir.entrypoint_module = Some(lir.module.clone());
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(
            bytes
                .windows(b"<entrypoint>".len())
                .filter(|w| *w == b"<entrypoint>")
                .count(),
            1
        );
        assert_eq!(
            bytes
                .windows(b"main".len())
                .filter(|w| *w == b"main")
                .count(),
            1
        );
    }

    #[test]
    fn naravm_passes_arrays_through_calls() {
        let lir = lir_of(
            "fun fill(a: Array[u64]): Array[u64] { a[0u64] = 7u64; return a; } fun main() { val a = fill(Array.new::[u64](1u64)); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn naravm_emits_string_arrays_with_ref_ops() {
        let lir = lir_of(
            "fun main() { var a = Array.new::[String](2u64); a[0u64] = \"hi\"; val x = a[0u64]; x; }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // create = 0x26, setrfat = 0x2B (store), getrfat = 0x2A (load).
        for op in [0x26u8, 0x2b, 0x2a] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_emits_objects_with_mixed_lane_field_ops() {
        let lir = lir_of(
            "use std; type Counter = object { value: u64, label: String, }; fun main() { var c = Counter { value = 1, label = \"count\" }; c.value = 2; std.print(c.label); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // createi = 0x27, getrfati = 0x2E, setvati/setrfati = 0x2D/0x2F.
        for op in [0x27u8, 0x2d, 0x2e, 0x2f] {
            assert!(bytes.contains(&op), "no {op:#x} in {bytes:?}");
        }
    }

    #[test]
    fn naravm_rejects_missing_object_fields_in_lir() {
        let lir = LirProgram {
            module: "t".into(),
            entrypoint: false,
            entrypoint_module: None,
            objects: vec![vl_lir::ObjectDef {
                name: "Counter".into(),
                fields: vec![("value".into(), vl_typecheck::Ty::U64)],
            }],
            globals: vec![],
            imports: vec![],
            err_lanes: (0, 0),
            functions: vec![
                vl_lir::Function {
                    name: "read".into(),
                    param_tys: vec![vl_typecheck::Ty::Object(Box::new(vl_typecheck::ObjectTy {
                        name: "Counter".into(),
                        args: vec![],
                    }))],
                    ret: vl_typecheck::Ty::U64,
                    instrs: vec![
                        Instr::Param {
                            dst: vl_lir::Reg(0),
                            index: 0,
                            span: Span::empty(0),
                        },
                        Instr::ObjectGet {
                            dst: vl_lir::Reg(1),
                            object: vl_lir::Reg(0),
                            name: "missing".into(),
                            ty: vl_typecheck::Ty::U64,
                            span: Span::empty(0),
                        },
                        Instr::Ret {
                            src: vl_lir::Reg(1),
                            span: Span::empty(0),
                        },
                    ],
                },
                vl_lir::Function {
                    name: "main".into(),
                    param_tys: vec![],
                    ret: vl_typecheck::Ty::Void,
                    instrs: vec![],
                },
            ],
        };
        let (_, diags) = NaraVmTarget.emit(&lir);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E500")),
            "{diags:?}"
        );
    }

    #[test]
    fn naravm_emits_monomorphized_instances() {
        let lir = lir_of(
            "fun id[T](x: T): T { return x; } fun main() { val a = id(1u64); val b = id::[String](\"s\"); a; b; }",
        );
        let names: Vec<&str> = lir.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"id$u64"), "{names:?}");
        assert!(names.contains(&"id$String"), "{names:?}");
        // The template itself never emits.
        assert!(!names.contains(&"id"), "{names:?}");
        let dump = lir.dump();
        assert!(dump.contains("call id$u64"), "{dump}");
        assert!(dump.contains("call id$String"), "{dump}");
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn countdown_while_emits_runnable_naravm() {
        let lir = lir_of(
            "use std; fun main() { var i = 3u64; while (i > 0u64) { std.print_u64(i); i = i - 1u64; } }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // A loop back-edge plus at least one conditional jump must be present.
        // jmp = 0x22, jz = 0x24.
        assert!(bytes.contains(&0x22), "no jmp in {bytes:?}");
        assert!(bytes.contains(&0x24), "no jz in {bytes:?}");
    }

    #[test]
    fn naravm_supports_signed_ordering_and_logic() {
        for src in [
            "fun main() { val a = 0 - 5; if (a < 3) { a; } }",
            "fun main() { if (true && !false) { 1; } }",
            "fun main() { var i = 0; while (i < 3) { i = i + 1; if (i == 2) { continue; } } }",
        ] {
            let lir = lir_of(src);
            let (artifact, diags) = NaraVmTarget.emit(&lir);
            assert!(diags.is_empty(), "{src}: {diags:?}");
            assert!(artifact.is_some());
        }
    }

    #[test]
    fn naravm_rejects_float_ordering_with_e404() {
        let lir = lir_of("fun main() { if (1.5f64 < 2.5f64) { 1; } }");
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(artifact.is_none());
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E404")),
            "{diags:?}"
        );
    }

    #[test]
    fn naravm_emits_user_calls_with_mixed_params_string_return_and_recursion() {
        let lir = lir_of(
            r#"
use std;
fun add(a: u64, b: u64): u64 { return a + b; }
fun greet(name: String, n: u64): String { return name; }
fun fact(n: u64): u64 {
    var r = 1u64;
    if (n == 0u64) { r; } else { r = n * fact(n - 1u64); }
    return r;
}
fun main() {
    std.print(greet("hi\n", 1u64));
    std.print_u64(add(fact(3u64), 1u64));
}
"#,
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // calli = 0x20: main -> greet/add/fact plus the recursive fact call.
        assert!(bytes.contains(&0x20), "no calli in {bytes:?}");
    }

    #[test]
    fn naravm_rejects_unknown_callee_with_e404() {
        use vl_common::Span;
        use vl_lir::{Function, Instr, LirProgram, Reg};
        let lir = LirProgram {
            module: "t".into(),
            entrypoint: false,
            entrypoint_module: None,
            objects: vec![],
            globals: vec![],
            imports: vec![],
            err_lanes: (0, 0),
            functions: vec![Function {
                name: "main".into(),
                param_tys: vec![],
                ret: vl_typecheck::Ty::Void,
                instrs: vec![
                    Instr::Const {
                        dst: Reg(0),
                        value: vl_common::Scalar::U64(1),
                        span: Span::empty(0),
                    },
                    Instr::Call {
                        dst: Reg(1),
                        callee: vl_lir::FunctionRef {
                            module: "t".into(),
                            function: "nope".into(),
                        },
                        args: vec![Reg(0)],
                        span: Span::empty(0),
                    },
                    Instr::Ret {
                        src: Reg(1),
                        span: Span::empty(0),
                    },
                ],
            }],
        };
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(artifact.is_none());
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E404")),
            "{diags:?}"
        );
    }

    #[test]
    fn unknown_target_is_none() {
        assert!(lookup("x86-64").is_none());
    }

    #[test]
    fn naravm_emits_checked_string_and_fs_calls_with_status_dispatch() {
        // `byte_at` exercises the value-result arm, `slice`/`read_file`
        // the reference-result arm. Lower against the authoritative
        // catalog (the semantic default is a minimal unit-test surface).
        fn lir_of_checked(src: &str) -> LirProgram {
            let catalog = modules();
            let (toks, _) = vl_lex::lex(src);
            let (prog, _) = vl_syntax::parse(&toks, src);
            let (res, _) = vl_semantic::resolve_with_modules(&prog, &catalog);
            let hir = vl_hir::lower(&prog, &res);
            let (typed, _) = vl_typecheck::check_with_modules(&hir, &catalog);
            vl_lir::lower(&hir, &typed)
        }
        // `byte_at` exercises the value-result arm, `slice`/`read_file`
        // the reference-result arm.
        for src in [
            "use std.string; fun main() { val b = string.byte_at(\"hi\", 0u64) catch 0u8; b; }",
            "use std.string; fun main() { val s = string.slice(\"hi\", 0u64, 1u64) catch \"x\"; s; }",
            "use std.fs; fun main() { val s = fs.read_file(\"hi\") catch \"x\"; s; }",
        ] {
            let lir = lir_of_checked(src);
            let (artifact, diags) = NaraVmTarget.emit(&lir);
            assert!(diags.is_empty(), "{src}: {diags:?}");
            let bytes = artifact.unwrap().bytes.unwrap();
            assert_eq!(&bytes[..4], b"nara");
            // calli, status-branch (jz), containers (createi/setvati).
            for op in [0x20u8, 0x24, 0x27, 0x2d] {
                assert!(bytes.contains(&op), "{src}: no {op:#x}");
            }
        }
        let bytes = NaraVmTarget
            .emit(&lir_of_checked(
                "use std.string; fun main() { val b = string.byte_at(\"hi\", 0u64) catch 0u8; b; }",
            ))
            .0
            .unwrap()
            .bytes
            .unwrap();
        assert!(
            bytes
                .windows(b"std::string".len())
                .any(|w| w == b"std::string"),
            "string native module missing"
        );
        let bytes = NaraVmTarget
            .emit(&lir_of_checked(
                "use std.fs; fun main() { val s = fs.read_file(\"hi\") catch \"x\"; s; }",
            ))
            .0
            .unwrap()
            .bytes
            .unwrap();
        assert!(
            bytes.windows(b"std::fs".len()).any(|w| w == b"std::fs"),
            "fs native module missing"
        );
    }

    #[test]
    fn constant_loads_choose_width_and_encode_big_endian_indices() {
        let mut e = empty_emitter();
        for (idx, value, reference) in [
            (255, vec![0x02, 1, 0xff], vec![0x03, 0x21, 0xff]),
            (256, vec![0x0c, 1, 1, 0], vec![0x0d, 0x21, 1, 0]),
            (
                0x1234,
                vec![0x0c, 1, 0x12, 0x34],
                vec![0x0d, 0x21, 0x12, 0x34],
            ),
            (
                65535,
                vec![0x0c, 1, 0xff, 0xff],
                vec![0x0d, 0x21, 0xff, 0xff],
            ),
        ] {
            e.bytecode.clear();
            e.load_constant(false, 1, idx);
            e.load_constant(true, 0x21, idx);
            assert_eq!(e.bytecode, [value, reference].concat());
        }
    }

    #[test]
    fn constant_pool_accepts_u16_max_and_rejects_overflow() {
        for kind in 0..3 {
            let mut e = empty_emitter();
            e.constants
                .resize_with(u16::MAX as usize, || NaraConstant::Value { value_idx: 0 });
            let add = |e: &mut NaraEmit| match kind {
                0 => e.add_string(b"s", Span::empty(0)),
                1 => e.add_value(e.values.len() as u64, Span::empty(0)),
                _ => nara_push_fn_const(e, 0, 0),
            };
            assert_eq!(add(&mut e), Some(65535));
            assert_eq!(add(&mut e), None);
            assert!(e.diags.iter().any(|d| d.code.as_deref() == Some("E405")));
        }
    }

    #[test]
    fn naravm_emits_more_than_256_constants() {
        let mut src = String::from("fun main() {");
        for i in 0..300 {
            src.push_str(&format!("std.print(\"s{i}\"); std.print_u64({i});"));
        }
        src.push('}');
        let (artifact, diags) = NaraVmTarget.emit(&lir_of(&src));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(artifact.is_some());
    }

    #[test]
    fn naravm_accepts_bare_print_from_single_export_use() {
        let src = "use std.print; fun main() { print(\"hi\\n\"); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve_with_modules(&prog, &modules());
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = vl_hir::lower(&prog, &res);
        let (typed, tdiags) = vl_typecheck::check(&hir);
        assert!(tdiags.is_empty(), "{tdiags:?}");
        let lir = vl_lir::lower(&hir, &typed);
        assert!(lir.dump().contains("call std::print"), "{}", lir.dump());
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn naravm_emits_println_as_print_plus_newline() {
        let lir = lir_of("use std; fun main() { std.println(\"hi\"); }");
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert_eq!(&bytes[..4], b"nara");
        // One `println` lowers to two `print` calls (value + "\n"),
        // so at least two calli (0x20) must be present.
        let callis = bytes.iter().filter(|b| **b == 0x20).count();
        assert!(callis >= 2, "expected two print calls, got {callis}");
    }

    #[test]
    fn naravm_accepts_bare_println_from_single_export_use() {
        let src = "use std.println; fun main() { println(\"hi\"); }";
        let (toks, _) = vl_lex::lex(src);
        let (prog, _) = vl_syntax::parse(&toks, src);
        let (res, rdiags) = vl_semantic::resolve_with_modules(&prog, &modules());
        assert!(rdiags.iter().all(|d| !d.is_error()), "{rdiags:?}");
        let hir = vl_hir::lower(&prog, &res);
        let (typed, tdiags) = vl_typecheck::check(&hir);
        assert!(tdiags.is_empty(), "{tdiags:?}");
        let lir = vl_lir::lower(&hir, &typed);
        assert!(lir.dump().contains("call std::println"), "{}", lir.dump());
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(&artifact.unwrap().bytes.unwrap()[..4], b"nara");
    }

    #[test]
    fn target_module_catalogs_are_specific() {
        assert!(modules_for_target("naravm")
            .iter()
            .any(|m| m.path.as_string() == "std"));
        // `std.fs.read_file` is checked like TCP, so it stays emittable.
        assert!(modules_for_target("naravm")
            .iter()
            .any(|m| m.path.as_string() == "std.fs"));
        assert!(modules_for_target("unknown")
            .iter()
            .any(|m| m.path.as_string() == "std.fs"));
    }

    #[test]
    fn mutable_and_readonly_share_runtime_abi() {
        use vl_typecheck::Ty;
        // `Foo` and `*Foo` lower to the same register class and container ops.
        let ro = NaraKind::of_ty(&Ty::Object(Box::new(vl_typecheck::ObjectTy {
            name: "Foo".into(),
            args: vec![],
        })))
        .expect("object kind");
        let mu = NaraKind::of_ty(&Ty::Mutable(Box::new(Ty::Object(Box::new(
            vl_typecheck::ObjectTy {
                name: "Foo".into(),
                args: vec![],
            },
        )))))
        .expect("mutable kind");
        assert_eq!(ro, mu);
        assert!(ro.is_ref());
        let ro_arr = NaraKind::of_ty(&Ty::Array(Box::new(Ty::U64))).expect("array kind");
        let mu_arr = NaraKind::of_ty(&Ty::Mutable(Box::new(Ty::Array(Box::new(Ty::U64)))))
            .expect("mutable array kind");
        assert_eq!(ro_arr, mu_arr);
    }

    #[test]
    fn extern_signatures_grant_no_hidden_mutation() {
        // Language-visible externs stay read-only; mutation authority comes
        // only from `*` in VL source, never inferred from VM internals.
        for m in modules() {
            for e in &m.exports {
                assert!(
                    vl_common::VlType::mutable_wellformed_error(&e.sig.ret).is_none()
                        || matches!(e.sig.ret, vl_common::VlType::Mutable(_)),
                    "extern {}.{} ret must be well-formed",
                    m.path.as_string(),
                    e.name
                );
                for p in &e.sig.params {
                    // No extern param is mutable today (no mutable string/file ops).
                    assert!(
                        !p.ty.is_mutable_view(),
                        "extern {}.{} param {} must not be mutable",
                        m.path.as_string(),
                        e.name,
                        p.name
                    );
                }
                // No extern return is mutable today either.
                assert!(
                    !e.sig.ret.is_mutable_view(),
                    "extern {}.{} return must not be mutable",
                    m.path.as_string(),
                    e.name
                );
            }
        }
    }

    #[test]
    fn object_and_array_mutation_via_mutable_params() {
        let lir = lir_of(
            "type Foo = object { value: u64, }; fun bump(c: *Foo) { c.value = 1u64; } fun fill(a: *Array[u64]) { a[0u64] = 1u64; } fun main() { val c: *Foo = Foo { value = 1u64 }; bump(c); }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        // setvati/setrfati for field/element writes plus container ops.
        assert!(bytes.contains(&0x2d) || bytes.contains(&0x2f), "{bytes:?}");
    }

    #[test]
    fn shared_mutable_global_across_functions() {
        let lir = lir_of(
            "type Foo = object { value: u64, }; val g: *Foo = Foo { value = 1u64 }; fun bump() { g.value = 2u64; } fun read(): u64 { return g.value; } fun main() { bump(); val x = read(); x; }",
        );
        // Both functions load the same stable global ID.
        let loads: Vec<u32> = lir
            .functions
            .iter()
            .flat_map(|f| {
                f.instrs.iter().filter_map(|i| match i {
                    Instr::GlobalLoad { global, .. } => Some(*global),
                    _ => None,
                })
            })
            .collect();
        assert!(!loads.is_empty(), "{}", lir.dump());
        // Plus initializer loads (if any) share the same IDs.
        let dump = lir.dump();
        assert!(dump.contains("global_load"), "{dump}");
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        // Module container + global access ops present.
        assert!(bytes.contains(&0x27), "no createi in {bytes:?}"); // createi
        assert!(
            bytes.contains(&0x2e) || bytes.contains(&0x2c),
            "no global load in {bytes:?}"
        );
    }

    #[test]
    fn global_rebinding_value_and_ref() {
        let lir = lir_of(
            "var n = 1u64; type Foo = object { value: u64, }; var g: *Foo = Foo { value = 1u64 }; fun main() { n = 2u64; g = Foo { value = 3u64 }; n; g; }",
        );
        let stores = lir
            .functions
            .iter()
            .flat_map(|f| {
                f.instrs.iter().filter_map(|i| match i {
                    Instr::GlobalStore { global, .. } => Some(*global),
                    _ => None,
                })
            })
            .count();
        assert_eq!(stores, 2, "{}", lir.dump());
        let (_artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn nested_calls_and_recursion_preserve_module_state() {
        // Reserved rf3F never allocated for temps; recursion + nested calls
        // keep globals working (verified by successful emission + calli).
        let lir = lir_of(
            "val n = 0u64; fun inner(): u64 { return n; } fun outer(): u64 { return inner() + inner(); } fun fact(n: u64): u64 { if (n == 0u64) { return 1u64; } return n * fact(n - 1u64); } fun main() { val a = outer(); val b = fact(3u64); a + b; }",
        );
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        let bytes = artifact.unwrap().bytes.unwrap();
        assert!(bytes.contains(&0x20), "no calli in {bytes:?}");
        assert!(bytes.contains(&0x27), "no module container in {bytes:?}");
    }

    #[test]
    fn capability_leak_is_internal_e500() {
        use vl_common::Span;
        use vl_lir::{Function, Global, LirProgram, Reg};
        let lir = LirProgram {
            module: "t".into(),
            entrypoint: false,
            entrypoint_module: None,
            objects: vec![],
            globals: vec![Global {
                id: 0,
                name: "g".into(),
                ty: vl_typecheck::Ty::Mutable(Box::new(vl_typecheck::Ty::Object(Box::new(
                    vl_typecheck::ObjectTy {
                        name: "Foo".into(),
                        args: vec![],
                    },
                )))),
                init: vec![],
                result: Reg(u32::MAX),
                span: Span::empty(0),
            }],
            imports: vec![],
            err_lanes: (0, 0),
            functions: vec![Function {
                name: "main".into(),
                param_tys: vec![],
                ret: vl_typecheck::Ty::Void,
                instrs: vec![],
            }],
        };
        let (_, diags) = NaraVmTarget.emit(&lir);
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E500")),
            "{diags:?}"
        );
    }

    #[test]
    fn tcp_module_declares_fallible_externs_and_error_set() {
        let catalog = modules();
        let tcp = catalog
            .iter()
            .find(|m| m.path.as_string() == "std.net.tcp")
            .expect("std.net.tcp in modules()");
        assert_eq!(
            tcp.export_names().collect::<Vec<_>>(),
            vec!["connect", "listen", "accept", "read", "write", "close"],
        );
        let set = tcp.lookup_error("TcpError").expect("TcpError set");
        assert_eq!(set.qualified, TCP_ERROR_SET);
        assert_eq!(
            set.variants
                .iter()
                .map(|v| v.name.clone())
                .collect::<Vec<_>>(),
            TCP_STATUS_VARIANTS
                .iter()
                .map(|(_, v)| (*v).to_string())
                .collect::<Vec<_>>(),
        );
        assert!(set.variants.iter().all(|v| v.payload.is_empty()));
        // Every export returns the named fallible (never a bare status).
        for export in &tcp.exports {
            match &export.sig.ret {
                vl_common::VlType::Fallible { err, .. } => {
                    assert_eq!(err.as_deref(), Some(TCP_ERROR_SET), "{}", export.name)
                }
                other => panic!("{} returns {other:?}, want TcpError!T", export.name),
            }
        }
        // The naravm target keeps TCP (only `std.fs` is filtered); the
        // broad catalog and the target view agree on the surface.
        let target = modules_for_target("naravm");
        let target_tcp = target
            .iter()
            .find(|m| m.path.as_string() == "std.net.tcp")
            .expect("std.net.tcp emittable on naravm");
        assert_eq!(target_tcp.exports.len(), tcp.exports.len());
    }

    #[test]
    fn tcp_status_codes_map_to_distinct_variant_hashes() {
        // The emitter's status dispatch is a 1-1 loop over this table, so
        // the codes must be unambiguous inputs with unambiguous outputs.
        let mut statuses = std::collections::HashSet::new();
        let mut hashes = std::collections::HashSet::new();
        for (status, variant) in TCP_STATUS_VARIANTS {
            assert!(statuses.insert(status), "duplicate status {status}");
            let hash = vl_hir::error_code(TCP_ERROR_SET, variant);
            assert_ne!(hash, 0, "{variant} must not collide with success");
            assert!(hashes.insert(hash), "duplicate code for {variant}");
        }
    }

    /// Hand-built `call std.net.tcp::connect` (value payload) plus
    /// `call std.net.tcp::read` (tuple payload with a reference lane):
    /// the backend checks `rv10`, wraps ok/err containers, and intern the
    /// native spelling. Real sources cover the rest in `tests/pipeline.rs`.
    fn tcp_test_lir(callee: &str, param_tys: Vec<vl_typecheck::Ty>) -> LirProgram {
        use vl_common::Span;
        use vl_lir::{Function, FunctionImport, FunctionRef, Instr, LirProgram, Reg};
        let ret = modules()
            .into_iter()
            .find(|m| m.path.as_string() == "std.net.tcp")
            .and_then(|m| m.lookup(callee).map(|e| e.sig.ret.clone()))
            .expect("tcp export");
        let ret = vl_typecheck::Ty::from_vl(&ret);
        let symbol = FunctionRef {
            module: "std.net.tcp".into(),
            function: callee.into(),
        };
        let mut instrs = Vec::new();
        let mut args = Vec::new();
        for (i, ty) in param_tys.iter().enumerate() {
            let dst = Reg(i as u32);
            match NaraKind::of_ty(ty).is_some_and(|k| k.is_ref()) {
                true => instrs.push(Instr::StringConst {
                    dst,
                    value: b"h".to_vec(),
                    span: Span::empty(0),
                }),
                false => instrs.push(Instr::Const {
                    dst,
                    value: vl_common::Scalar::U64(1),
                    span: Span::empty(0),
                }),
            }
            args.push(dst);
        }
        let dst = Reg(args.len() as u32);
        instrs.push(Instr::Call {
            dst,
            callee: symbol.clone(),
            args,
            span: Span::empty(0),
        });
        instrs.push(Instr::Ret {
            src: dst,
            span: Span::empty(0),
        });
        LirProgram {
            module: "t".into(),
            entrypoint: false,
            entrypoint_module: None,
            objects: vec![],
            globals: vec![],
            imports: vec![FunctionImport {
                symbol,
                param_tys,
                ret,
            }],
            functions: vec![Function {
                name: "main".into(),
                param_tys: vec![],
                ret: vl_typecheck::Ty::Void,
                instrs,
            }],
            err_lanes: (0, 0),
        }
    }

    #[test]
    fn naravm_emits_checked_tcp_calls_with_status_dispatch() {
        // `connect` exercises the value-payload arm, `read` the tuple arm
        // with a reference element (`setrfati` 0x2f).
        for (callee, params) in [
            (
                "connect",
                vec![vl_typecheck::Ty::String, vl_typecheck::Ty::U64],
            ),
            ("read", vec![vl_typecheck::Ty::U64, vl_typecheck::Ty::U64]),
        ] {
            let lir = tcp_test_lir(callee, params);
            let (artifact, diags) = NaraVmTarget.emit(&lir);
            assert!(diags.is_empty(), "{callee}: {diags:?}");
            let bytes = artifact.unwrap().bytes.unwrap();
            assert_eq!(&bytes[..4], b"nara");
            // calli, status-branch (jz), err-arm joins (jmp), status
            // compares (eq), ok/err containers (createi/setvati).
            for op in [0x20u8, 0x24, 0x22, 0x0a, 0x27, 0x2d] {
                assert!(bytes.contains(&op), "{callee}: no {op:#x} in {bytes:?}");
            }
            // Native spelling (not the dotted VL spelling).
            assert!(
                bytes
                    .windows(b"std::net::tcp".len())
                    .any(|w| w == b"std::net::tcp"),
                "{callee}: native module missing"
            );
            // Every status input and every variant output is interned.
            for status in 1u64..=7 {
                assert!(
                    bytes.windows(8).any(|w| w == status.to_be_bytes()),
                    "{callee}: status {status} missing"
                );
            }
            for (_, variant) in TCP_STATUS_VARIANTS {
                let hash = vl_hir::error_code(TCP_ERROR_SET, variant);
                assert!(
                    bytes.windows(8).any(|w| w == hash.to_be_bytes()),
                    "{callee}: code for {variant} missing"
                );
            }
        }
        // The tuple arm moves a reference result (`setrfati` 0x2f).
        let lir = tcp_test_lir("read", vec![vl_typecheck::Ty::U64, vl_typecheck::Ty::U64]);
        let (artifact, diags) = NaraVmTarget.emit(&lir);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            artifact.unwrap().bytes.unwrap().contains(&0x2f),
            "no setrfati for the read data lane"
        );
    }

    #[test]
    fn fallible_value_lane_includes_error_code_and_payload() {
        assert_eq!(nara_fallible_lanes(&NaraKind::U64, (1, 0)), (3, 0));
        assert_eq!(nara_fallible_lanes(&NaraKind::String, (2, 1)), (4, 1));
    }

    #[test]
    fn call_argument_staging_handles_overlapping_abi_destinations() {
        let mut e = empty_emitter();
        nara_stage_call_args(&mut e, &[(false, 0x12), (false, 0x12), (false, 0x15)]);
        assert_eq!(
            e.bytecode,
            vec![0x06, 0x12, 0x06, 0x12, 0x06, 0x15, 0x07, 0x13, 0x07, 0x12, 0x07, 0x11]
        );
        e.bytecode.clear();
        nara_stage_call_args(
            &mut e,
            &[(false, 0x12), (true, 0x35), (false, 0x13), (true, 0x36)],
        );
        assert_eq!(
            e.bytecode,
            vec![
                0x06, 0x12, 0x08, 0x35, 0x06, 0x13, 0x08, 0x36, 0x09, 0x32, 0x07, 0x12, 0x09, 0x31,
                0x07, 0x11
            ]
        );
    }

    #[test]
    fn native_multi_results_are_staged_before_scratch_can_overlap_them() {
        let mut e = empty_emitter();
        e.next_rv = 0x12;
        let temps =
            nara_stage_native_results(&mut e, &[(false, 0x11), (false, 0x12)], Span::empty(0))
                .expect("two temporary result registers");
        assert_eq!(temps, vec![0x12, 0x13]);
        assert_eq!(
            e.bytecode,
            vec![0x06, 0x11, 0x06, 0x12, 0x07, 0x13, 0x07, 0x12]
        );
    }
}
