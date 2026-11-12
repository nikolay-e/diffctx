//! What a shell script's syntax states, as the facts contract names it.
//!
//! The bash grammar reads `sh` and `bash`; a script whose shebang names zsh, ksh
//! or fish is not read, and a loss says so.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - The script is a definition of kind `Module`, its top-level code (the program):
//!   keyed `<script>` in the module of its file, spanning the file. Its hash covers
//!   the shebang and every top-level command that is not a function or an
//!   assignment, so an edit of the program is a change of the script, and adding,
//!   removing or editing a function is the function's own change, not the
//!   script's. What sources or runs the file is what reads it. Its text hash covers
//!   the same text: the file without its function definitions and global
//!   assignments, which are definitions of their own, and without blank lines.
//! - A function (`name() {}`, `function name {}`) is a definition of kind
//!   `Function`; a global variable's first assignment is a definition of kind
//!   `Variable`, in the macro namespace so a variable and a function of one name
//!   are two keys. Where in the file either stands does not matter: a function is
//!   global once the script runs.
//! - A variable assigned in a function after `local` (or `declare`, `typeset`
//!   without `-g`) is a local binding and no definition; a later plain assignment
//!   to a name that is not local is a write of the global.
//! - A command word that is a plain word is a call of that name; `time` and `eval`
//!   call the word after them. `$name` and `${name…}` are reads of the name.
//! - `source path` and `. path` are `ShellPath`s that are sourced; a command word
//!   with a `/` in it, and `bash path`, are `ShellPath`s that are run. The path is
//!   spelled as a head and a literal tail: a variable counts for the directory of
//!   the script when every assignment of it in the script says so.
//! - A body hash covers the leaves and the nesting without comments, so an edit of
//!   a comment is layout and a change of quote style is not.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Language as TsLanguage, Node as TsNode};

use crate::facts::config::syntax;
use crate::facts::shell::{PathHead, PathUse, ShellPath};
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, Extractor, Fact, Hasher, Language, Local, LocalKey,
    LocalKind, Name, Namespace, Receiver, Ref, Scope, Span, UseKind, Visibility, expired,
    hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

pub struct ShellExtractor;

/// The key of a script's own definition in the module of its file.
pub const SCRIPT: &str = "<script>";
/// Nesting deeper than this is not read.
const MAX_DEPTH: usize = 120;
/// Nodes between two looks at the run's deadline.
const POLL_EVERY: usize = 256;

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

#[cfg(feature = "tree-sitter-bash")]
fn grammar() -> Option<TsLanguage> {
    Some(TsLanguage::new(tree_sitter_bash::LANGUAGE))
}

#[cfg(not(feature = "tree-sitter-bash"))]
fn grammar() -> Option<TsLanguage> {
    None
}

/// Whether the shebang names a shell the bash grammar does not read.
fn names_another_shell(source: &str) -> bool {
    let Some(first) = source.lines().next() else {
        return false;
    };
    let Some(line) = first.strip_prefix("#!") else {
        return false;
    };
    let mut words = line.split_whitespace();
    let basename = |word: &str| word.rsplit('/').next().unwrap_or(word).to_string();
    let Some(program) = words.next().map(basename) else {
        return false;
    };
    let interpreter = if program == "env" {
        words
            .find(|word| !word.starts_with('-') && !word.contains('='))
            .map_or_else(String::new, basename)
    } else {
        program
    };
    interpreter == "zsh" || interpreter == "fish" || interpreter.starts_with("ksh")
}

impl Extractor for ShellExtractor {
    fn language(&self) -> Language {
        Language::Shell
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        let mut blob = BlobFacts::new(Language::Shell);
        let Ok(source) = std::str::from_utf8(text) else {
            blob.losses.push(Loss::Unsupported { path: unbound() });
            return blob;
        };
        if names_another_shell(source) {
            blob.losses.push(Loss::Unsupported { path: unbound() });
            return blob;
        }
        let Some(grammar) = grammar() else {
            blob.losses.push(Loss::Unsupported { path: unbound() });
            return blob;
        };
        let Some(tree) = syntax::parse(&grammar, text, budget) else {
            if expired(budget) {
                blob.deadline();
            } else {
                blob.losses.push(Loss::Unsupported { path: unbound() });
            }
            return blob;
        };
        let root = tree.root_node();
        if root.has_error() {
            blob.parse_error(syntax::error_lines(root));
        }
        let mut walker = Walker::new(source, budget);
        walker.prepare(root);
        walker.script(root);
        walker.visit(root, &Scope::Module, None, 0);
        if walker.halted {
            blob.deadline();
        }
        if walker.too_deep {
            blob.losses.push(Loss::ExtractorFailed { path: unbound() });
        }
        blob.facts = walker.facts;
        blob.record_lexical(root);
        blob.in_source_order();
        blob
    }
}

