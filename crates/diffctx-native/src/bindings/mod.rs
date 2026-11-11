//! What a use of a changed definition is, as the rows of an answer state it
//! (`Finding`, `Evidence`), the languages the import readers know (`Lang`), and the two
//! regex readers of imports the selection graph and the overlay still take: Python's
//! (`python_bindings`) and the definitions a JavaScript file or page declares
//! (`js_facts`). The binding model that resolved a use through them is gone: the cells
//! (`decide.rs`) resolve, and a language no cell reads is answered by the text tier.
//! Swift, SQL and the scripts of a page are cells now, not the text tier.

use std::path::Path;

mod js;

pub(crate) use js::js_facts;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    Js,
}

pub fn lang_of(path: &str) -> Option<Lang> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    match ext.as_str() {
        "py" | "pyi" => Some(Lang::Python),
        // A page's inline scripts are JavaScript (#420).
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "html" | "htm" => {
            Some(Lang::Js)
        }
        _ => None,
    }
}

/// The owner of a property set on the global object (`self.x = …`): any
/// script the realm loads reaches it by name, bare or through the global
/// object, with no import to bind it (#426).
pub(crate) const GLOBAL_OWNER: &str = "<global>";

const GLOBAL_OBJECTS: &[&str] = &["self", "globalThis", "window", "global"];
