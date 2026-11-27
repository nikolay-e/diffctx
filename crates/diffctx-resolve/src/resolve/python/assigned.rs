//! What a call makes and what a variable is assigned: the object a name holds
//! when the code builds it, returns it or copies it from another name.

use super::super::{EvidenceKind, UnresolvedReason};
use super::{Engine, Entity, Lead, Lookup, MAX_DEPTH, Outcome};
use diffctx_facts::facts::{DefKind, Fact, Name, Ref, UseKind};

impl<'a> Engine<'a> {
    /// The object a call makes: an instance of the class it constructs, or of
    /// the class a function annotated `-> Cls` returns.
    pub(super) fn call_result(
        &self,
        f: usize,
        scope: &[Name],
        chain: &[Name],
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        if let [only] = chain {
            if only.as_str() == "super" {
                let class = self.receiver_name(f, scope).map(|(_, class)| class);
                return (
                    class.map_or(Entity::Unknown, Entity::Super),
                    EvidenceKind::SuperCall,
                );
            }
        }
        if depth >= MAX_DEPTH {
            return (Entity::Unknown, EvidenceKind::Constructed);
        }
        match self.path_entity(f, scope, chain, depth + 1) {
            Entity::Class(c) => (Entity::Instance(c), EvidenceKind::Constructed),
            Entity::Def(id) => (self.return_instance(id), EvidenceKind::FactoryReturn),
            _ => (Entity::Unknown, EvidenceKind::Constructed),
        }
    }

    /// A local variable assigned from a call: `model = await Model.from_config()`.
    /// Every assignment of the name in the function must agree.
    pub(super) fn local_entity(
        &self,
        f: usize,
        scope: &[Name],
        name: &Name,
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        let file = &self.ix.files[f];
        for level in (0..=scope.len()).rev() {
            let key = (scope[..level].to_vec(), name.clone());
            let Some(assignments) = file.variables.get(&key) else {
                continue;
            };
            let slot = (f, key.0.clone(), name.clone());
            if let Some(known) = self.assumed.borrow().get(&slot) {
                return (known.clone(), EvidenceKind::Constructed);
            }
            let mut found: Option<(Entity, EvidenceKind)> = None;
            for at in assignments {
                // `x = x.copy_with(...)`: what the name held before is the type
                // its next assignment is read under.
                if let Some((entity, _)) = &found {
                    self.assumed
                        .borrow_mut()
                        .insert(slot.clone(), entity.clone());
                }
                let next = self.assigned_call(f, *at, depth);
                self.assumed.borrow_mut().remove(&slot);
                let Some(next) = next else {
                    return (Entity::Unknown, EvidenceKind::Constructed);
                };
                match &found {
                    Some((entity, _)) if *entity != next.0 => {
                        return (Entity::Unknown, EvidenceKind::Constructed);
                    }
                    _ => found = Some(next),
                }
            }
            return found.unwrap_or((Entity::Unknown, EvidenceKind::Constructed));
        }
        (Entity::Unknown, EvidenceKind::Constructed)
    }

    /// The object the variable bound by the fact at `binding` is assigned.
    fn assigned_call(
        &self,
        f: usize,
        binding: usize,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::LocalBinding(local) = facts.get(binding)? else {
            return None;
        };
        if self.literal_after(f, local.site.bytes.end) {
            return Some((Entity::Builtin, EvidenceKind::Constructed));
        }
        self.call_assigned_after(f, binding + 1, local.site.bytes.end, depth)
            .or_else(|| self.entered_call(f, local.site.bytes.start, depth))
            .or_else(|| self.alias_after(f, binding + 1, local.site.bytes.end, depth))
    }

