//! Reflective lookups by a name the code builds, read by the Java and Kotlin
//! extractors: `Lib.class.getMethod("via" + x)`, `lookup.findVirtual(Lib.class, name, type)`.

use std::ops::Range;

use tree_sitter::Node;

use super::kotlin::{KotlinFacts, text};

/// A reflective lookup (`Lib.class.getMethod("via" + x)`) whose name is not a
/// literal. `arg` is the name when it is a bare identifier, `class` the class
/// literal the lookup is written on.
#[derive(Debug, Clone)]
pub struct ComputedCall {
    pub range: Range<usize>,
    pub class: Option<String>,
    pub arg: Option<String>,
}

/// The reflective lookups by name, with the position of the name among the
/// arguments: `Class.getMethod(name)`, `lookup.findVirtual(Class, name, type)`.
const LOOKUPS: [(&str, usize); 7] = [
    ("getMethod", 0),
    ("getDeclaredMethod", 0),
    ("getField", 0),
    ("getDeclaredField", 0),
    ("findVirtual", 1),
    ("findStatic", 1),
    ("findGetter", 1),
];

pub(super) fn lookup_position(name: &str) -> Option<usize> {
    LOOKUPS.iter().find(|(n, _)| *n == name).map(|(_, at)| *at)
}

/// The class a class literal (`Lib.class`, `Lib::class`) names, searched in
/// `node` and below.
pub(super) fn class_literal_in(node: Node, bytes: &[u8]) -> Option<String> {
    if matches!(node.kind(), "class_literal" | "callable_reference") {
        let written = text(node, bytes)?;
        let class = written
            .strip_suffix(".class")
            .or_else(|| written.strip_suffix("::class"))?;
        return class.rsplit('.').next().map(str::to_string);
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| class_literal_in(child, bytes));
    found
}

/// Records a lookup whose `name` argument is no literal.
pub(super) fn computed(
    call: Node,
    name: Node,
    class: Option<String>,
    bytes: &[u8],
    out: &mut Vec<ComputedCall>,
) {
    let mut cursor = name.walk();
    let templated = name
        .named_children(&mut cursor)
        .any(|c| c.kind().starts_with("interpolated"));
    if matches!(name.kind(), "string_literal" | "text_block") && !templated {
        return;
    }
    let arg = matches!(name.kind(), "identifier" | "simple_identifier")
        .then(|| text(name, bytes).map(str::to_string))
        .flatten();
    out.push(ComputedCall {
        range: call.byte_range(),
        class,
        arg,
    });
}

/// Drops the lookups by a variable the file gives a string literal: a held
/// name the literal channel already reads.
pub(super) fn settle_computed(facts: &mut KotlinFacts) {
    let consts = std::mem::take(&mut facts.consts);
    facts
        .computed
        .retain(|c| !c.arg.as_ref().is_some_and(|a| consts.contains(a)));
}
