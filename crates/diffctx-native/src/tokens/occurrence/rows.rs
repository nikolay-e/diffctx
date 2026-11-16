//! What the outcome of an occurrence reads per language: rows of data, never code.

use crate::facts::{DefKind, LocalKind};
use crate::resolve::CandidateReason;
use crate::vocabulary::family::Family;

/// The locals whose site is only the declaration of the name: no pattern shares it with a
/// field read that has no binding of its own (`let Retry { attempt } = r;` is why Rust's
/// `Variable` is not here; a TypeScript pattern's field read is bound).
pub(super) const DECLARING_LOCALS: &[(Family, LocalKind)] = &[
    (Family::Python, LocalKind::Variable),
    (Family::Python, LocalKind::Closure),
    (Family::TypeScript, LocalKind::Variable),
    (Family::TypeScript, LocalKind::Closure),
];

/// The keyword after which a local's site is the declaration of the name and nothing else
/// (`let total = …;`, `let mut total`): a pattern that is more than a name (`let T { a }`)
/// does not follow it directly.
pub(super) const DECLARING_KEYWORDS: &[(Family, LocalKind, &str)] =
    &[(Family::Rust, LocalKind::Variable, "let")];

/// Families whose extractors state the read of a value at the member after it (`NAME.exec(x)`
/// binds `exec`): the object is that read, not a use of its own.
pub(super) const MEMBER_OBJECTS: &[Family] = &[Family::TypeScript];

/// Families where a call of a bare name is the call of a local of that name (a closure held in
/// a `let`) once the local's statement is over and its block still open.
pub(super) const LOCAL_CALLS: &[Family] = &[Family::Rust];

/// Families whose local site starts where the local comes into force (after its declarator, or
/// at the start of the scope of a parameter) and spans the scope: the name lies before it.
pub(super) const SCOPED_LOCALS: &[Family] = &[Family::Java, Family::Kotlin, Family::Swift];

/// Where a bare name heading a chain (`name.x`) can be shown to be no member, as (family,
/// its locals reach the whole function and not a block, a class member needs a receiver).
pub(super) const BARE_HEADS: &[(Family, bool, bool)] = &[
    (Family::Python, true, true),
    (Family::TypeScript, false, true),
];

/// The keywords that open and close the module path of an import (`from a.b import c`):
/// what lies between them names modules, never a definition.
pub(super) const MODULE_PATHS: &[(Family, &str, &str)] = &[(Family::Python, "from", "import")];

/// The keyword that opens a clause naming the package of the file: the name after it is no use.
pub(super) const PACKAGE_HEADS: &[(Family, &str)] = &[
    (Family::Go, "package"),
    (Family::Java, "package"),
    (Family::Kotlin, "package"),
];

/// What separates a path's segments: a name before it names a module or a type, and so is no
/// use of a function, a member or a value.
pub(super) const PATH_SEPARATORS: &[(Family, &str)] = &[(Family::Rust, "::")];

/// Families that write a keyword argument as `name=value`: the name is a field of the class it
/// builds when the callee is that class.
pub(super) const KEYWORD_ARGUMENTS: &[Family] = &[Family::Python];

/// Families whose types are satisfied by shape: a member of the subject's name may be the one a
/// call through an external type runs, whatever the subject's class declares.
pub(super) const STRUCTURAL_TYPES: &[Family] = &[Family::TypeScript, Family::Go];

/// The candidates a resolver reads off one class's members (overloads cut by the argument
/// count, a member and the overrides below it): they name every callee among that class.
pub(super) const CLASS_CANDIDATES: &[(Family, CandidateReason)] = &[
    (Family::Java, CandidateReason::Overloaded),
    (Family::Java, CandidateReason::Dispatch),
];

/// Commands whose arguments name no function; `true`: they run the program named (`sudo check`).
pub(super) const COMMAND_ARGUMENTS: &[(Family, &str, bool)] = &[
    (Family::Shell, "echo printf cat grep ls cd rm mkdir", false),
    (Family::Shell, "sudo env exec nohup xargs command", true),
];

/// The keyword that opens a block declaring the members of the type it names.
pub(super) const IMPL_HEADS: &[(Family, &str)] = &[(Family::Rust, "impl")];

/// Families whose variable locals declare at the name and are read unbound; `true`: as `$name`.
pub(super) const NAMED_LOCALS: &[(Family, bool)] = &[
    (Family::Rust, false),
    (Family::Go, false),
    (Family::Php, true),
    (Family::C, false),
    (Family::Shell, true),
];

/// Families whose resolver pairs a prototype with its definition: a call binds both, and
/// each is the other's place (the header's declaration is the function itself).
pub(super) const PAIRED_PROTOTYPES: &[Family] = &[Family::C];

/// Families whose module offers every name it imports to its importers, so a renamed import
/// is a name of the subject there too (`from a import f as g`, then `from m import g`).
pub(super) const OFFERING_MODULES: &[Family] = &[Family::Python];

/// The words after which a SQL name is declared or names an object of another namespace, never
/// called (the extractor reads no fact there), for a subject of the kinds listed: `DROP FUNCTION
/// f`, `INSERT INTO f`, `CREATE TABLE f` are no use of the routine `f`.
pub(super) const STATEMENT_NAMES: &[(Family, &[DefKind], &str)] = &[
    (
        Family::Sql,
        &[DefKind::Function],
        "function procedure table into references type copy view",
    ),
    (Family::Sql, &[DefKind::Object], "view"),
];

/// Families whose names live in schemas, not in classes: a definition of one key in several files
/// (a migration's `CREATE OR REPLACE`) is one object, and a routine of another schema is no
/// member a call through an unknown receiver may run.
pub(super) const SCHEMA_OBJECTS: &[Family] = &[Family::Sql];
