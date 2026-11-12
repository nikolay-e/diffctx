//! One name table for the two JVM languages.
//!
//! Kotlin and Java compile to the same classes, so each reads the other's
//! declarations under the names the compiler gives them. This module holds that
//! table and the resolver both languages' cells share: every Kotlin and Java file
//! is read once, each language's engine resolves its own files against its own
//! declarations and the neighbour's, written as the engine reads its own, and a
//! cell sees the links of both.
//!
//! A projected definition carries the key of the definition it stands for, so a
//! binding that lands on it is a link to the real one; a holder only the compiler
//! makes (`FKt`, `INSTANCE`, `Companion`) carries a key no cell lists.
//!
//! - Kotlin as Java: a top-level function or property of `F.kt` is a static member
//!   of `FKt` (`@JvmName` renames a member, `@file:JvmName` the class); a property is `getX`, `setX`, `isX`
//!   (and the field `x` when `const`, `@JvmField` or `lateinit`); an `object` has
//!   the static field `INSTANCE`; a companion is the static field `Companion` (a
//!   named one, its name) and its `@JvmStatic` members are static members of the
//!   class around it too; an extension is a static of `FKt` taking its receiver.
//! - Java as Kotlin: a static member is a member of the type's companion (the
//!   Kotlin engine reads a type's statics there); a getter `getX()` or `isX()` and
//!   a setter `setX(v)` are also the property `x`; an enum constant is a constant
//!   of the companion.

use std::collections::{HashMap, HashSet};

use super::{Binding, Definition, Relation, Resolution, java, kotlin};
use crate::facts::{
    Def, DefKey, DefKind, Disambig, Export, Fact, FactsCache, FileFacts, Language, LocalKey, Name,
    Namespace, Visibility, expired, facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, Stage};

/// The bindings of one snapshot's Kotlin and Java files.
pub struct Jvm {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    read: Vec<RepoPath>,
}

impl Jvm {
    /// Resolves every Kotlin and Java file of `side`; the definitions are `own`'s.
    pub(super) fn open(
        ctx: &AnalysisContext,
        side: SideTag,
        cache: &FactsCache,
        own: Language,
    ) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let (mut kotlin_files, mut java_files) = (Vec::new(), Vec::new());
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            let language = Language::from_path(&entry.path);
            if !matches!(language, Some(Language::Kotlin | Language::Java)) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                losses.extend(file.losses());
                read.push(entry.path.clone());
                if language == Some(Language::Kotlin) {
                    kotlin_files.push(file);
                } else {
                    java_files.push(file);
                }
            }
        }
        let (kotlin_definitions, mut bindings, kotlin_stopped) =
            kotlin::solve(ctx, &kotlin_files, &java_files);
        let setters = setters(&java_files);
        for binding in &mut bindings {
            if binding.relation != Relation::Writes {
                continue;
            }
            if let Resolution::Resolved { target, .. } = &mut binding.resolution {
                if let Some(setter) = setters.get(target) {
                    *target = setter.clone();
                }
            }
        }
        let (java_definitions, java_bindings, java_stopped, java_losses) =
            java::solve(ctx, side, reader, &java_files, &kotlin_files);
        bindings.extend(java_bindings);
        losses.extend(java_losses);
        if kotlin_stopped || java_stopped {
            losses.push(Loss::Deadline {
                stage: Stage::Search,
            });
        }
        let definitions = if own == Language::Kotlin {
            kotlin_definitions
        } else {
            java_definitions
        };
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        Ok(Self {
            definitions,
            bindings,
            by_target,
            losses,
            read,
        })
    }

    pub(super) fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub(super) fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The bindings that resolve to `key` or have it among their candidates.
    #[allow(
        clippy::disallowed_methods,
        reason = "a key the index has no entry for has no references"
    )]
    pub(super) fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        self.by_target
            .get(key)
            .map(|at| at.iter().map(|i| &self.bindings[*i]).collect())
            .unwrap_or_default()
    }

    /// What the answer is partial for: files the extractor could not fully read,
    /// reads that failed, a run cut short.
    pub(super) fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// The files the resolver read: what an absence may rest on.
    pub(super) fn read_paths(&self) -> &[RepoPath] {
        &self.read
    }
}

