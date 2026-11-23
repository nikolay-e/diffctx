//! A Python class that lost a field with a default: only the constructions that
//! can pass the field are affected by its removal. A construction that does not
//! name it, hand it positional arguments or splat into it builds the class as
//! before; what the text cannot settle stays a site.

use std::ops::Range;

use diffctx_facts::facts::{Def, DefChange, DefKey, DefKind, Lexical, Name, SemanticDef, facts_of};

use diffctx_resolve::resolve::{Binding, Definition, Relation};

use diffctx_snapshot::snapshot::{RepoPath, SideTag};

use super::super::changed::bases_in;

use super::super::verdict::{Reach, Subject};

use super::Cell;

/// What an argument list passes: the names it gives by keyword, and whether it
/// holds an argument no name settles (positional, `*args`, `**kwargs`, or a
/// `default=...`, which marks a field required).
struct Arguments {
    keywords: Vec<String>,
    open: bool,
}

impl Arguments {
    fn take(&mut self, segment: &[u8]) {
        let segment = segment.trim_ascii();
        if segment.is_empty() {
            return;
        }
        let word = segment
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || !c.is_ascii())
            .count();
        match segment[word..].trim_ascii_start() {
            [b'=', next, ..] if word > 0 && *next != b'=' => {
                let value = segment[word..].trim_ascii_start()[1..].trim_ascii();
                if &segment[..word] == b"default" && value == b"..." {
                    self.open = true;
                } else {
                    self.keywords
                        .push(String::from_utf8_lossy(&segment[..word]).into_owned());
                }
            }
            _ => self.open = true,
        }
    }
}

/// Whether a class signature lists a base written as a call: `class A(make())`.
fn call_base(signature: Option<&str>) -> bool {
    let Some(text) = signature else {
        return false;
    };
    let (Some(open), Some(close)) = (text.find('('), text.rfind(')')) else {
        return false;
    };
    text.get(open + 1..close).is_some_and(|bases| {
        bases
            .split(',')
            .any(|base| !base.contains('=') && base.contains('('))
    })
}

fn open_paren(text: &[u8], at: usize) -> Option<usize> {
    let skipped = text
        .get(at..)?
        .iter()
        .take_while(|c| matches!(**c, b' ' | b'\t'))
        .count();
    (text.get(at + skipped) == Some(&b'(')).then_some(at + skipped)
}

