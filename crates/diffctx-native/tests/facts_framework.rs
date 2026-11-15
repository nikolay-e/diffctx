//! The entries a framework makes, read off decorators and annotations: one rule
//! table for every language, so each case here states the same fact whichever
//! extractor read the source. The negative cases are as much the contract as the
//! positive ones: an unimported framework, or a name some other object owns, is
//! no entry.

use _diffctx::facts::framework::describe;
use _diffctx::facts::lang::java::JavaExtractor;
use _diffctx::facts::lang::kotlin::KotlinExtractor;
use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::lang::typescript::TypeScript;
use _diffctx::facts::{BlobFacts, Extractor, Fact, Scope};
use _diffctx::snapshot::RunBudget;

/// `kind target framework verb label` of every entry a framework makes.
fn entries(blob: &BlobFacts) -> Vec<String> {
    blob.facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::EntryPoint(entry) => {
                let framework = entry.framework.as_ref()?;
                let target = match &entry.target {
                    Scope::Def(local) => {
                        let mut parts: Vec<&str> =
                            local.container.iter().map(|n| n.as_str()).collect();
                        parts.push(local.name.as_str());
                        parts.join(".")
                    }
                    other => format!("{other:?}"),
                };
                Some(format!(
                    "{:?} {target} {} {} {}",
                    entry.kind,
                    framework.name,
                    framework.verb.as_ref().map_or("-", |v| v.as_str()),
                    framework.label.as_deref().unwrap_or("-"),
                ))
            }
            _ => None,
        })
        .collect()
}

fn python(source: &str) -> Vec<String> {
    entries(&PythonExtractor.extract(source.as_bytes(), &RunBudget::default()))
}

#[test]
fn flask_and_fastapi_routes_state_their_verb_and_path() {
    let found = python(
        "from flask import Flask\n\napp = Flask(__name__)\n\n\n@app.route(\"/items\", methods=[\"POST\", \"put\"])\ndef write():\n    pass\n\n\n@app.route(\"/items/<id>\")\ndef read(id):\n    pass\n\n\n@app.get('/health')\ndef health():\n    pass\n",
    );
    assert_eq!(
        found,
        [
            "Route write flask POST /items",
            "Route write flask PUT /items",
            "Route read flask GET /items/<id>",
            "Route health flask GET /health",
        ]
    );
    let found = python(
        "from fastapi import APIRouter\n\nrouter = APIRouter()\n\n\n@router.delete(\"/reviews/{rid}\")\nasync def drop(rid: int):\n    pass\n",
    );
    assert_eq!(found, ["Route drop fastapi DELETE /reviews/{rid}"]);
}

