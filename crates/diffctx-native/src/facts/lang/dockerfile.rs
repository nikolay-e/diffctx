//! What a Dockerfile states, as the facts contract names it.
//!
//! - A build stage is a definition of kind `Object` named as the file writes its
//!   alias (`FROM alpine AS Build` is `Build`), else by its position (`0`, `1`);
//!   its span runs from its `FROM` to the next, its body hash covers every
//!   instruction in it, and its signature is `FROM <base>`.
//! - A stage the file names, as a `FROM` base (qualifier `from`) or a `COPY
//!   --from=` or `RUN --mount=from=` value (qualifier `copy`), is a `Read` of that
//!   name owned by the stage that writes it. A base with no `:` or `@` could be
//!   an image as well: the resolver tells. An `ARG` before the first `FROM` and
//!   the names a `FROM` line reads belong to no stage (`Scope::Module`).
//! - `ARG` and `ENV` are `Variable` definitions in the stage (the file when
//!   before the first `FROM`), signed `ARG NAME=value` or `ENV NAME=value`. A
//!   `$NAME` or `${NAME...}` in what Docker expands (`FROM`, `ARG`, `ENV`,
//!   `COPY`, `ADD`, `WORKDIR`, `USER`, `EXPOSE`, `LABEL`, `VOLUME`, `STOPSIGNAL`)
//!   is a `Read` with no qualifier; what a `RUN`, `CMD` or `ENTRYPOINT` shell
//!   expands is not read: no inline shell in this version.
//! - A source of `COPY` or `ADD` (without `--from`, not a URL or a heredoc), a
//!   `RUN --mount=type=bind,source=` without `from=`, and a script a `RUN` names
//!   (the destination of an earlier `COPY` in the stage, or `./name` under a `COPY
//!   . .`) are a `PathMention` of the repository path, owned by the stage.
//! - `ONBUILD` and heredoc bodies are not read.

use std::collections::HashMap;
use std::ops::Range;

use crate::facts::config::dockerfile::{Dockerfile, Instruction, Word, parse, unquote};
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, Extractor, Fact, Hasher, Language, Lexical, LocalKey, Name,
    Namespace, PathMention, Receiver, Ref, Scope, Span, UseKind, Visibility, expired, hash_text,
};
use crate::snapshot::RunBudget;

pub struct DockerfileExtractor;

fn part(span: &Span, offset: usize, len: usize) -> Span {
    let start = span.bytes.start + offset as u32;
    Span::new(start..start + len as u32, span.lines)
}

impl Extractor for DockerfileExtractor {
    fn language(&self) -> Language {
        Language::Dockerfile
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        let mut blob = BlobFacts::new(Language::Dockerfile);
        if expired(budget) {
            blob.deadline();
            return blob;
        }
        let Ok(source) = std::str::from_utf8(text) else {
            blob.unsupported();
            return blob;
        };
        (blob.facts, blob.lexical) = Reader::new(source).run();
        blob.in_source_order();
        blob
    }
}

/// The keywords whose words Docker expands variables in.
const EXPANDING: &[&str] = &[
    "FROM",
    "ARG",
    "ENV",
    "COPY",
    "ADD",
    "WORKDIR",
    "USER",
    "EXPOSE",
    "LABEL",
    "VOLUME",
    "STOPSIGNAL",
];

/// Commands that run the file named after them.
const INTERPRETERS: &[&str] = &[
    "sh", "bash", "zsh", "python", "python3", "node", "perl", "ruby",
];

struct Stage {
    key: LocalKey,
    start: u32,
    end: u32,
    first_line: u32,
    last_line: u32,
    name_span: Span,
    base: String,
    name: String,
    hasher: Hasher,
    workdir: String,
    /// `(destination in the image, source path)` of each `COPY` so far.
    copies: Vec<(String, String)>,
}

struct Reader<'a> {
    source: &'a str,
    facts: Vec<Fact>,
    stages: Vec<Stage>,
    twins: HashMap<LocalKey, u16>,
    finished: Vec<Fact>,
    /// Comment lines and the quoted values of `LABEL` and `ENV`: text, never code.
    text: Vec<(Range<u32>, Lexical)>,
}

fn key_in(container: &[String], name: &str) -> LocalKey {
    LocalKey::new(
        container.iter().map(|c| Name::new(c.as_str())).collect(),
        name,
        Namespace::Value,
    )
}

