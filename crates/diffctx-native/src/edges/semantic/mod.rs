pub(crate) mod ansible;
pub(crate) mod bazel;
pub(crate) mod c_family;
pub(crate) mod cargo_edges;
pub(crate) mod clojure;
pub(crate) mod css;
pub(crate) mod dart;
pub(crate) mod dbt;
pub(crate) mod dotnet;
pub(crate) mod elixir;
pub(crate) mod erlang;
pub(crate) mod go;
pub(crate) mod graphql;
pub(crate) mod haskell;
pub(crate) mod javascript;
pub(crate) mod julia;
pub(crate) mod jvm;
pub(crate) mod latex;
pub(in crate::edges) mod legacy;
pub(crate) mod lua;
pub(crate) mod nim;
pub(crate) mod nix;
pub(crate) mod ocaml;
pub(crate) mod openapi;
pub(crate) mod perl;
pub(crate) mod php;
pub(crate) mod prisma;
pub(crate) mod protobuf;
pub(crate) mod python;
pub(crate) mod r_lang;
pub(crate) mod ruby;
pub(crate) mod rust_lang;
pub(crate) mod shell;
pub(crate) mod sql;
pub(crate) mod swift;
pub(crate) mod tags;
pub(crate) mod terraform;
pub(crate) mod zig;

use super::base::EdgeBuilder;

/// The cells' builders for the groups drawn from them (none by default), then
/// the text builders, each narrowed to what the cells do not cover
/// (`CELL_EDGES`).
pub(crate) fn get_semantic_builders() -> Vec<Box<dyn EdgeBuilder>> {
    let mut builders = super::cells::builders();
    builders.extend(text_builders().into_iter().map(super::cells::beside));
    builders
}

pub(super) fn text_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![
        Box::new(python::PythonEdgeBuilder),
        Box::new(javascript::JavaScriptEdgeBuilder),
        Box::new(go::GoEdgeBuilder),
        Box::new(rust_lang::RustEdgeBuilder),
        Box::new(jvm::JVMEdgeBuilder),
        Box::new(c_family::CFamilyEdgeBuilder),
        Box::new(dart::DartEdgeBuilder),
        Box::new(dotnet::DotNetEdgeBuilder),
        Box::new(haskell::HaskellEdgeBuilder),
        Box::new(shell::ShellEdgeBuilder),
        Box::new(ruby::RubyEdgeBuilder),
        Box::new(php::PhpEdgeBuilder),
        Box::new(swift::SwiftEdgeBuilder),
        Box::new(elixir::ElixirEdgeBuilder),
        Box::new(sql::SqlEdgeBuilder),
        Box::new(lua::LuaEdgeBuilder),
        Box::new(css::CssEdgeBuilder),
        Box::new(protobuf::ProtobufEdgeBuilder),
        Box::new(graphql::GraphqlEdgeBuilder),
        Box::new(latex::LatexEdgeBuilder),
        Box::new(prisma::PrismaEdgeBuilder),
        Box::new(openapi::OpenapiEdgeBuilder),
        Box::new(dbt::DbtEdgeBuilder),
        Box::new(r_lang::RLangEdgeBuilder),
        Box::new(perl::PerlEdgeBuilder),
        Box::new(julia::JuliaEdgeBuilder),
        Box::new(zig::ZigEdgeBuilder),
        Box::new(nix::NixEdgeBuilder),
        Box::new(ocaml::OCamlEdgeBuilder),
        Box::new(nim::NimEdgeBuilder),
        Box::new(erlang::ErlangEdgeBuilder),
        Box::new(clojure::ClojureEdgeBuilder),
        Box::new(cargo_edges::CargoEdgeBuilder),
        Box::new(bazel::BazelEdgeBuilder),
        Box::new(ansible::AnsibleEdgeBuilder),
        Box::new(terraform::TerraformEdgeBuilder),
        Box::new(tags::TagsEdgeBuilder),
    ]
}