#[test]
fn a_method_of_an_object_the_file_does_not_take_from_a_framework_is_no_route() {
    let found = python(
        "import functools\n\ncache = functools.lru_cache\n\n\n@cache.get(\"/items\")\ndef read():\n    pass\n",
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn commands_tools_fixtures_and_jobs_are_named_by_what_the_source_gives() {
    let found = python(
        "import click\nimport pytest\nfrom apscheduler.schedulers.background import BackgroundScheduler\nfrom mcp.server.fastmcp import FastMCP\n\nmcp = FastMCP(\"x\")\n\n\n@click.group()\ndef cli():\n    pass\n\n\n@cli.command(\"push\")\ndef push_all():\n    pass\n\n\n@mcp.tool()\ndef list_items():\n    pass\n\n\n@pytest.fixture(name=\"db\")\ndef make_db():\n    pass\n\n\n@scheduler.scheduled_job(\"cron\", hour=1)\ndef nightly():\n    pass\n",
    );
    assert_eq!(
        found,
        [
            "Command cli click - cli",
            "Command push_all click - push",
            "Tool list_items mcp - list_items",
            "Fixture make_db pytest - db",
            "Scheduled nightly apscheduler - cron",
        ]
    );
}

#[test]
fn a_bare_imported_fixture_is_named_by_its_function() {
    let found = python("from pytest import fixture\n\n\n@fixture\ndef client():\n    pass\n");
    assert_eq!(found, ["Fixture client pytest - client"]);
}

#[test]
fn jvm_annotations_name_the_framework_the_import_names() {
    let java = JavaExtractor.extract(
        "package p;\n\nimport org.springframework.scheduling.annotation.Scheduled;\nimport org.springframework.web.bind.annotation.*;\n\nclass C {\n    @GetMapping(\"/x\") void list() {\n    }\n\n    @Scheduled(fixedRate = 1000) void purge() {\n    }\n\n    @PostMapping void create() {\n    }\n}\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    assert_eq!(
        entries(&java),
        [
            "Route C.list spring GET /x",
            "Scheduled C.purge spring - -",
            "Route C.create spring POST -",
        ]
    );
    let kotlin = KotlinExtractor.extract(
        "package p\n\nimport org.springframework.web.bind.annotation.GetMapping\n\nclass C {\n    @GetMapping(\"/x\")\n    fun list() {\n    }\n}\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    assert_eq!(entries(&kotlin), ["Route C.list spring GET /x"]);
    let unimported = JavaExtractor.extract(
        "package p;\n\nclass C {\n    @GetMapping(\"/x\") void list() {\n    }\n}\n".as_bytes(),
        &RunBudget::default(),
    );
    assert!(entries(&unimported).is_empty());
}

#[test]
fn a_spring_handler_states_its_path_under_the_classs_mapping() {
    let java = JavaExtractor.extract(
        "package p;\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\n@RequestMapping(\"/api/orders\")\nclass C {\n    @GetMapping(\"/{id}\") void one() {\n    }\n\n    @PostMapping void add() {\n    }\n\n    @PutMapping(path = {\"/a\", \"b\"}, produces = \"x/y\") void both() {\n    }\n}\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    assert_eq!(
        entries(&java),
        [
            "Route C.one spring GET /api/orders/{id}",
            "Route C.add spring POST /api/orders",
            "Route C.both spring PUT /api/orders/a",
            "Route C.both spring PUT /api/orders/b",
        ]
    );
    let kotlin = KotlinExtractor.extract(
        "package p\n\nimport org.springframework.web.bind.annotation.PostMapping\nimport org.springframework.web.bind.annotation.RequestMapping\n\n@RequestMapping(value = [\"/tools\"])\nclass C {\n    @PostMapping(\"/{name}\")\n    fun call() {\n    }\n}\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    assert_eq!(entries(&kotlin), ["Route C.call spring POST /tools/{name}"]);
}

#[test]
fn a_request_mapping_on_a_method_is_a_route_and_on_a_class_is_none() {
    let java = JavaExtractor.extract(
        "package p;\n\nimport org.springframework.web.bind.annotation.*;\n\n@RequestMapping(\"/x\")\nclass C {\n    @RequestMapping(\"/y\") void m() {\n    }\n}\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    assert_eq!(entries(&java), ["Route C.m spring - /x/y"]);
}

fn java(source: &str) -> Vec<String> {
    entries(&JavaExtractor.extract(source.as_bytes(), &RunBudget::default()))
}

fn kotlin(source: &str) -> Vec<String> {
    entries(&KotlinExtractor.extract(source.as_bytes(), &RunBudget::default()))
}

#[test]
fn a_handler_with_annotation_arguments_is_sited_at_its_name() {
    let source = "package p;\n\nimport org.springframework.web.bind.annotation.GetMapping;\n\nclass C {\n    @GetMapping(\"/x\") void list() {\n    }\n}\n";
    let blob = JavaExtractor.extract(source.as_bytes(), &RunBudget::default());
    let sites: Vec<&str> = blob
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::EntryPoint(entry) => {
                Some(&source[entry.site.bytes.start as usize..entry.site.bytes.end as usize])
            }
            _ => None,
        })
        .collect();
    assert_eq!(sites, ["list"]);
}

#[test]
fn a_request_mapping_names_its_methods() {
    let found = java(
        "package p;\n\nimport org.springframework.web.bind.annotation.*;\n\nclass C {\n    @RequestMapping(value = \"/a\", method = RequestMethod.POST) void one() {\n    }\n\n    @RequestMapping(path = \"/b\", method = {RequestMethod.GET, RequestMethod.PUT}) void two() {\n    }\n\n    @RequestMapping(\"/c\") void any() {\n    }\n}\n",
    );
    assert_eq!(
        found,
        [
            "Route C.one spring POST /a",
            "Route C.two spring GET /b",
            "Route C.two spring PUT /b",
            "Route C.any spring - /c",
        ]
    );
    let found = kotlin(
        "package p\n\nimport org.springframework.web.bind.annotation.RequestMapping\nimport org.springframework.web.bind.annotation.RequestMethod\n\nclass C {\n    @RequestMapping(\"/a\", method = [RequestMethod.DELETE])\n    fun one() {\n    }\n}\n",
    );
    assert_eq!(found, ["Route C.one spring DELETE /a"]);
}

#[test]
fn jax_rs_paths_compose_like_spring_and_the_verb_is_the_annotation() {
    let found = java(
        "package p;\n\nimport jakarta.ws.rs.GET;\nimport jakarta.ws.rs.POST;\nimport jakarta.ws.rs.Path;\n\n@Path(\"/tools\")\nclass C {\n    @GET @Path(\"/{name}\") String one() {\n        return null;\n    }\n\n    @POST String add() {\n        return null;\n    }\n}\n",
    );
    assert_eq!(
        found,
        [
            "Route C.one jax-rs GET /tools/{name}",
            "Route C.add jax-rs POST /tools"
        ]
    );
    let found = kotlin(
        "package p\n\nimport jakarta.ws.rs.GET\nimport jakarta.ws.rs.Path\n\n@Path(\"/tools\")\nclass C {\n    @GET\n    @Path(\"/{name}\")\n    fun one(): String = \"\"\n}\n",
    );
    assert_eq!(found, ["Route C.one jax-rs GET /tools/{name}"]);
}

#[test]
fn a_constant_path_is_its_literal_where_the_file_or_its_imports_say_which() {
    let found = java(
        "package p;\n\nimport org.springframework.web.bind.annotation.*;\nimport demo.Routes;\n\nclass C {\n    static final String LOCAL = \"/local\";\n\n    @GetMapping(LOCAL) void a() {\n    }\n\n    @GetMapping(Routes.ITEMS) void b() {\n    }\n\n    @GetMapping(Other.X) void c() {\n    }\n\n    @GetMapping(\"${api.base}/d\") void d() {\n    }\n\n    @GetMapping(LOCAL + \"/e\") void e() {\n    }\n}\n",
    );
    assert_eq!(
        found,
        [
            "Route C.a spring GET /local",
            "Route C.b spring GET /{@demo.Routes#ITEMS}",
            "Route C.c spring GET /{@p.Other#X}",
            "Route C.d spring GET -",
            "Route C.e spring GET -",
        ]
    );
    let found = kotlin(
        "package p\n\nimport org.springframework.web.bind.annotation.GetMapping\nimport org.springframework.web.bind.annotation.RequestMapping\n\nobject Paths {\n    const val BASE = \"/base\"\n}\n\n@RequestMapping(Paths.BASE)\nclass C {\n    @GetMapping(\"/x\")\n    fun a() {\n    }\n}\n",
    );
    assert_eq!(found, ["Route C.a spring GET /base/x"]);
}

#[test]
fn nestjs_decorators_are_routes_and_jobs() {
    let ts = TypeScript.extract(
        "import { Controller, Get } from '@nestjs/common';\nimport { Cron } from '@nestjs/schedule';\n\n@Controller('items')\nexport class Items {\n  @Get()\n  list() {\n    return [];\n  }\n\n  @Cron('0 * * * *')\n  sweep() {}\n}\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    assert_eq!(
        entries(&ts),
        [
            "Route Items.list nestjs GET -",
            "Scheduled Items.sweep nestjs - -"
        ]
    );
}

#[test]
fn an_entry_reads_as_an_entry_point_never_as_nothing_calling_it() {
    let blob = PythonExtractor.extract(
        "from fastapi import APIRouter\n\nrouter = APIRouter()\n\n\n@router.get(\"/reviews\")\ndef reviews():\n    pass\n"
            .as_bytes(),
        &RunBudget::default(),
    );
    let said: Vec<String> = blob
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::EntryPoint(e) => Some(describe(e.kind, e.framework.as_ref()?)),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["entry point: route GET /reviews (fastapi)"]);
}
