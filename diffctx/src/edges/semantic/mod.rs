pub mod ansible;
pub mod bazel;
pub mod c_family;
pub mod cargo_edges;
pub mod clojure;
pub mod css;
pub mod dart;
pub mod dbt;
pub mod dotnet;
pub mod elixir;
pub mod erlang;
pub mod go;
pub mod graphql;
pub mod haskell;
pub mod javascript;
pub mod julia;
pub mod jvm;
pub mod latex;
pub mod lua;
pub mod nim;
pub mod nix;
pub mod ocaml;
pub mod openapi;
pub mod perl;
pub mod php;
pub mod prisma;
pub mod protobuf;
pub mod python;
pub mod r_lang;
pub mod ruby;
pub mod rust_lang;
pub mod shell;
pub mod sql;
pub mod swift;
pub mod tags;
pub mod zig;

use super::base::EdgeBuilder;

pub fn get_semantic_builders() -> Vec<Box<dyn EdgeBuilder>> {
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
        Box::new(tags::TagsEdgeBuilder),
    ]
}