/// The arguments of the call whose callee ends at `at`; `None` when no call
/// follows or its parentheses do not close.
fn arguments(text: &[u8], at: usize) -> Option<Arguments> {
    let open = open_paren(text, at)?;
    let mut found = Arguments {
        keywords: Vec::new(),
        open: false,
    };
    let (mut depth, mut start) = (0usize, open + 1);
    for (i, &c) in text.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    found.take(&text[start..i]);
                    return Some(found);
                }
            }
            b',' if depth == 1 => {
                found.take(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// Where a statement assigns, outside every bracket: the `=` of `name: type = value`,
/// never the one of `==`, `<=`, `!=` or `:=`.
fn assignment(statement: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in statement.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'=' if depth == 0 => {
                let before = i.checked_sub(1).map(|at| statement[at]);
                if statement.get(i + 1) != Some(&b'=')
                    && !matches!(before, Some(b'=' | b'!' | b'<' | b'>' | b':'))
                {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the class-body statement at `span` gives its name a default a
/// construction may leave out. A `field(..)` or `Field(..)` call has one only
/// when it says so; any other call, and `...`, count as none.
fn defaulted(text: &[u8], span: Range<usize>) -> bool {
    let Some(statement) = text.get(span.clone()) else {
        return false;
    };
    let Some(equals) = assignment(statement) else {
        return false;
    };
    let value = statement[equals + 1..].trim_ascii_start();
    if value.is_empty() || value.starts_with(b"...") {
        return false;
    }
    let word = value
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'.')
        .count();
    let after = span.end - value.len() + word;
    if word == 0 || open_paren(text, after).is_none() {
        return true;
    }
    let specifier = matches!(
        value[..word].rsplit(|c| *c == b'.').next(),
        Some(b"field" | b"Field" | b"ib" | b"attrib")
    );
    specifier
        && arguments(text, after).is_some_and(|args| {
            !args.open
                && args
                    .keywords
                    .iter()
                    .any(|k| matches!(k.as_str(), "default" | "default_factory" | "factory"))
        })
}

impl Cell {
    /// The file's bytes with its comments blanked and its strings filled with
    /// `0`, so that only code is left to read the brackets and names of.
    fn code_of(&self, side: SideTag, path: &RepoPath) -> Option<Vec<u8>> {
        let reader = match side {
            SideTag::Base => &self.ctx.base,
            SideTag::Head => &self.ctx.head,
        };
        let mut text = reader.read(path).ok()??.to_vec();
        let facts = facts_of(&self.ctx, side, &self.cache, path).ok()??;
        for (range, kind) in &facts.blob.lexical {
            let fill = match kind {
                Lexical::Comment => b' ',
                Lexical::String => b'0',
            };
            if let Some(bytes) = text.get_mut(range.start as usize..range.end as usize) {
                bytes.fill(fill);
            }
        }
        Some(text)
    }

    /// The names a construction of `class` must pass to be affected by the
    /// removal of `field`. `None`: every construction is, because the field had
    /// no default, or its name is not what a construction passes (an alias, an
    /// underscore, a base class of the repository that may rename its fields),
    /// or the text does not say.
    pub(super) fn passed_to_reach(
        &self,
        file: &str,
        field: &DefKey,
        class: &Definition,
    ) -> Option<Vec<Name>> {
        let path = RepoPath::new(file).ok()?;
        let def = self.declared(SideTag::Base, &path, field).ok()??;
        if !matches!(
            def.kind,
            DefKind::Field | DefKind::Variable | DefKind::Constant
        ) {
            return None;
        }
        let holder = self.declared(SideTag::Base, &path, &class.key).ok()??;
        let bases_are_outside = bases_in(holder.signature.as_deref()).iter().all(|base| {
            !(self.head.definitions().iter())
                .any(|d| d.kind == DefKind::Class && d.key.local.name.as_str() == base.as_str())
        });
        if !bases_are_outside
            || self.subclassed(class)
            || self.builds_its_own_constructor(&path, class, &holder)
        {
            return None;
        }
        let code = self.code_of(SideTag::Base, &path)?;
        let renames = code.windows(5).any(|word| word == b"alias");
        let span = def.span.bytes.start as usize..def.span.bytes.end as usize;
        (!renames && !field.local.name.as_str().starts_with('_') && defaulted(&code, span))
            .then(|| vec![field.local.name.clone()])
    }

    /// Whether a class of the repository extends `class`, by its name, by the
    /// name an import gives it, or by a module-level name bound to it: the
    /// constructions of a subclass run the constructor too, and none of them is
    /// a use of the class. A base that is a call may return any class, so it
    /// counts for every class.
    fn subclassed(&self, class: &Definition) -> bool {
        let mut names = vec![class.key.local.name.as_str().to_string()];
        for b in self.head.references_to(&class.key) {
            names.push(b.name.as_str().to_string());
            let holder =
                (self.key_in(b.file.as_str(), &b.from)).and_then(|key| self.definition(&key));
            if let Some(d) =
                holder.filter(|d| matches!(d.kind, DefKind::Variable | DefKind::Constant))
            {
                names.push(d.key.local.name.as_str().to_string());
            }
        }
        self.head.definitions().iter().any(|d| {
            d.kind == DefKind::Class
                && d.key != class.key
                && self
                    .declared(SideTag::Head, &d.file, &d.key)
                    .ok()
                    .flatten()
                    .is_some_and(|decl| {
                        let signature = decl.signature.as_deref();
                        bases_in(signature).iter().any(|base| names.contains(base))
                            || call_base(signature)
                    })
        })
    }

    /// Whether the class's constructor is not the one its decorator builds from
    /// its fields: an `__init__` of its own, or `init=False`.
    fn builds_its_own_constructor(&self, path: &RepoPath, class: &Definition, base: &Def) -> bool {
        let mut inside = class.key.local.container.clone();
        inside.push(class.key.local.name.clone());
        let is_init = |local: &diffctx_facts::facts::LocalKey| {
            local.name.as_str() == "__init__" && local.container == inside
        };
        base.decorators
            .iter()
            .any(|d| d.as_str().replace(' ', "").contains("init=False"))
            || self
                .head
                .definitions()
                .iter()
                .any(|d| is_init(&d.key.local))
            || facts_of(&self.ctx, SideTag::Base, &self.cache, path)
                .ok()
                .flatten()
                .is_none_or(|facts| facts.defs().any(|d| is_init(&d.local)))
    }

    /// Whether `class` changed in nothing but removed fields a construction may
    /// leave out, under a decorator that only builds its constructor from its
    /// fields: the removals' own subjects are the whole change, and the class
    /// as such, which a decorator may read, is not changed.
    pub(super) fn lost_only_omittable_fields(
        &self,
        class: &SemanticDef,
        head: &Definition,
        changes: &[&SemanticDef],
    ) -> bool {
        let Some(file) = class.head.as_ref() else {
            return false;
        };
        let Ok(Some(declared)) = self.declared(SideTag::Head, file, &class.key) else {
            return false;
        };
        let builds_constructor = !declared.decorators.is_empty()
            && declared.decorators.iter().all(|decorator| {
                let callee = decorator.as_str().split('(').next().unwrap_or_default();
                matches!(
                    callee.rsplit('.').next(),
                    Some("dataclass" | "define" | "frozen" | "mutable" | "attrs" | "s")
                )
            });
        let mut inside = class.key.local.container.clone();
        inside.push(class.key.local.name.clone());
        let mut members = changes
            .iter()
            .filter(|m| m.key.local.container.starts_with(&inside))
            .peekable();
        builds_constructor
            && members.peek().is_some()
            && members.all(|m| {
                m.key.local.container == inside
                    && m.head.is_none()
                    && matches!(m.change, DefChange::Removed)
                    && m.base.as_ref().is_some_and(|base| {
                        self.passed_to_reach(base.as_str(), &m.key, head).is_some()
                    })
            })
    }

    /// Whether the construction at `b` leaves out every field `subject` lost.
    pub(super) fn leaves_out(&self, b: &Binding, subject: &Subject) -> bool {
        let (Reach::Construction, Some(names)) = (subject.reach, &subject.passing) else {
            return false;
        };
        if !matches!(b.relation, Relation::Instantiates | Relation::Calls) {
            return false;
        }
        let Some(code) = self.code_of(SideTag::Head, &b.file) else {
            return false;
        };
        arguments(&code, b.site.bytes.end as usize).is_some_and(|args| {
            !args.open
                && !args
                    .keywords
                    .iter()
                    .any(|k| names.iter().any(|n| n.as_str() == k))
        })
    }
}
