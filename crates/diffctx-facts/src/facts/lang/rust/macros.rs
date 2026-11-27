//! The macros of a Rust file: an invocation, the rules of a `macro_rules!`, and the tokens
//! of either read as the code they usually expand to.

use tree_sitter::Node;

use super::{Cx, Ty, Walker, path_of};
use crate::facts::{Name, Receiver, TypeName, UseKind};

impl<'t> Walker<'t> {
    pub(super) fn macro_call(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if let Some(name) = node.child_by_field_name("macro") {
            if let Some(mut path) = path_of(self.bytes, name) {
                if let Some(last) = path.pop() {
                    let site = match name.kind() {
                        "scoped_identifier" => name.child_by_field_name("name").unwrap_or(name),
                        _ => name,
                    };
                    let called = format!("{last}!");
                    // Expanded as an item of an `impl`, its `self` is the impl's type.
                    let receiver = match &cx.self_path {
                        Some(own) if !cx.in_fn => Receiver::Typed(TypeName::new(own.as_str())),
                        _ => Receiver::None,
                    };
                    self.emit(
                        site,
                        cx,
                        receiver,
                        UseKind::Call,
                        &called,
                        path.into_iter().map(Name::new).collect(),
                    );
                }
            }
        }
        let mut cursor = node.walk();
        let trees: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "token_tree")
            .collect();
        for tree in trees {
            self.tokens(tree, cx, depth + 1);
        }
    }

    /// The rules' bodies of a `macro_rules!`, whose `self.m(..)` calls are the calls of
    /// the `impl` each expansion sits in.
    pub(super) fn macro_rules(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        let bodies: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|rule| rule.kind() == "macro_rule")
            .filter_map(|rule| rule.child_by_field_name("right"))
            .collect();
        self.defining_macro = true;
        for body in bodies {
            self.tokens(body, cx, depth + 1);
        }
        self.defining_macro = false;
    }

    /// A macro's tokens read as the code they usually expand to: `a::b(..)`,
    /// `x.m(..)` and `name!`.
    pub(super) fn tokens(&mut self, tree: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        let mut cursor = tree.walk();
        let toks: Vec<Node<'t>> = tree.children(&mut cursor).collect();
        let ident = |n: Node<'t>| {
            matches!(
                n.kind(),
                "identifier" | "self" | "type_identifier" | "crate" | "super"
            )
        };
        let mut i = 0;
        while i < toks.len() {
            let tok = toks[i];
            if tok.kind() == "token_tree" {
                self.tokens(tok, cx, depth + 1);
                i += 1;
                continue;
            }
            if !ident(tok) {
                i += 1;
                continue;
            }
            // A run `a::b::c`.
            let mut run = vec![tok];
            let mut j = i;
            while toks.get(j + 1).is_some_and(|t| t.kind() == "::")
                && toks.get(j + 2).is_some_and(|t| ident(*t))
            {
                run.push(toks[j + 2]);
                j += 2;
            }
            let end = j;
            let next = toks.get(end + 1);
            let call = next.is_some_and(|n| {
                n.kind() == "token_tree" && n.child(0).is_some_and(|c| c.kind() == "(")
            });
            let names: Vec<String> = run.iter().map(|n| self.ident(*n).to_string()).collect();
            let previous = i.checked_sub(1).and_then(|p| toks.get(p));
            let last = run[run.len() - 1];
            // A rule's body is read for `self.m(..)` only: its other words declare
            // names or stand for the caller's.
            if self.defining_macro
                && !(call
                    && run.len() == 1
                    && previous.is_some_and(|p| p.kind() == ".")
                    && i.checked_sub(2).is_some_and(|b| toks[b].kind() == "self"))
            {
                i = end + 1;
                continue;
            }
            if next.is_some_and(|n| n.kind() == "!") {
                let called = format!("{}!", names[names.len() - 1]);
                let qualifier = names[..names.len() - 1]
                    .iter()
                    .map(|n| Name::new(n.as_str()))
                    .collect();
                self.emit(last, cx, Receiver::None, UseKind::Call, &called, qualifier);
            } else if previous.is_some_and(|p| p.kind() == ".") && run.len() == 1 {
                if call {
                    let before = i.checked_sub(2).and_then(|p| toks.get(p)).copied();
                    let (receiver, qualifier) = match before {
                        Some(b) if b.kind() == "self" => (Receiver::SelfType, Vec::new()),
                        Some(b) if b.kind() == "identifier" => {
                            let local = self.ident(b).to_string();
                            match self.lookup(&local).cloned() {
                                Some(Some(Ty::Named(t))) => {
                                    (Receiver::Typed(TypeName::new(t.as_str())), Vec::new())
                                }
                                _ => (Receiver::Unknown, vec![Name::new(local)]),
                            }
                        }
                        _ => (Receiver::Unknown, Vec::new()),
                    };
                    self.emit(last, cx, receiver, UseKind::Call, &names[0], qualifier);
                }
            } else if previous.is_none_or(|p| p.kind() != "::" && p.kind() != ".") {
                let keyword = |n: &str| matches!(n, "self" | "Self" | "super" | "crate");
                let name = names[names.len() - 1].clone();
                if !keyword(&name) && !(run.len() == 1 && self.lookup(&name).is_some()) {
                    let qualifier = names[..names.len() - 1]
                        .iter()
                        .map(|n| Name::new(n.as_str()))
                        .collect();
                    let kind = if call { UseKind::Call } else { UseKind::Read };
                    self.emit(last, cx, Receiver::None, kind, &name, qualifier);
                }
            }
            i = end + 1;
        }
    }
}
