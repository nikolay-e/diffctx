mod ansible;
mod bazel;
mod c_family;
mod cargo_edges;
mod clojure;
mod css;
mod dart;
mod dbt;
mod dotnet;
mod elixir;
mod erlang;
mod go;
mod graphql;
mod haskell;
mod javascript;
mod julia;
mod jvm;
mod latex;
pub(in crate::edges) mod legacy;
mod lua;
mod nim;
mod nix;
mod ocaml;
mod openapi;
mod perl;
mod php;
mod prisma;
mod protobuf;
mod python;
mod r_lang;
mod ruby;
mod rust_lang;
mod shell;
mod sql;
mod swift;
mod tags;
mod terraform;
mod zig;

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