/// The extractor of a script for a shell the bash grammar does not read, which a
/// file's extension or shebang says: it is never read as bash, and says so.
pub struct UnreadShellExtractor;

impl Extractor for UnreadShellExtractor {
    fn language(&self) -> Language {
        Language::ShellUnread
    }

    fn extract(&self, _text: &[u8], _budget: &RunBudget) -> BlobFacts {
        let mut blob = BlobFacts::new(Language::ShellUnread);
        blob.losses.push(Loss::Unsupported { path: unbound() });
        blob
    }
}

/// How a path is spelled: where it starts from, and the literal rest.
#[derive(Clone, PartialEq, Eq)]
struct Spelled {
    head: PathHead,
    tail: String,
}

impl Spelled {
    fn literal(text: &str) -> Self {
        Self {
            head: PathHead::Literal,
            tail: text.to_string(),
        }
    }

    fn unknown() -> Self {
        Self {
            head: PathHead::Unknown,
            tail: String::new(),
        }
    }

    fn then(mut self, next: Self) -> Self {
        match next.head {
            PathHead::Literal => self.tail.push_str(&next.tail),
            PathHead::ScriptDir if self.head == PathHead::Literal && self.tail.is_empty() => {
                return next;
            }
            _ => return Self::unknown(),
        }
        self
    }
}

/// How a declaration scopes the names it assigns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decl {
    /// A plain assignment: global unless the name was declared local.
    Plain,
    /// `local`, or `declare` in a function without `-g`.
    Local,
    /// `export`, `readonly`, `declare -g`, and `declare` at the top.
    Global,
}

struct Walker<'a> {
    text: &'a str,
    budget: &'a RunBudget,
    facts: Vec<Fact>,
    /// The value of every assignment of a name in the script, for the paths that
    /// spell their directory in a variable.
    assigned: HashMap<&'a str, Vec<TsNode<'a>>>,
    /// The variables the script has defined or written at global scope so far.
    globals: HashSet<String>,
    /// The names a function declared local.
    locals: HashSet<(String, String)>,
    /// How many functions of a name were met before.
    functions: HashMap<String, u16>,
    nodes: usize,
    halted: bool,
    too_deep: bool,
}

impl<'a> Walker<'a> {
    fn new(text: &'a str, budget: &'a RunBudget) -> Self {
        Self {
            text,
            budget,
            facts: Vec::new(),
            assigned: HashMap::new(),
            globals: HashSet::new(),
            locals: HashSet::new(),
            functions: HashMap::new(),
            nodes: 0,
            halted: false,
            too_deep: false,
        }
    }

