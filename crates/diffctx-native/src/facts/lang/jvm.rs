//! What the Java and Kotlin readers write once: both walk their tree into the same
//! facts, so the methods that read it the same way are one macro, not two copies.

/// The reader methods that Java and Kotlin share, written once.
macro_rules! shared_reader_methods {
    () => {
        fn tick(&mut self) -> bool {
            self.ticks += 1;
            if self.ticks % POLL_EVERY == 0 && expired(self.budget) {
                self.halted = true;
            }
            self.halted
        }

        fn span(&self, range: Range<usize>) -> Span {
            self.line_starts.span(range)
        }

        fn slice(&self, range: Range<usize>) -> &'a str {
            let end = range.end.min(self.bytes.len());
            let start = range.start.min(end);
            std::str::from_utf8(&self.bytes[start..end]).unwrap_or("")
        }

        fn package(&mut self) {
            let Some(package) = self.read.package.as_deref() else {
                return;
            };
            let at = self
                .line_starts
                .iter()
                .copied()
                .find(|start| {
                    self.slice(*start..(*start + 7).min(self.bytes.len()))
                        .starts_with("package")
                })
                .unwrap_or(0);
            let end = self
                .slice(at..(at + 512).min(self.bytes.len()))
                .find('\n')
                .map_or((at + 512).min(self.bytes.len()), |n| at + n);
            self.facts.push(Fact::Import(Import {
                site: self.span(at..end.max(at + 1)),
                enclosing: Scope::Module,
                module: ModulePath::new(package),
                relative: 0,
                kind: ImportKind::SideEffect,
                names: Vec::new(),
                star: Star::None,
                default: None,
            }));
        }

        /// The ranges of the declarations written inside `index`, outermost only.
        fn nested(&self, index: usize) -> Vec<Range<usize>> {
            let decls = &self.read.declarations;
            let span = &decls[index].span;
            let mut out: Vec<Range<usize>> = Vec::new();
            for other in &decls[index + 1..] {
                if other.span.start >= span.end {
                    break;
                }
                if other.span.end > span.end || other.span.start < span.start {
                    continue;
                }
                if out.last().is_some_and(|last| other.span.start < last.end) {
                    continue;
                }
                out.push(other.span.clone());
            }
            out
        }

        fn doc_hash(&self, start: usize) -> Option<u64> {
            let before = self.leaves.partition_point(|l| l.end <= start);
            let leaf = self.leaves[..before].last()?;
            if !leaf.comment || !self.slice(leaf.start..leaf.end).starts_with("/**") {
                return None;
            }
            if !self.slice(leaf.end..start).trim().is_empty() {
                return None;
            }
            let text = self.slice(leaf.start..leaf.end);
            hash_tokens(
                text.split(|c: char| c.is_whitespace() || c == '*' || c == '/')
                    .filter(|w| !w.is_empty())
                    .map(str::as_bytes),
            )
        }

        /// The innermost declaration holding each of the (ascending) bytes asked.
        fn owners(&self, bytes: &[usize]) -> Vec<Option<usize>> {
            let decls = &self.read.declarations;
            let mut out = Vec::with_capacity(bytes.len());
            let mut stack: Vec<usize> = Vec::new();
            let mut next = 0;
            for &byte in bytes {
                while next < decls.len() && decls[next].span.start <= byte {
                    stack.push(next);
                    next += 1;
                }
                while let Some(&top) = stack.last() {
                    if decls[top].span.end <= byte {
                        stack.pop();
                    } else {
                        break;
                    }
                }
                out.push(stack.last().copied());
            }
            out
        }

        fn scope_of(&self, owner: Option<usize>) -> Scope {
            owner.map_or(Scope::Module, |at| Scope::Def(self.keys[at].clone()))
        }

        fn receiver_of(&self, u: &old::Use) -> (Receiver, Vec<Name>) {
            let name = |s: &String| Name::new(s.as_str());
            match &u.receiver {
                None => (Receiver::None, Vec::new()),
                Some(old::Receiver::This) => (Receiver::SelfType, Vec::new()),
                Some(old::Receiver::Expr) => (Receiver::Unknown, Vec::new()),
                Some(old::Receiver::Call(callee)) => {
                    (Receiver::CallResult(vec![name(callee)]), Vec::new())
                }
                Some(old::Receiver::Name(head)) => self.chain(u.at, std::slice::from_ref(head)),
                Some(old::Receiver::Path(parts)) => match parts.split_first() {
                    Some((first, rest)) if first == "this" => {
                        (Receiver::SelfType, rest.iter().map(name).collect())
                    }
                    _ => self.chain(u.at, parts),
                },
            }
        }

        fn locals(&mut self) {
            let mut order: Vec<usize> = (0..self.read.bindings.len()).collect();
            order.sort_by_key(|i| self.read.bindings[*i].start);
            let starts: Vec<usize> = order.iter().map(|i| self.read.bindings[*i].start).collect();
            let owners = self.owners(&starts);
            for (i, owner) in order.into_iter().zip(owners) {
                if self.tick() {
                    return;
                }
                let b = &self.read.bindings[i];
                let end = b.scope.end.max(b.start + 1);
                self.facts.push(Fact::LocalBinding(Local {
                    name: Name::new(b.name.as_str()),
                    site: self.span(b.start..end),
                    enclosing: self.scope_of(owner),
                    kind: if b.start == b.scope.start {
                        LocalKind::Parameter
                    } else {
                        LocalKind::Variable
                    },
                }));
            }
        }
    };
}

pub(super) use shared_reader_methods;