    /// The object made by the call a `with` item binds the variable to:
    /// `with Cls(...) as v` / `async with Cls(...) as v`, the call read back from
    /// the text before `as` and found among the facts by where its name ends.
    fn entered_call(&self, f: usize, binding: u32, depth: usize) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let text = self.text(f)?;
        let blank = |b: &u8| b.is_ascii_whitespace();
        let mut end = binding as usize;
        let skip_back = |mut at: usize| {
            while at > 0 && blank(&text[at - 1]) {
                at -= 1;
            }
            at
        };
        end = skip_back(end);
        if end < 3
            || &text[end - 2..end] != b"as"
            || !blank(&text[end - 3]) && text[end - 3] != b')'
        {
            return None;
        }
        end = skip_back(end - 2);
        if end == 0 || text[end - 1] != b')' {
            return None;
        }
        let mut open = end - 1;
        let mut depth_parens = 0usize;
        loop {
            match text[open] {
                b')' => depth_parens += 1,
                b'(' => {
                    depth_parens -= 1;
                    if depth_parens == 0 {
                        break;
                    }
                }
                _ => {}
            }
            if open == 0 {
                return None;
            }
            open -= 1;
        }
        let name_end = skip_back(open);
        let facts = &self.ix.files[f].facts.blob.facts;
        let at = facts.partition_point(|fact| (fact.site().bytes.start as usize) < name_end);
        let Fact::Reference(callee) = facts.get(at.checked_sub(1)?)? else {
            return None;
        };
        if callee.use_kind != UseKind::Call || callee.site.bytes.end as usize != name_end {
            return None;
        }
        // The chain written before the name is the one the facts qualify it with.
        let chain: String = callee.qualifier.iter().map(|q| format!("{q}.")).collect();
        let mut from = callee.site.bytes.start as usize;
        for want in chain.bytes().rev() {
            from = skip_back(from);
            if from == 0 || text[from - 1] != want {
                return None;
            }
            from -= 1;
        }
        match self.callee_result(f, callee, depth)? {
            (Entity::Instance(manager), evidence) => Some((self.entered(manager), evidence)),
            made => Some(made),
        }
    }

    /// What `with` binds for a manager of the project: what its `__enter__` hands back,
    /// the manager itself only when that is `return self`. A manager that defines none
    /// here is bound as made.
    fn entered(&self, manager: usize) -> Entity {
        let enter = ["__enter__", "__aenter__"].into_iter().find_map(|hook| {
            match self.class_member(manager, &Name::new(hook), false) {
                Lookup::Found {
                    entity: Entity::Def(id),
                    ..
                } => Some(id),
                _ => None,
            }
        });
        let Some(enter) = enter else {
            return Entity::Instance(manager);
        };
        match self.return_instance(enter) {
            Entity::Unknown if self.hands_back_self(enter) => Entity::Instance(manager),
            handed => handed,
        }
    }

    fn hands_back_self(&self, id: usize) -> bool {
        let rec = &self.ix.defs[id];
        let span = rec.def.span.bytes.start as usize..rec.def.span.bytes.end as usize;
        let Some(text) = self.text(rec.file) else {
            return false;
        };
        text.get(span).is_some_and(|body| {
            String::from_utf8_lossy(body)
                .lines()
                .any(|line| line.trim() == "return self")
        })
    }

    /// The object made by the call that is the fact `next`, when the text from
    /// `after` to the call is `= [await] chain.` and the call ends the statement.
    fn call_assigned_after(
        &self,
        f: usize,
        next: usize,
        after: u32,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        self.call_after(f, next, after, Lead::Assign, depth)
    }

    /// The object made by the call that is the fact `next`, when the text from
    /// `after` to it is `lead [await] chain.` and the call ends the statement.
    fn call_after(
        &self,
        f: usize,
        next: usize,
        after: u32,
        lead: Lead,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::Reference(callee) = facts.get(next)? else {
            return None;
        };
        if callee.use_kind != UseKind::Call {
            return None;
        }
        self.chain_follows(f, after, lead, callee, true)?;
        self.callee_result(f, callee, depth)
    }

    /// The text from `after` to `source` is `lead [await] chain.`, the chain
    /// written before the name as the facts qualify it, and the expression ends
    /// its statement: what a variable is assigned is that one reference. With
    /// `call`, the reference is a callee and the statement ends with its call.
    fn chain_follows(
        &self,
        f: usize,
        after: u32,
        lead: Lead,
        source: &Ref,
        call: bool,
    ) -> Option<()> {
        let text = self.text(f)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "the slice is read for an assignment shape; bytes that are not UTF-8 show none, so nothing is inferred from them"
        )]
        let between =
            std::str::from_utf8(text.get(after as usize..source.site.bytes.start as usize)?)
                .ok()?;
        let mut rest = match lead {
            Lead::Assign => {
                let equals = between.find('=')?;
                let before = between[..equals].trim();
                let after_equals = &between[equals + 1..];
                if !(before.is_empty() || before.starts_with(':') && !before.contains('\n'))
                    || after_equals.starts_with('=')
                {
                    return None;
                }
                after_equals.trim_start()
            }
            Lead::Return => between.trim_start(),
        };
        if let Some(awaited) = rest.strip_prefix("await") {
            if awaited.starts_with(char::is_whitespace) {
                rest = awaited.trim_start();
            }
        }
        let written: String = rest.split_whitespace().collect();
        let expected: String = source.qualifier.iter().map(|q| format!("{q}.")).collect();
        let end = source.site.bytes.end as usize;
        let ends = if call {
            ends_statement(&text, end)
        } else {
            name_ends_statement(&text, end)
        };
        (written == expected && ends).then_some(())
    }

    /// What a variable assigned a name or an attribute chain holds, when the
    /// chain is static: `internals = options.internals`,
    /// `retry = self.options.retry`. The fact at `next` is the chain's last name,
    /// read; what it resolves to is what the variable holds.
    fn alias_after(
        &self,
        f: usize,
        next: usize,
        after: u32,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::Reference(source) = facts.get(next)? else {
            return None;
        };
        if source.use_kind != UseKind::Read {
            return None;
        }
        self.chain_follows(f, after, Lead::Assign, source, false)?;
        match self.outcome(f, source, depth + 1) {
            Outcome::Found {
                entity:
                    held @ (Entity::Class(_)
                    | Entity::Instance(_)
                    | Entity::Def(_)
                    | Entity::Module(_)
                    | Entity::Package(_)
                    | Entity::Dynamic),
                ..
            } => Some((held, EvidenceKind::Aliased)),
            _ => None,
        }
    }

    /// What calling `callee` makes: the language's own object, an object of a
    /// class of a package outside the repository (a capitalised name: the type is
    /// known and not ours), an instance of a class of the repository, or what a
    /// function returns. What a function of outside code returns is unknown: it
    /// may hand back an object of ours (`pytest.raises(E).value`).
    fn callee_result(
        &self,
        f: usize,
        callee: &Ref,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        match self.outcome(f, callee, depth + 1) {
            // What `getattr`, `eval` and `__import__` hand back is whatever the name
            // says: a module or an object of ours.
            Outcome::Unresolved(UnresolvedReason::Builtin)
                if !matches!(callee.name.as_str(), "getattr" | "eval" | "__import__") =>
            {
                Some((Entity::Builtin, EvidenceKind::Constructed))
            }
            Outcome::Unresolved(UnresolvedReason::External)
            | Outcome::Found {
                entity: Entity::External,
                ..
            } if callee.name.as_str().starts_with(char::is_uppercase) => {
                Some((Entity::External, EvidenceKind::Constructed))
            }
            Outcome::Found {
                entity: Entity::Class(c),
                ..
            } => Some((Entity::Instance(c), EvidenceKind::Constructed)),
            Outcome::Found {
                entity: Entity::Def(id),
                ..
            } => {
                // A variable that holds a class makes an instance of it when called.
                let made = match self.ix.defs[id].def.kind {
                    DefKind::Variable | DefKind::Constant | DefKind::Field => {
                        match self.var_instance(id) {
                            Entity::Class(c) => Entity::Instance(c),
                            _ => Entity::Unknown,
                        }
                    }
                    _ => self.return_instance(id),
                };
                match made {
                    Entity::Unknown => None,
                    entity => Some((entity, EvidenceKind::FactoryReturn)),
                }
            }
            _ => None,
        }
    }

    /// Whether the text after `after` is `= <literal>`: a list, a dict, a string,
    /// a number. What it binds is the language's own object, never a class of the
    /// repository.
    fn literal_after(&self, f: usize, after: u32) -> bool {
        let Some(text) = self.text(f) else {
            return false;
        };
        let Some(tail) = text.get(after as usize..) else {
            return false;
        };
        let tail = String::from_utf8_lossy(&tail[..tail.len().min(96)]).into_owned();
        let line = tail.split('\n').next().unwrap_or("");
        let Some((before, rest)) = line.split_once('=') else {
            return false;
        };
        let before = before.trim();
        if !(before.is_empty() || before.starts_with(':')) || rest.starts_with('=') {
            return false;
        }
        let mut chars = rest.trim_start().chars();
        match chars.next() {
            Some('[' | '{' | '"' | '\'') => true,
            Some(c) if c.is_ascii_digit() => true,
            Some('f' | 'b' | 'r' | 'u' | 'F' | 'B' | 'R' | 'U') => {
                matches!(chars.next(), Some('"' | '\''))
            }
            _ => false,
        }
    }

    /// The instance of the class a function's `-> Cls` annotation names; without
    /// an annotation, the class of its one `return Cls(...)`.
    pub(super) fn return_instance(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        let annotation = rec
            .def
            .signature
            .as_deref()
            .and_then(|signature| signature.rfind("->").map(|at| &signature[at + 2..]));
        match annotation {
            Some(written) => self.type_entity(rec.file, &rec.def.local.container, written),
            None => self.sole_return(id),
        }
    }

    fn sole_return(&self, id: usize) -> Entity {
        if let Some(known) = self.returns.borrow().get(&id) {
            return known.clone();
        }
        self.returns.borrow_mut().insert(id, Entity::Unknown);
        let found = self.compute_sole_return(id);
        self.returns.borrow_mut().insert(id, found.clone());
        found
    }

    /// The function has one `return`, no `yield` and no function or class inside
    /// it, and the `return` hands back a call: what the call makes, the instance
    /// of a constructor it names.
    fn compute_sole_return(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        if !matches!(rec.def.kind, DefKind::Function | DefKind::Method) {
            return Entity::Unknown;
        }
        let f = rec.file;
        let Some(text) = self.text(f) else {
            return Entity::Unknown;
        };
        let from = rec.def.span.bytes.start as usize;
        #[allow(
            clippy::disallowed_methods,
            reason = "a body that is not UTF-8 resolves as `Entity::Unknown`, which callers keep unresolved"
        )]
        let Some(body) = text
            .get(from..rec.def.span.bytes.end as usize)
            .and_then(|b| std::str::from_utf8(b).ok())
        else {
            return Entity::Unknown;
        };
        if body
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == "yield")
        {
            return Entity::Unknown;
        }
        let (mut returns, mut defined, mut offset) = (Vec::new(), 0usize, from);
        for line in body.split_inclusive('\n') {
            let trimmed = line.trim_start();
            if ["def ", "async def ", "class "]
                .iter()
                .any(|keyword| trimmed.starts_with(keyword))
            {
                defined += 1;
            }
            if let Some(after) = trimmed.strip_prefix("return") {
                if after.is_empty() || after.starts_with(|c: char| c.is_whitespace() || c == '(') {
                    returns.push(offset + (line.len() - trimmed.len()) + "return".len());
                }
            }
            offset += line.len();
        }
        let ([after], 1) = (returns.as_slice(), defined) else {
            return Entity::Unknown;
        };
        let facts = &self.ix.files[f].facts.blob.facts;
        let at = facts.partition_point(|fact| (fact.site().bytes.start as usize) < *after);
        match self.call_after(f, at, *after as u32, Lead::Return, 1) {
            Some((entity, _)) => entity,
            None => Entity::Unknown,
        }
    }

    pub(super) fn compute_var_instance(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        if let Some((_, annotation)) = rec.def.signature.as_deref().and_then(|s| s.split_once(':'))
        {
            let typed = self.type_entity(rec.file, &rec.def.local.container, annotation);
            if typed != Entity::Unknown {
                return typed;
            }
        }
        if self.literal_after(rec.file, rec.def.name_span.bytes.end) {
            return Entity::Builtin;
        }
        let after = rec.def.name_span.bytes.end;
        let value = self.value_fact(rec.file, rec.fact + 1, after);
        match self
            .call_assigned_after(rec.file, value, after, 0)
            .or_else(|| self.alias_after(rec.file, value, after, 0))
        {
            Some((entity, _)) => entity,
            None => Entity::Unknown,
        }
    }

    /// The fact that starts the value a declaration assigns: the facts the
    /// annotation between the name and the `=` holds are the type's, not the value's.
    fn value_fact(&self, f: usize, next: usize, after: u32) -> usize {
        let Some(text) = self.text(f) else {
            return next;
        };
        let line = text.get(after as usize..).map_or(&[][..], |tail| {
            tail.split(|b| *b == b'\n').next().unwrap_or(tail)
        });
        let Some(equals) = line.iter().position(|b| *b == b'=') else {
            return next;
        };
        let start = after + equals as u32 + 1;
        let facts = &self.ix.files[f].facts.blob.facts;
        next + facts[next.min(facts.len())..]
            .partition_point(|fact| fact.site().bytes.start < start)
    }
}