/// A definition as the neighbouring language reads it, and the key of the
/// definition it stands for.
pub(super) struct Projected {
    pub def: Def,
    pub key: DefKey,
}

// ---- reading written declarations

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The first place `name` stands as a whole word in `text`.
fn find_word(text: &str, name: &str) -> Option<usize> {
    text.match_indices(name).map(|(at, _)| at).find(|at| {
        !text[..*at].chars().next_back().is_some_and(is_word)
            && !text[at + name.len()..].chars().next().is_some_and(is_word)
    })
}

fn has_word(text: &str, word: &str) -> bool {
    find_word(text, word).is_some()
}

/// The outer name of a written type: `a.b.List<T>?` is `List`; a function type
/// has none.
fn simple_type(written: &str) -> Option<String> {
    let text = written.trim();
    let end = text
        .find(|c: char| !(is_word(c) || c == '.'))
        .unwrap_or(text.len());
    let last = text[..end].rsplit('.').next()?;
    (!last.is_empty()).then(|| last.to_string())
}

/// The span of the first parenthesised list of `text`.
fn parens(text: &str) -> Option<(usize, usize)> {
    let open = text.find('(')?;
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((open, open + i));
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` split at the commas outside brackets.
fn split_top(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut from) = (0usize, 0usize);
    for (i, c) in text.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[from..]);
    parts
}

fn without_generics(text: &str) -> String {
    let mut depth = 0usize;
    text.chars()
        .filter(|c| match *c {
            '<' => {
                depth += 1;
                false
            }
            '>' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect()
}

fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How Kotlin names the property of a Java accessor's rest: `URLPath` is
/// `urlPath`, `Name` is `name`.
fn decapitalised(rest: &str) -> String {
    let upper = rest.chars().take_while(|c| c.is_uppercase()).count();
    let lower = if upper <= 1 || upper == rest.chars().count() {
        upper
    } else {
        upper - 1
    };
    rest.chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if i < lower {
                c.to_lowercase().collect::<Vec<char>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// The rest of `name` after `prefix` when a capital follows: `getX` gives `X`.
fn after_prefix<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)
        .filter(|rest| rest.chars().next().is_some_and(char::is_uppercase))
}

fn path_of(def: &Def) -> Vec<Name> {
    let mut path = def.local.container.clone();
    path.push(def.local.name.clone());
    path
}

/// The text of the first quoted string after `marker`: `@JvmName("n")` is `n`.
fn annotation_value(signature: &str, marker: &str) -> Option<String> {
    let after = &signature[signature.find(marker)? + marker.len()..];
    let open = after.find('"')?;
    let close = after[open + 1..].find('"')?;
    Some(after[open + 1..open + 1 + close].to_string())
}

fn projected(
    original: &Def,
    key: &DefKey,
    container: Vec<Name>,
    name: &str,
    kind: DefKind,
    header: String,
) -> Projected {
    let mut def = original.clone();
    def.local = LocalKey {
        container,
        name: Name::new(name),
        ns: original.local.ns,
        disambig: original
            .local
            .disambig
            .clone()
            .filter(|d| !matches!(d, Disambig::ImplOf(_))),
    };
    def.kind = kind;
    def.signature = Some(header);
    if def.visibility == Visibility::Crate {
        def.visibility = Visibility::Public;
    }
    Projected {
        def,
        key: key.clone(),
    }
}

/// A holder the compiler makes: its key is no definition of any cell.
fn synthetic(
    file: &FileFacts,
    anchor: &Def,
    container: Vec<Name>,
    name: &str,
    kind: DefKind,
    header: String,
) -> Projected {
    let local = LocalKey {
        container,
        name: Name::new(name),
        ns: Namespace::Value,
        disambig: Some(Disambig::Cfg("jvm".to_string())),
    };
    let key = file.key(&local);
    let mut def = anchor.clone();
    def.local = local;
    def.kind = kind;
    def.signature = Some(header);
    def.decorators = Vec::new();
    def.visibility = Visibility::Public;
    Projected { def, key }
}

// ---- Kotlin as Java

const KOTLIN_KEYWORDS: [&str; 7] = [
    "fun",
    "val",
    "var",
    "class",
    "interface",
    "object",
    "constructor",
];

/// The declaring keyword of a Kotlin header and where it stands: what is
/// written before it is the modifiers.
fn keyword(signature: &str) -> Option<(usize, &'static str)> {
    KOTLIN_KEYWORDS
        .iter()
        .filter_map(|word| find_word(signature, word).map(|at| (at, *word)))
        .min_by_key(|(at, _)| *at)
}

fn modifiers(signature: &str) -> &str {
    keyword(signature).map_or(signature, |(at, _)| &signature[..at])
}

fn after_keyword(signature: &str) -> &str {
    &signature[keyword(signature).map_or(0, |(at, word)| at + word.len())..]
}

/// The parameters a Kotlin header writes, as a Java parameter list reads them:
/// the count is what the callers' arguments are held against.
fn parameters(signature: &str) -> String {
    let after = after_keyword(signature);
    let Some((open, close)) = parens(after) else {
        return String::new();
    };
    let mut text = after[open + 1..close].replace("->", "=");
    if text.contains("vararg") {
        text.push_str(" ...");
    }
    text
}

/// The type a Kotlin declaration writes: after `name` for a property, past the
/// parameters for a function.
fn written_after(signature: &str, name: &str, callable: bool) -> Option<String> {
    let after = after_keyword(signature);
    let mut rest = &after[find_word(after, name)? + name.len()..];
    if callable {
        let (_, close) = parens(rest)?;
        rest = &rest[close + 1..];
    }
    simple_type(rest.trim_start().strip_prefix(':')?)
}

/// The outer names of the supertypes a Kotlin type header states.
fn supertypes(signature: &str, name: &str) -> Vec<String> {
    let after = after_keyword(signature);
    let Some(at) = find_word(after, name) else {
        return Vec::new();
    };
    let rest = &after[at + name.len()..];
    let mut depth = 0usize;
    let mut colon = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                colon = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    let Some(colon) = colon else {
        return Vec::new();
    };
    let listed = rest[colon..].split(" where ").next().unwrap_or("");
    split_top(listed)
        .into_iter()
        .filter_map(simple_type)
        .collect()
}

/// The class the compiler makes of a file's top-level declarations: the one
/// `@file:JvmName` names, else the file's name and `Kt`.
fn file_class(file: &FileFacts) -> String {
    let named = file.blob.facts.iter().find_map(|fact| match fact {
        Fact::Export(Export::Default {
            local: Some(name), ..
        }) => Some(name.as_str().to_string()),
        _ => None,
    });
    if let Some(name) = named {
        return name;
    }
    let stem = std::path::Path::new(file.path.as_str())
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    format!("{}Kt", capitalised(stem))
}

/// Where a member of Kotlin is a member of Java, and whether it is static there:
/// a top-level one in the file's class, one of an `object` in it (static when it
/// says so), one of a companion in it and, when it says so, in the class around.
fn placements(
    container: &[Name],
    holders: &HashMap<Vec<Name>, &Def>,
    class: &str,
    stated_static: bool,
) -> Vec<(Vec<Name>, bool)> {
    if container.is_empty() {
        return vec![(vec![Name::new(class)], true)];
    }
    let Some(holder) = holders
        .get(container)
        .filter(|holder| holder.kind == DefKind::Object)
    else {
        return vec![(container.to_vec(), false)];
    };
    if !stated_static {
        return vec![(container.to_vec(), false)];
    }
    let companion = holder
        .signature
        .as_deref()
        .is_some_and(|signature| has_word(modifiers(signature), "companion"));
    match container.split_last() {
        Some((_, outer)) if companion && !outer.is_empty() => {
            vec![(container.to_vec(), false), (outer.to_vec(), true)]
        }
        _ => vec![(container.to_vec(), !companion)],
    }
}

/// `getX` for `x`, and `isX` itself: how Kotlin names a property's getter.
fn getter_of(name: &str) -> String {
    if after_prefix(name, "is").is_some() {
        name.to_string()
    } else {
        format!("get{}", capitalised(name))
    }
}

fn setter_of(name: &str) -> String {
    format!(
        "set{}",
        after_prefix(name, "is").map_or_else(|| capitalised(name), str::to_string)
    )
}

/// The members of a Kotlin file as Java reads them.
pub(super) fn kotlin_as_java(file: &FileFacts) -> Vec<Projected> {
    let defs: Vec<&Def> = file.defs().collect();
    let holders: HashMap<Vec<Name>, &Def> = defs.iter().map(|d| (path_of(d), *d)).collect();
    let class = file_class(file);
    let mut out = Vec::new();
    let mut top: Option<&Def> = None;
    for &d in &defs {
        let key = file.key(&d.local);
        let signature = d.signature.as_deref().unwrap_or("");
        let words = modifiers(signature);
        let name = d.local.name.as_str();
        let extension = matches!(d.local.disambig, Some(Disambig::ImplOf(_)));
        let mut container = d.local.container.clone();
        if extension {
            container.pop();
        }
        let in_interface = holders
            .get(&container)
            .is_some_and(|holder| holder.kind == DefKind::Interface);
        match d.kind {
            DefKind::Class
            | DefKind::Interface
            | DefKind::Enum
            | DefKind::Struct
            | DefKind::Trait
            | DefKind::Object => {
                let (word, kind) = match d.kind {
                    DefKind::Interface => ("interface", DefKind::Interface),
                    DefKind::Enum => ("enum", DefKind::Enum),
                    _ => ("class", DefKind::Class),
                };
                let supers = supertypes(signature, name).join(" ");
                let extends = if supers.is_empty() {
                    String::new()
                } else {
                    format!(" extends {supers}")
                };
                let header = format!("public {word} {name}{extends}");
                out.push(projected(
                    d,
                    &key,
                    d.local.container.clone(),
                    name,
                    kind,
                    header,
                ));
                if d.kind == DefKind::Object {
                    let mut inside = d.local.container.clone();
                    if has_word(words, "companion") {
                        let header = format!("public static final {name} {name}");
                        out.push(synthetic(file, d, inside, name, DefKind::Field, header));
                    } else {
                        inside.push(d.local.name.clone());
                        let header = format!("public static final {name} INSTANCE");
                        out.push(synthetic(
                            file,
                            d,
                            inside,
                            "INSTANCE",
                            DefKind::Field,
                            header,
                        ));
                    }
                }
            }
            DefKind::Function | DefKind::Method | DefKind::Constructor => {
                if container.is_empty() && d.kind != DefKind::Constructor {
                    top = top.or(Some(d));
                }
                let mut params = parameters(signature);
                if extension {
                    params = if params.trim().is_empty() {
                        "Object receiver".to_string()
                    } else {
                        format!("Object receiver, {params}")
                    };
                }
                let declared = written_after(signature, name, true);
                let java_name =
                    annotation_value(signature, "@JvmName(").unwrap_or_else(|| name.to_string());
                if d.kind == DefKind::Constructor {
                    let header = format!("public {name}({params})");
                    out.push(projected(
                        d,
                        &key,
                        container,
                        name,
                        DefKind::Constructor,
                        header,
                    ));
                    continue;
                }
                let open = has_word(words, "open")
                    || has_word(words, "abstract")
                    || has_word(words, "override")
                    || in_interface;
                let stated_static = signature.contains("@JvmStatic");
                for (inside, is_static) in placements(&container, &holders, &class, stated_static) {
                    let statics = if is_static { " static" } else { "" };
                    let sealed = if open || is_static { "" } else { " final" };
                    let header = format!(
                        "public{statics}{sealed} {} {java_name}({params})",
                        declared.as_deref().unwrap_or("Object")
                    );
                    out.push(projected(
                        d,
                        &key,
                        inside,
                        &java_name,
                        DefKind::Method,
                        header,
                    ));
                }
            }
            DefKind::EnumMember => {
                let holder = container.last().map_or("Object", Name::as_str);
                let header = format!("public static final {holder} {name}");
                out.push(projected(d, &key, container, name, DefKind::Field, header));
            }
            DefKind::Property | DefKind::Variable | DefKind::Constant | DefKind::Field => {
                if container.is_empty() {
                    top = top.or(Some(d));
                }
                let written = written_after(signature, name, false);
                let declared = written.as_deref().unwrap_or("Object");
                let mutable = keyword(signature).is_some_and(|(_, word)| word == "var");
                let constant = has_word(words, "const") || signature.contains("@JvmField");
                let exposed = constant || has_word(words, "lateinit");
                let stated_static = signature.contains("@JvmStatic") || constant;
                for (inside, is_static) in placements(&container, &holders, &class, stated_static) {
                    let statics = if is_static { " static" } else { "" };
                    if exposed {
                        let header = format!("public{statics} {declared} {name}");
                        out.push(projected(
                            d,
                            &key,
                            inside.clone(),
                            name,
                            DefKind::Field,
                            header,
                        ));
                    }
                    if constant {
                        continue;
                    }
                    let getter = annotation_value(signature, "@get:JvmName(")
                        .unwrap_or_else(|| getter_of(name));
                    let header = format!("public{statics} final {declared} {getter}()");
                    out.push(projected(
                        d,
                        &key,
                        inside.clone(),
                        &getter,
                        DefKind::Method,
                        header,
                    ));
                    if mutable {
                        let setter = annotation_value(signature, "@set:JvmName(")
                            .unwrap_or_else(|| setter_of(name));
                        let header =
                            format!("public{statics} final void {setter}({declared} value)");
                        out.push(projected(d, &key, inside, &setter, DefKind::Method, header));
                    }
                }
            }
            DefKind::TypeAlias | DefKind::Macro | DefKind::Module => {}
        }
    }
    if let Some(anchor) = top {
        let header = format!("public final class {class}");
        out.push(synthetic(
            file,
            anchor,
            Vec::new(),
            &class,
            DefKind::Class,
            header,
        ));
    }
    out
}

// ---- Java as Kotlin

/// What a Java header writes in front of `name`: modifiers and the type.
fn before_name<'a>(signature: &'a str, name: &str) -> &'a str {
    find_word(signature, name).map_or(signature, |at| &signature[..at])
}

/// The outer name of the type that ends `text`: `List<String>` and `int[]`.
fn type_before(text: &str) -> Option<String> {
    let mut text = text.trim_end().trim_end_matches("[]").trim_end();
    if text.ends_with('>') {
        let mut depth = 0usize;
        let mut cut = None;
        for (i, c) in text.char_indices().rev() {
            match c {
                '>' => depth += 1,
                '<' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        cut = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        text = &text[..cut?];
    }
    let word = text.rsplit(|c: char| !(is_word(c) || c == '.')).next()?;
    simple_type(word)
}

fn java_parameters(signature: &str, name: &str) -> String {
    let Some(at) = find_word(signature, name) else {
        return String::new();
    };
    let after = &signature[at..];
    parens(after).map_or_else(String::new, |(open, close)| {
        after[open + 1..close].to_string()
    })
}

/// The property Kotlin reads a Java accessor as: `getX()` and `isX()` read it,
/// `setX(v)` writes it.
fn property_of(name: &str, params: &str, returns: bool) -> Option<String> {
    let none = params.trim().is_empty();
    if none && returns {
        if let Some(rest) = after_prefix(name, "get") {
            return Some(decapitalised(rest));
        }
        if after_prefix(name, "is").is_some() {
            return Some(name.to_string());
        }
    }
    after_prefix(name, "set")
        .filter(|_| !none && !params.contains(','))
        .map(decapitalised)
}

/// The setter of each Java getter of its class: Kotlin writes the property the
/// getter reads, and the write is a use of the setter.
pub(super) fn setters(files: &[FileFacts]) -> HashMap<DefKey, DefKey> {
    let mut found = HashMap::new();
    for file in files {
        let mut getters: HashMap<(Vec<Name>, String), DefKey> = HashMap::new();
        let mut writers: Vec<(Vec<Name>, Vec<String>, DefKey)> = Vec::new();
        for d in file.defs().filter(|d| d.kind == DefKind::Method) {
            let signature = d.signature.as_deref().unwrap_or("");
            let name = d.local.name.as_str();
            let params = java_parameters(signature, name);
            let returns = type_before(before_name(signature, name)).is_some_and(|ty| ty != "void");
            let Some(property) = property_of(name, &params, returns) else {
                continue;
            };
            let key = file.key(&d.local);
            match after_prefix(name, "set") {
                Some(rest) => {
                    let names = vec![property, format!("is{rest}")];
                    writers.push((d.local.container.clone(), names, key));
                }
                None => {
                    getters.insert((d.local.container.clone(), property), key);
                }
            }
        }
        for (container, names, setter) in writers {
            for property in names {
                if let Some(getter) = getters.get(&(container.clone(), property)) {
                    found.insert(getter.clone(), setter.clone());
                }
            }
        }
    }
    found
}

fn java_supertypes(signature: &str) -> Vec<String> {
    let plain = without_generics(signature);
    let mut listing = false;
    let mut out = Vec::new();
    for token in plain.split(|c: char| c.is_whitespace() || c == ',') {
        match token {
            "extends" | "implements" => listing = true,
            "permits" | "throws" => listing = false,
            "" => {}
            _ if listing => out.extend(simple_type(token)),
            _ => {}
        }
    }
    out
}

/// The members of a Java file as Kotlin reads them.
pub(super) fn java_as_kotlin(file: &FileFacts) -> Vec<Projected> {
    let defs: Vec<&Def> = file.defs().collect();
    let holders: HashMap<Vec<Name>, &Def> = defs.iter().map(|d| (path_of(d), *d)).collect();
    let mut out = Vec::new();
    let mut companions: HashSet<Vec<Name>> = HashSet::new();
    for &d in &defs {
        let key = file.key(&d.local);
        let signature = d.signature.as_deref().unwrap_or("");
        let name = d.local.name.as_str();
        let container = d.local.container.clone();
        let written = before_name(signature, name);
        let value = matches!(
            d.kind,
            DefKind::Field | DefKind::Constant | DefKind::Variable | DefKind::Property
        );
        let member = !matches!(
            d.kind,
            DefKind::Class
                | DefKind::Interface
                | DefKind::Enum
                | DefKind::Struct
                | DefKind::Trait
                | DefKind::Object
        );
        let is_static = member
            && (has_word(written, "static")
                || d.kind == DefKind::EnumMember
                || (value
                    && holders
                        .get(&container)
                        .is_some_and(|holder| holder.kind == DefKind::Interface)));
        let inside = if is_static && !container.is_empty() {
            if companions.insert(container.clone()) {
                out.push(synthetic(
                    file,
                    d,
                    container.clone(),
                    "Companion",
                    DefKind::Object,
                    "companion object Companion".to_string(),
                ));
            }
            let mut inside = container.clone();
            inside.push(Name::new("Companion"));
            inside
        } else {
            container.clone()
        };
        match d.kind {
            DefKind::Class
            | DefKind::Interface
            | DefKind::Enum
            | DefKind::Struct
            | DefKind::Trait
            | DefKind::Object => {
                let word = match d.kind {
                    DefKind::Interface => "interface",
                    DefKind::Enum => "enum class",
                    _ => "class",
                };
                let supers = java_supertypes(signature).join(", ");
                let header = if supers.is_empty() {
                    format!("{word} {name}")
                } else {
                    format!("{word} {name} : {supers}")
                };
                out.push(projected(d, &key, container, name, d.kind, header));
            }
            DefKind::Constructor => {
                let params = java_parameters(signature, name);
                let header = format!("constructor({params})");
                out.push(projected(
                    d,
                    &key,
                    container,
                    name,
                    DefKind::Constructor,
                    header,
                ));
            }
            DefKind::Function | DefKind::Method => {
                let params = java_parameters(signature, name);
                let returned = type_before(written).filter(|ty| ty != "void");
                let header = match &returned {
                    Some(ty) => format!("fun {name}({params}): {ty}"),
                    None => format!("fun {name}({params})"),
                };
                out.push(projected(d, &key, inside.clone(), name, d.kind, header));
                if let Some(property) = property_of(name, &params, returned.is_some()) {
                    let header = match &returned {
                        Some(ty) => format!("val {property}: {ty}"),
                        None => format!("var {property}"),
                    };
                    out.push(projected(
                        d,
                        &key,
                        inside,
                        &property,
                        DefKind::Property,
                        header,
                    ));
                }
            }
            DefKind::EnumMember => {
                let holder = container.last().map_or("Any", Name::as_str);
                let header = format!("val {name}: {holder}");
                out.push(projected(d, &key, inside, name, DefKind::Constant, header));
            }
            DefKind::Field | DefKind::Constant | DefKind::Variable | DefKind::Property => {
                let header = match type_before(written) {
                    Some(ty) => format!("val {name}: {ty}"),
                    None => format!("val {name}"),
                };
                out.push(projected(d, &key, inside, name, d.kind, header));
            }
            DefKind::TypeAlias | DefKind::Macro | DefKind::Module => {}
        }
    }
    out
}