impl<'a> Reader<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            facts: Vec::new(),
            stages: Vec::new(),
            twins: HashMap::new(),
            finished: Vec::new(),
            text: Vec::new(),
        }
    }

    fn twin(&mut self, key: LocalKey) -> LocalKey {
        let seen = self.twins.entry(key.clone()).or_insert(0);
        let nth = *seen;
        *seen += 1;
        if nth == 0 {
            key
        } else {
            key.with_disambig(Disambig::Overload(nth))
        }
    }

    fn scope(&self) -> Scope {
        self.stages
            .last()
            .map_or(Scope::Module, |s| Scope::Def(s.key.clone()))
    }

    fn container(&self) -> Vec<String> {
        self.stages
            .last()
            .map_or_else(Vec::new, |s| vec![s.name.clone()])
    }

    fn run(mut self) -> (Vec<Fact>, Vec<(Range<u32>, Lexical)>) {
        let parsed = parse(self.source);
        let mut on_build = false;
        for instruction in &parsed.instructions {
            if instruction.keyword != "FROM" {
                if let Some(stage) = self.stages.last_mut() {
                    stage.end = instruction.span.bytes.end;
                    stage.last_line = instruction.span.lines.1;
                }
                self.hash_in(instruction);
            }
            if instruction.keyword == "ONBUILD" {
                on_build = true;
            }
            if on_build {
                on_build = false;
                continue;
            }
            match instruction.keyword.as_str() {
                "FROM" => self.from(instruction),
                "ARG" => self.arg(instruction),
                "ENV" => self.env(instruction),
                "COPY" | "ADD" => self.copy(instruction),
                "RUN" => self.run_instruction(instruction),
                "WORKDIR" => {
                    if let (Some(stage), Some(word)) =
                        (self.stages.last_mut(), instruction.words.first())
                    {
                        stage.workdir = join_path(&stage.workdir, &unquote(&word.text));
                    }
                }
                _ => {}
            }
            if EXPANDING.contains(&instruction.keyword.as_str()) && instruction.keyword != "FROM" {
                for word in &instruction.words {
                    self.reads_in(word);
                }
            }
        }
        self.close_stages();
        self.record_text(&parsed);
        let mut facts = std::mem::take(&mut self.facts);
        facts.extend(std::mem::take(&mut self.finished));
        (facts, self.text)
    }

    fn record_text(&mut self, parsed: &Dockerfile) {
        let heredocs: Vec<_> = parsed.instructions.iter().filter(|i| i.heredoc).collect();
        let mut offset = 0;
        for line in self.source.split_inclusive('\n') {
            let at = offset + line.len() - line.trim_start().len();
            let within = |i: &&Instruction| {
                (i.span.bytes.start as usize..i.span.bytes.end as usize).contains(&at)
            };
            if line[at - offset..].starts_with('#') && !heredocs.iter().any(within) {
                let end = offset + line.trim_end().len();
                self.text.push((at as u32..end as u32, Lexical::Comment));
            }
            offset += line.len();
        }
        let valued =
            (parsed.instructions.iter()).filter(|i| ["LABEL", "ENV"].contains(&&*i.keyword));
        for word in valued.flat_map(|i| &i.words) {
            if let Some((key, value)) = word.text.split_once('=') {
                if value.starts_with(['"', '\'']) && !value.contains('$') {
                    let start = word.span.bytes.start + key.len() as u32 + 1;
                    self.text
                        .push((start..word.span.bytes.end, Lexical::String));
                }
            }
        }
    }

    fn hash_in(&mut self, instruction: &Instruction) {
        if let Some(stage) = self.stages.last_mut() {
            stage.hasher.token(instruction.keyword.as_bytes());
            for flag in &instruction.flags {
                stage.hasher.token(flag.name.as_bytes());
                stage.hasher.token(flag.value.as_bytes());
            }
            for word in &instruction.words {
                stage.hasher.token(word.text.as_bytes());
            }
        }
    }

    /// Emits the definition of every stage, now its extent is known.
    fn close_stages(&mut self) {
        let stages = std::mem::take(&mut self.stages);
        for stage in stages {
            let text = self
                .source
                .get(stage.start as usize..stage.end as usize)
                .unwrap_or("");
            self.finished.push(Fact::Definition(Def {
                local: stage.key.clone(),
                kind: DefKind::Object,
                span: Span {
                    bytes: stage.start..stage.end,
                    lines: (stage.first_line, stage.last_line),
                },
                name_span: stage.name_span.clone(),
                signature: Some(format!("FROM {}", stage.base)),
                decorators: Vec::new(),
                visibility: Visibility::Public,
                body_hash: stage.hasher.finish().unwrap_or(1),
                text_hash: hash_text(text.as_bytes()),
                doc_hash: None,
            }));
        }
    }

    fn from(&mut self, instruction: &Instruction) {
        // The previous stage is complete: keep it for the definition, start the new.
        let Some(base_word) = instruction.words.first() else {
            return;
        };
        let alias = match instruction.words.as_slice() {
            [_, as_word, name, ..] if as_word.text.eq_ignore_ascii_case("as") => Some(name),
            _ => None,
        };
        // Stages already read stay in `finished` once closed.
        let index = self.stages_seen();
        let name = alias.map_or_else(|| index.to_string(), |w| w.text.clone());
        let key = self.twin(LocalKey::new(Vec::new(), name.as_str(), Namespace::Type));
        let name_span = alias.map_or_else(|| base_word.span.clone(), |w| w.span.clone());
        let start = instruction.span.bytes.start;
        // Variables a `FROM` line reads belong to no stage.
        for var in vars_in(&base_word.text) {
            self.facts.push(Fact::Reference(Ref {
                site: base_word.span.clone(),
                enclosing: Scope::Module,
                receiver: Receiver::None,
                use_kind: UseKind::Read,
                name: Name::new(var),
                qualifier: Vec::new(),
            }));
        }
        self.close_stages();
        let mut hasher = Hasher::new();
        hasher.token(instruction.keyword.as_bytes());
        for word in &instruction.words {
            hasher.token(word.text.as_bytes());
        }
        self.stages.push(Stage {
            key: key.clone(),
            start,
            end: instruction.span.bytes.end,
            first_line: instruction.span.lines.0,
            last_line: instruction.span.lines.1,
            name_span,
            base: base_word.text.clone(),
            name,
            hasher,
            workdir: String::new(),
            copies: Vec::new(),
        });
        let base = base_word.text.as_str();
        if base != "scratch" && !base.contains(['$', ':', '@']) {
            self.stage_ref(base_word, "from", base);
        }
    }

    /// How many stages the file has had so far.
    fn stages_seen(&self) -> usize {
        self.finished
            .iter()
            .filter(|f| matches!(f, Fact::Definition(d) if d.kind == DefKind::Object))
            .count()
            + self.stages.len()
    }

    fn stage_ref(&mut self, word: &Word, via: &str, name: &str) {
        self.facts.push(Fact::Reference(Ref {
            site: word.span.clone(),
            enclosing: self.scope(),
            receiver: Receiver::None,
            use_kind: UseKind::Read,
            name: Name::new(name),
            qualifier: vec![Name::new(via)],
        }));
    }

    fn flag_ref(&mut self, value: &str, span: &Span) {
        let flag = &self.source[span.bytes.start as usize..span.bytes.end as usize];
        let at = flag.find("from=").map_or(0, |at| at + 5);
        let word = Word {
            text: value.to_string(),
            span: part(span, at, value.len()),
        };
        self.stage_ref(&word, "copy", value);
    }

    fn define(&mut self, instruction: &Instruction, keyword: &str, name: &str, value: &str) {
        let container = self.container();
        let key = self.twin(key_in(&container, name));
        let mut hasher = Hasher::new();
        hasher.token(keyword.as_bytes()).token(value.as_bytes());
        let text = self
            .source
            .get(instruction.span.bytes.start as usize..instruction.span.bytes.end as usize)
            .unwrap_or("");
        let name_span = instruction
            .words
            .iter()
            .find(|w| w.text.starts_with(name))
            .map_or_else(
                || instruction.keyword_span.clone(),
                |w| part(&w.span, 0, name.len()),
            );
        self.facts.push(Fact::Definition(Def {
            local: key,
            kind: DefKind::Variable,
            span: instruction.span.clone(),
            name_span,
            signature: Some(if value.is_empty() {
                format!("{keyword} {name}")
            } else {
                format!("{keyword} {name}={value}")
            }),
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: hasher.finish().unwrap_or(1),
            text_hash: hash_text(text.as_bytes()),
            doc_hash: None,
        }));
    }

    fn arg(&mut self, instruction: &Instruction) {
        for word in &instruction.words {
            let (name, value) = word.text.split_once('=').unwrap_or((&word.text, ""));
            if is_name(name) {
                self.define(instruction, "ARG", name, &unquote(value));
            }
        }
    }

    fn env(&mut self, instruction: &Instruction) {
        let words = &instruction.words;
        if words.first().is_some_and(|w| w.text.contains('=')) {
            for word in words {
                if let Some((name, value)) = word.text.split_once('=') {
                    if is_name(name) {
                        self.define(instruction, "ENV", name, &unquote(value));
                    }
                }
            }
        } else if let Some(name) = words.first() {
            // The old form: `ENV name value with spaces`.
            let value: Vec<&str> = words[1..].iter().map(|w| w.text.as_str()).collect();
            if is_name(&name.text) {
                self.define(instruction, "ENV", &name.text, &value.join(" "));
            }
        }
    }

    fn copy(&mut self, instruction: &Instruction) {
        let words: Vec<&Word> = instruction
            .words
            .iter()
            .filter(|w| terminator_free(&w.text))
            .collect();
        if let Some(from) = instruction.flag("from") {
            let (value, span) = (from.value.clone(), from.span.clone());
            self.flag_ref(&value, &span);
            return;
        }
        let Some((dest, sources)) = words.split_last() else {
            return;
        };
        let scope = self.scope();
        let workdir = self
            .stages
            .last()
            .map_or_else(String::new, |s| s.workdir.clone());
        for source in sources {
            let text = unquote(&source.text);
            if is_remote(&text) {
                continue;
            }
            self.facts.push(Fact::PathMention(PathMention {
                site: source.span.clone(),
                enclosing: scope.clone(),
                text: text.clone(),
            }));
            let dest_text = unquote(&dest.text);
            let absolute = join_path(&workdir, &dest_text);
            let landed = if dest_text.ends_with('/') || sources.len() > 1 {
                let name = text.rsplit('/').next().unwrap_or(&text);
                join_path(&absolute, name)
            } else {
                absolute
            };
            if let Some(stage) = self.stages.last_mut() {
                stage.copies.push((landed, text));
            }
        }
    }

    fn run_instruction(&mut self, instruction: &Instruction) {
        if instruction.heredoc {
            return;
        }
        let scope = self.scope();
        for flag in instruction.flags.iter().filter(|f| f.name == "mount") {
            let pairs: Vec<(&str, &str)> = flag
                .value
                .split(',')
                .filter_map(|p| p.split_once('='))
                .collect();
            let get = |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
            if let Some(from) = get("from") {
                let (value, span) = (from.to_string(), flag.span.clone());
                self.flag_ref(&value, &span);
            } else if get("type") == Some("bind") {
                if let Some(source) = get("source").or_else(|| get("src")) {
                    self.facts.push(Fact::PathMention(PathMention {
                        site: flag.span.clone(),
                        enclosing: scope.clone(),
                        text: source.to_string(),
                    }));
                }
            }
        }
        let mut words = instruction.words.iter();
        let Some(mut script) = words.next() else {
            return;
        };
        if INTERPRETERS.contains(&script.text.as_str()) {
            let Some(next) = words.next() else { return };
            script = next;
        }
        let text = unquote(&script.text);
        let Some(stage) = self.stages.last() else {
            return;
        };
        let found = stage
            .copies
            .iter()
            .find(|(dest, _)| *dest == join_path(&stage.workdir, &text))
            .map(|(_, source)| source.clone())
            .or_else(|| {
                let whole = stage
                    .copies
                    .iter()
                    .any(|(_, source)| source == "." || source == "./");
                (whole && text.starts_with("./")).then(|| text.trim_start_matches("./").to_string())
            });
        if let Some(path) = found {
            self.facts.push(Fact::PathMention(PathMention {
                site: script.span.clone(),
                enclosing: scope,
                text: path,
            }));
        }
    }

    fn reads_in(&mut self, word: &Word) {
        let scope = self.scope();
        for name in vars_in(&word.text) {
            self.facts.push(Fact::Reference(Ref {
                site: word.span.clone(),
                enclosing: scope.clone(),
                receiver: Receiver::None,
                use_kind: UseKind::Read,
                name: Name::new(name),
                qualifier: Vec::new(),
            }));
        }
    }
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !text.starts_with(|c: char| c.is_ascii_digit())
}

/// `<<EOF` is a file the instruction writes, no path.
fn terminator_free(word: &str) -> bool {
    !word.starts_with("<<")
}

fn is_remote(text: &str) -> bool {
    text.contains("://") || text.starts_with("git@") || text.starts_with("github.com/")
}

fn join_path(base: &str, path: &str) -> String {
    if path.starts_with('/') || base.is_empty() {
        return path.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), path)
}

/// The names `$NAME` and `${NAME...}` read in a word.
fn vars_in(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\' {
            at += 2;
            continue;
        }
        if bytes[at] != b'$' {
            at += 1;
            continue;
        }
        at += 1;
        let braced = bytes.get(at) == Some(&b'{');
        if braced {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }
        if at > start && !bytes[start].is_ascii_digit() {
            found.push(text[start..at].to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_are_read_where_docker_expands_them() {
        assert_eq!(vars_in("${A}:$B-c"), ["A", "B"]);
        assert_eq!(vars_in("\\$A"), Vec::<String>::new());
        assert_eq!(vars_in("${V:-d}"), ["V"]);
    }
}
