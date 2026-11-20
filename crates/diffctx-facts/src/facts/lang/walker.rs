//! The methods the tree walkers of the language extractors write alike, once each.

use tree_sitter::Node;

use crate::facts::{Hasher, hash_text};

/// Hash of a node's text without the members that are facts of their own.
pub(super) fn hash_outside(bytes: &[u8], node: Node<'_>, members: &[Node<'_>]) -> u64 {
    let mut cuts: Vec<(usize, usize)> = members
        .iter()
        .map(|m| (m.start_byte(), m.end_byte()))
        .collect();
    cuts.sort_unstable();
    let mut pieces: Vec<&[u8]> = Vec::new();
    let mut at = node.start_byte();
    for (from, to) in cuts {
        if from >= at {
            pieces.push(bytes[at..from].trim_ascii());
            at = to;
        }
    }
    pieces.push(bytes[at..node.end_byte()].trim_ascii());
    hash_text(&pieces.join(&b'\n'))
}

/// Mixes a node's tree into `hash`: node kinds and leaf text, left out the comments and,
/// below the root, every node `held` names.
pub(super) fn hash_tree_into(
    bytes: &[u8],
    root: Node<'_>,
    hash: &mut Hasher,
    held: impl Fn(Node<'_>) -> bool,
) {
    let mut stack = vec![(root, false)];
    while let Some((node, closing)) = stack.pop() {
        if closing {
            hash.bytes(&[2]);
            continue;
        }
        if node.kind().ends_with("comment") || (node.id() != root.id() && held(node)) {
            continue;
        }
        if node.child_count() == 0 {
            hash.bytes(&[3]).token(node.kind().as_bytes());
            let text = std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]);
            hash.token(text.unwrap_or("").as_bytes());
        } else {
            hash.bytes(&[1]).token(node.kind().as_bytes());
            stack.push((node, true));
            let mut cursor = node.walk();
            let kids: Vec<Node<'_>> = node.children(&mut cursor).collect();
            stack.extend(kids.into_iter().rev().map(|k| (k, false)));
        }
    }
}

/// [`hash_tree_into`] on a fresh hash: `0` when nothing was mixed in.
pub(super) fn hash_tree(bytes: &[u8], root: Node<'_>, held: impl Fn(Node<'_>) -> bool) -> u64 {
    let mut hash = Hasher::new();
    hash_tree_into(bytes, root, &mut hash, held);
    hash.finish().unwrap_or(0)
}

/// Whether `node` is one of the `members` a declaration's own hash leaves to them.
pub(super) fn is_member(members: &[Node<'_>], node: Node<'_>) -> bool {
    members.iter().any(|m| m.id() == node.id())
}

/// Budget polling and text slicing of a tree walker.
macro_rules! walker_methods {
    () => {
        fn tick(&mut self) -> bool {
            if self.ticks % POLL_EVERY == 0 && expired(self.budget) {
                self.halted = true;
            }
            self.ticks += 1;
            !self.halted
        }

        fn text(&self, node: Node<'_>) -> &'t str {
            let bytes: &'t [u8] = self.bytes;
            std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).unwrap_or("")
        }
    };
}

/// The depth guard of a walker that recurses into expressions.
macro_rules! walker_depth {
    () => {
        fn deeper(&mut self, depth: u16) -> bool {
            if depth > MAX_DEPTH {
                self.too_deep = true;
                return false;
            }
            self.tick()
        }
    };
}

/// A reference fact of a walker.
macro_rules! walker_emit {
    () => {
        fn emit(
            &mut self,
            node: Node<'t>,
            cx: &Cx,
            receiver: Receiver,
            use_kind: UseKind,
            name: &str,
            qualifier: Vec<Name>,
        ) {
            self.facts.push(Fact::Reference(Ref {
                site: span(node),
                enclosing: cx.scope.clone(),
                receiver,
                use_kind,
                name: Name::new(name),
                qualifier,
            }));
        }
    };
}