/// Whether a name that ends at `at` is the whole of its statement: nothing but
/// blanks and a comment follows on the line.
fn name_ends_statement(text: &[u8], mut at: usize) -> bool {
    while text.get(at).is_some_and(|b| *b == b' ' || *b == b'\t') {
        at += 1;
    }
    text.get(at)
        .is_none_or(|b| matches!(*b, b'\n' | b'\r' | b'#' | b';'))
}

/// Whether the call whose callee ends at `at` is the end of its statement.
fn ends_statement(text: &[u8], mut at: usize) -> bool {
    let blank = |b: u8| b == b' ' || b == b'\t';
    while text.get(at).is_some_and(|b| blank(*b)) {
        at += 1;
    }
    if text.get(at) != Some(&b'(') {
        return false;
    }
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    while let Some(&c) = text.get(at) {
        if let Some(q) = quote {
            if c == b'\\' {
                at += 1;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                b'"' | b'\'' => quote = Some(c),
                b'#' => {
                    while text.get(at).is_some_and(|b| *b != b'\n') {
                        at += 1;
                    }
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        at += 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        at += 1;
    }
    if depth != 0 {
        return false;
    }
    while text.get(at).is_some_and(|b| blank(*b)) {
        at += 1;
    }
    text.get(at)
        .is_none_or(|b| matches!(*b, b'\n' | b'\r' | b'#' | b';'))
}
