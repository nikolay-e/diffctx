//! The Dockerfile extractor, read through the facts it states.

use _diffctx::facts::lang::dockerfile::DockerfileExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, Extractor, Fact, LocalKey, Namespace, Receiver, Ref, Scope,
};
use _diffctx::snapshot::RunBudget;

fn docker(source: &str) -> BlobFacts {
    DockerfileExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    if let Some(Disambig::Overload(n)) = &local.disambig {
        text.push_str(&format!("#{n}"));
    }
    if matches!(local.ns, Namespace::Type) {
        text.push_str("@type");
    }
    text
}

fn def_named<'a>(facts: &'a BlobFacts, wanted: &str) -> &'a Def {
    facts
        .defs()
        .find(|def| key(&def.local) == wanted)
        .unwrap_or_else(|| {
            let all: Vec<String> = facts.defs().map(|d| key(&d.local)).collect();
            panic!("no definition {wanted} in {all:?}")
        })
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => key(local),
        Scope::Callback { .. } => "callback".to_string(),
    }
}

/// `kind [qualifier] in scope`, for the references to `name`.
fn uses(facts: &BlobFacts, name: &str) -> Vec<String> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Reference(r) if r.name.as_str() == name => Some(r),
            _ => None,
        })
        .map(|r: &Ref| {
            assert!(matches!(r.receiver, Receiver::None));
            let qualifier: Vec<&str> = r.qualifier.iter().map(|n| n.as_str()).collect();
            format!(
                "{:?} [{}] in {}",
                r.use_kind,
                qualifier.join("."),
                scope(&r.enclosing)
            )
        })
        .collect()
}

fn mentions(facts: &BlobFacts) -> Vec<(String, u32)> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::PathMention(m) => Some((m.text.clone(), m.site.lines.0)),
            _ => None,
        })
        .collect()
}

const FILE: &str = "# syntax=docker/dockerfile:1
ARG BASE=alpine:3.20
FROM ${BASE} AS Build
ARG VERSION=1
ENV HOME=/app NAME=$VERSION
WORKDIR /app
COPY src/app.sh /usr/local/bin/app.sh
COPY . .
RUN /usr/local/bin/app.sh && ./extra.sh
RUN --mount=type=bind,source=conf,target=/c ls
FROM alpine:3.20
COPY --from=Build /app /out
COPY --from=0 /x /y
";

#[test]
fn stages_are_objects_named_as_written_or_by_position() {
    let facts = docker(FILE);
    let build = def_named(&facts, "Build@type");
    assert_eq!(build.kind, DefKind::Object);
    assert_eq!(build.signature.as_deref(), Some("FROM ${BASE}"));
    assert_eq!(build.span.lines, (3, 10));
    let second = def_named(&facts, "1@type");
    assert_eq!(second.span.lines, (11, 13));
    assert_eq!(second.signature.as_deref(), Some("FROM alpine:3.20"));
}

#[test]
fn arg_and_env_are_variables_of_their_stage() {
    let facts = docker(FILE);
    let sig = |wanted: &str| def_named(&facts, wanted).signature.clone().expect("sig");
    assert_eq!(def_named(&facts, "BASE").kind, DefKind::Variable);
    assert_eq!(sig("BASE"), "ARG BASE=alpine:3.20");
    assert_eq!(sig("Build.VERSION"), "ARG VERSION=1");
    assert_eq!(sig("Build.HOME"), "ENV HOME=/app");
    assert_eq!(sig("Build.NAME"), "ENV NAME=$VERSION");
}

#[test]
fn a_variable_is_read_where_docker_expands_it() {
    let facts = docker(FILE);
    assert_eq!(uses(&facts, "BASE"), ["Read [] in module"]);
    assert_eq!(uses(&facts, "VERSION"), ["Read [] in Build@type"]);
}

#[test]
fn a_stage_is_named_by_a_from_a_copy_or_a_mount() {
    let facts = docker(FILE);
    assert_eq!(uses(&facts, "Build"), ["Read [copy] in 1@type"]);
    assert_eq!(uses(&facts, "0"), ["Read [copy] in 1@type"]);
    let chained = docker("FROM alpine AS a\nFROM a AS b\nRUN --mount=from=a,target=/x true\n");
    assert_eq!(
        uses(&chained, "a"),
        ["Read [from] in b@type", "Read [copy] in b@type"]
    );
    // An untagged base may be an image as well: the resolver tells.
    assert_eq!(uses(&chained, "alpine"), ["Read [from] in a@type"]);
}

#[test]
fn places_are_sources_scripts_and_bind_mounts() {
    let facts = docker(FILE);
    assert_eq!(
        mentions(&facts),
        [
            ("src/app.sh".to_string(), 7),
            (".".to_string(), 8),
            ("src/app.sh".to_string(), 9),
            ("conf".to_string(), 10)
        ]
    );
    let none =
        docker("FROM a\nCOPY --from=a /x /y\nADD https://h/x.tgz /t\nCOPY <<EOF /f\nhi\nEOF\n");
    assert!(mentions(&none).is_empty());
}

#[test]
fn an_edit_inside_a_stage_moves_its_hash_and_no_other_stage_s() {
    let before = docker("FROM a AS x\nRUN one\nFROM b AS y\nRUN two\n");
    let after = docker("FROM a AS x\nRUN uno\nFROM b AS y\nRUN two\n");
    assert_ne!(
        def_named(&before, "x@type").body_hash,
        def_named(&after, "x@type").body_hash
    );
    assert_eq!(
        def_named(&before, "y@type").body_hash,
        def_named(&after, "y@type").body_hash
    );
}

#[test]
fn onbuild_and_heredoc_bodies_are_not_read() {
    let facts = docker("FROM a AS x\nONBUILD COPY ./gone /g\nRUN <<EOF\ncp ./x /y\nEOF\n");
    assert!(mentions(&facts).is_empty());
}

#[test]
fn a_variable_and_a_stage_read_are_sited_at_their_names_alone() {
    let source = "ARG V=1\nFROM a AS builder\nCOPY --from=builder /x /y\nRUN --mount=type=cache,from=builder,target=/c true\n";
    let facts = docker(source);
    let spelled =
        |span: &_diffctx::facts::Span| &source[span.bytes.start as usize..span.bytes.end as usize];
    assert_eq!(spelled(&def_named(&facts, "V").name_span), "V");
    let sites: Vec<&str> = facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Reference(r) if r.name.as_str() == "builder" => Some(spelled(&r.site)),
            _ => None,
        })
        .collect();
    assert_eq!(sites, ["builder", "builder"]);
}