    fn text_of(&self, node: TsNode) -> &'a str {
        self.text
            .get(node.start_byte()..node.end_byte())
            .unwrap_or("")
    }

    /// The assignments of the script, by name.
    fn prepare(&mut self, root: TsNode<'a>) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
            if node.kind() != "variable_assignment" {
                continue;
            }
            if let (Some(name), Some(value)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ) {
                self.assigned
                    .entry(self.text_of(name))
                    .or_default()
                    .push(value);
            }
        }
    }

    /// The script itself, as a definition: the program is its top-level code.
    fn script(&mut self, root: TsNode<'a>) {
        let mut hasher = Hasher::new();
        if let Some(shebang) = self.text.lines().next().filter(|l| l.starts_with("#!")) {
            hasher.token(shebang.trim().as_bytes());
        }
        // The program's own text: what is left when the definitions it holds are cut out.
        let mut program = String::new();
        let mut from = 0;
        for child in syntax::children(root) {
            match child.kind() {
                "function_definition" => {
                    program.push_str(self.text.get(from..child.start_byte()).unwrap_or(""));
                    from = child.end_byte();
                }
                "variable_assignment" | "declaration_command" => {
                    program.push_str(self.text.get(from..child.start_byte()).unwrap_or(""));
                    from = child.end_byte();
                }
                _ => self.mix(child, &mut hasher, 0),
            }
        }
        program.push_str(self.text.get(from..).unwrap_or(""));
        let program: String = program
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        self.facts.push(Fact::Definition(Def {
            local: LocalKey::value(SCRIPT),
            kind: DefKind::Module,
            span: syntax::span(root),
            name_span: Span::new(0..0, (1, 1)),
            signature: None,
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: hasher.finish().unwrap_or(1),
            text_hash: hash_text(program.as_bytes()),
            doc_hash: None,
        }));
    }

    fn visit(&mut self, node: TsNode<'a>, scope: &Scope, function: Option<&str>, depth: usize) {
        self.nodes += 1;
        if depth > MAX_DEPTH {
            self.too_deep = true;
            return;
        }
        if self.nodes % POLL_EVERY == 0 && expired(self.budget) {
            self.halted = true;
        }
        if self.halted {
            return;
        }
        match node.kind() {
            "function_definition" => {
                self.function(node, depth);
                return;
            }
            "command" => self.command(node, scope),
            "variable_assignment" => self.assignment(node, scope, function, Decl::Plain),
            "declaration_command" => {
                self.declaration(node, scope, function, depth);
                return;
            }
            "simple_expansion" | "expansion" => self.read(node, scope, function),
            _ => {}
        }
        for child in syntax::children(node) {
            self.visit(child, scope, function, depth + 1);
        }
    }

    fn function(&mut self, node: TsNode<'a>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else {
            for child in syntax::children(node) {
                self.visit(child, &Scope::Module, None, depth + 1);
            }
            return;
        };
        let name = self.text_of(name_node).to_string();
        let earlier = self.functions.entry(name.clone()).or_insert(0);
        let mut local = LocalKey::value(name.as_str());
        if *earlier > 0 {
            local = local.with_disambig(Disambig::Overload(*earlier));
        }
        *earlier = earlier.saturating_add(1);
        let span = syntax::span(node);
        self.facts.push(Fact::Definition(Def {
            local: local.clone(),
            kind: DefKind::Function,
            name_span: syntax::span(name_node),
            signature: None,
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: self.tree_hash(node),
            text_hash: hash_text(self.text_of(node).as_bytes()),
            doc_hash: None,
            span,
        }));
        let inside = Scope::Def(local);
        for child in syntax::children(node) {
            if child != name_node {
                self.visit(child, &inside, Some(name.as_str()), depth + 1);
            }
        }
    }

    fn command(&mut self, node: TsNode<'a>, scope: &Scope) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let word = self.text_of(name_node);
        let site = syntax::span(name_node);
        if matches!(word, "source" | ".") {
            if let Some(arg) = node.child_by_field_name("argument") {
                self.path(arg, scope, PathUse::Source);
            }
        } else if matches!(word, "time" | "eval") {
            // These run the command after them in this shell, a function
            // included; `sudo`, `env` and `exec` run a program of that name.
            let mut cursor = node.walk();
            let arg = node
                .children_by_field_name("argument", &mut cursor)
                .find(|a| !self.text_of(*a).starts_with('-'))
                .filter(|a| a.kind() == "word");
            if let Some(arg) = arg {
                let called = self.text_of(arg).to_string();
                self.call(called, syntax::span(arg), scope);
            }
        } else if word == "trap" {
            let mut cursor = node.walk();
            let handler = node
                .children_by_field_name("argument", &mut cursor)
                .find(|a| !self.text_of(*a).starts_with('-'))
                .filter(|a| matches!(a.kind(), "string" | "raw_string"));
            if let Some(handler) = handler {
                self.trap_handler(handler, scope);
            }
        } else if matches!(word, "bash" | "sh" | "dash") {
            // `bash script args`: the first word that is no flag, unless the
            // shell is given code with `-c`.
            let mut cursor = node.walk();
            let args: Vec<TsNode<'a>> = node
                .children_by_field_name("argument", &mut cursor)
                .collect();
            let code = args.iter().any(|a| self.text_of(*a) == "-c");
            let script = args
                .iter()
                .find(|a| !self.text_of(**a).starts_with('-'))
                .filter(|a| {
                    let text = self.text_of(**a);
                    text.contains('/') || text.ends_with(".sh") || text.ends_with(".bash")
                });
            if let Some(arg) = script.filter(|_| !code) {
                self.path(*arg, scope, PathUse::Run);
            }
        } else if let Some(first) = name_node
            .named_child(0)
            .filter(|first| first.kind() == "word")
        {
            if word.contains('/') {
                self.path(first, scope, PathUse::Run);
            } else {
                self.call(word.to_string(), site, scope);
            }
        }
    }

    /// A quoted trap handler is shell code run later: its first word is a call.
    fn trap_handler(&mut self, handler: TsNode<'a>, scope: &Scope) {
        let inner = &self.text_of(handler)[1..];
        let lead = inner.len() - inner.trim_start().len();
        let word = inner[lead..]
            .split(|c: char| c.is_whitespace() || ";&|<>()$\"'`".contains(c))
            .next()
            .unwrap_or("");
        if word.is_empty() {
            return;
        }
        let start = (handler.start_byte() + 1 + lead) as u32;
        let row = handler.start_position().row as u32 + 1;
        let line = row + inner[..lead].matches('\n').count() as u32;
        let site = Span::new(start..start + word.len() as u32, (line, line));
        self.call(word.to_string(), site, scope);
    }

    fn call(&mut self, name: String, site: Span, scope: &Scope) {
        self.facts.push(Fact::Reference(Ref {
            site,
            enclosing: scope.clone(),
            receiver: Receiver::None,
            use_kind: UseKind::Call,
            name: Name::new(name),
            qualifier: Vec::new(),
        }));
    }

    fn path(&mut self, arg: TsNode<'a>, scope: &Scope, how: PathUse) {
        let spelled = self.spelled(arg, 0);
        self.facts.push(Fact::ShellPath(ShellPath {
            site: syntax::span(arg),
            enclosing: scope.clone(),
            head: spelled.head,
            tail: spelled.tail,
            how,
        }));
    }

    /// How a word spells a path, its variables read off their assignments in the
    /// same script when every assignment agrees.
    fn spelled(&self, node: TsNode<'a>, depth: u8) -> Spelled {
        let text = self.text_of(node);
        let script_dir =
            |t: &str| t.contains("$0") || t.contains("${0") || t.contains("BASH_SOURCE");
        match node.kind() {
            "word" | "string_content" => Spelled::literal(text),
            "raw_string" => Spelled::literal(text.trim_matches('\'')),
            "command_substitution" if text.contains("dirname") && script_dir(text) => Spelled {
                head: PathHead::ScriptDir,
                tail: String::new(),
            },
            "expansion" if text.ends_with("%/*}") && script_dir(text) => Spelled {
                head: PathHead::ScriptDir,
                tail: String::new(),
            },
            "simple_expansion" | "expansion" => {
                let name = text.trim_start_matches('$').trim_matches(['{', '}']);
                match self.assigned.get(name) {
                    Some(values) if depth < 4 => {
                        let mut all = values.iter().map(|v| self.spelled(*v, depth + 1));
                        let first = all.next().unwrap_or_else(Spelled::unknown);
                        if all.all(|s| s == first) {
                            first
                        } else {
                            Spelled::unknown()
                        }
                    }
                    _ => Spelled::unknown(),
                }
            }
            "string" | "concatenation" => syntax::children(node)
                .into_iter()
                .fold(Spelled::literal(""), |acc, part| {
                    acc.then(self.spelled(part, depth))
                }),
            _ => Spelled::unknown(),
        }
    }

    fn declaration(
        &mut self,
        node: TsNode<'a>,
        scope: &Scope,
        function: Option<&str>,
        depth: usize,
    ) {
        let children = syntax::children(node);
        let keyword = node
            .child(0)
            .map_or("", |first| self.text_of(first))
            .to_string();
        let global_flag = children.iter().any(|c| {
            c.kind() == "word"
                && self.text_of(*c).starts_with('-')
                && self.text_of(*c).contains('g')
        });
        let decl = match keyword.as_str() {
            "local" => Decl::Local,
            "declare" | "typeset" if function.is_some() && !global_flag => Decl::Local,
            _ => Decl::Global,
        };
        for child in children {
            match child.kind() {
                "variable_assignment" => {
                    self.assignment(child, scope, function, decl);
                    for inner in syntax::children(child) {
                        self.visit(inner, scope, function, depth + 1);
                    }
                }
                "variable_name" => {
                    if let (Decl::Local, Some(function)) = (decl, function) {
                        let name = self.text_of(child).to_string();
                        self.local(&name, child, scope, function);
                    }
                }
                _ => self.visit(child, scope, function, depth + 1),
            }
        }
    }

    fn local(&mut self, name: &str, at: TsNode<'a>, scope: &Scope, function: &str) {
        self.locals.insert((function.to_string(), name.to_string()));
        self.facts.push(Fact::LocalBinding(Local {
            name: Name::new(name),
            site: syntax::span(at),
            enclosing: scope.clone(),
            kind: LocalKind::Variable,
        }));
    }

    fn assignment(&mut self, node: TsNode<'a>, scope: &Scope, function: Option<&str>, decl: Decl) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        // `FOO=bar cmd` sets FOO for the command alone.
        if node.parent().is_some_and(|p| p.kind() == "command") {
            return;
        }
        let raw = self.text_of(name_node);
        let name = raw.split('[').next().unwrap_or(raw).to_string();
        if let (Decl::Local, Some(function)) = (decl, function) {
            self.local(&name, name_node, scope, function);
            return;
        }
        if function.is_some_and(|f| self.locals.contains(&(f.to_string(), name.clone()))) {
            return;
        }
        if self.globals.insert(name.clone()) {
            self.facts.push(Fact::Definition(Def {
                local: LocalKey::new(Vec::new(), name.as_str(), Namespace::Macro),
                kind: DefKind::Variable,
                span: syntax::span(node),
                name_span: syntax::span(name_node),
                signature: None,
                decorators: Vec::new(),
                visibility: Visibility::Public,
                body_hash: self.tree_hash(node),
                text_hash: hash_text(self.text_of(node).as_bytes()),
                doc_hash: None,
            }));
        } else {
            self.facts.push(Fact::Reference(Ref {
                site: syntax::span(name_node),
                enclosing: scope.clone(),
                receiver: Receiver::None,
                use_kind: UseKind::Write,
                name: Name::new(name),
                qualifier: Vec::new(),
            }));
        }
    }

    fn read(&mut self, node: TsNode<'a>, scope: &Scope, function: Option<&str>) {
        let Some(name_node) = syntax::children(node)
            .into_iter()
            .find_map(|c| match c.kind() {
                "variable_name" => Some(c),
                "subscript" => c.child_by_field_name("name"),
                _ => None,
            })
        else {
            return;
        };
        let name = self.text_of(name_node);
        // A positional or special parameter (`$0`, `$1`, `$_`) is the shell's, never
        // a variable some script assigns.
        if name == "_" || name.bytes().all(|b| b.is_ascii_digit()) {
            return;
        }
        if function.is_some_and(|f| self.locals.contains(&(f.to_string(), name.to_string()))) {
            return;
        }
        self.facts.push(Fact::Reference(Ref {
            site: syntax::span(name_node),
            enclosing: scope.clone(),
            receiver: Receiver::None,
            use_kind: UseKind::Read,
            name: Name::new(name),
            qualifier: Vec::new(),
        }));
    }

    /// The tree of a node without comments: node kinds, nesting and leaf text.
    fn tree_hash(&self, node: TsNode) -> u64 {
        let mut hasher = Hasher::new();
        self.mix(node, &mut hasher, 0);
        hasher.finish().unwrap_or(1)
    }

    fn mix(&self, node: TsNode, hasher: &mut Hasher, depth: usize) {
        if node.kind() == "comment" || depth > MAX_DEPTH {
            return;
        }
        if node.child_count() == 0 {
            hasher.token(self.text_of(node).as_bytes());
            return;
        }
        hasher.token(node.kind().as_bytes());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.mix(child, hasher, depth + 1);
        }
    }
}
