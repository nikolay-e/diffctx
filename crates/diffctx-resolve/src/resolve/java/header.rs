//! What the header text of a Java definition says: its modifiers, keyword, supertypes,
//! type parameters, written type and parameter count (see `Sig`).

use super::Sig;
use super::jvm::{self, Flat};
use diffctx_facts::facts::DefKind;

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "synchronized",
    "native",
    "transient",
    "volatile",
    "strictfp",
    "default",
    "sealed",
    "non-sealed",
];

const TYPE_KEYWORDS: &[&str] = &["class", "interface", "enum", "record"];

impl Flat {
    /// Where the first non-blank top-level character at or after `from` stands.
    fn next_at(&self, from: usize) -> Option<usize> {
        (from.min(self.top.len())..self.top.len()).find(|i| !self.top[*i].is_whitespace())
    }

    /// The first non-blank top-level character at or after `from`.
    fn next_char(&self, from: usize) -> Option<char> {
        self.top[from.min(self.top.len())..]
            .iter()
            .copied()
            .find(|c| !c.is_whitespace())
    }
}

/// A written type with its qualifier: `a.b.List` stays `a.b.List`; a primitive
/// is its own name, which no type of the repository has.
fn written_path(written: &str) -> Option<String> {
    let text = written.trim().trim_end_matches("...");
    (!text.is_empty()
        && text.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        }))
    .then(|| text.to_string())
}

/// The outer name of a written type: `a.b.List` is `List`.
fn outer_name(written: &str) -> Option<String> {
    let path = written_path(written)?;
    path.rsplit('.').next().map(str::to_string)
}

/// The type parameters of the list opened by the `<` at `open`, each with the
/// written types it is bounded by.
fn type_params(flat: &Flat, open: Option<usize>) -> Vec<(String, Vec<String>)> {
    let Some(open) = open else {
        return Vec::new();
    };
    let end = (open + 1..flat.chars.len())
        .find(|i| flat.chars[*i] == '>' && flat.depth[*i] == 1)
        .unwrap_or(flat.chars.len());
    let mut params = Vec::new();
    let mut start = open + 1;
    for i in open + 1..=end {
        if i == end || (flat.chars[i] == ',' && flat.depth[i] == 1) {
            let part: String = flat.chars[start..i].iter().collect();
            let mut words = part.split_whitespace().filter(|w| !w.starts_with('@'));
            if let Some(name) = words.next() {
                let bounds = match words.next() {
                    Some("extends") => words
                        .collect::<Vec<_>>()
                        .join(" ")
                        .split('&')
                        .filter_map(bound_path)
                        .collect(),
                    _ => Vec::new(),
                };
                params.push((name.to_string(), bounds));
            }
            start = i + 1;
        }
    }
    params
}

/// A bound as written, without its type arguments: `a.b.Repo<T>` is `a.b.Repo`.
fn bound_path(bound: &str) -> Option<String> {
    let head = bound.split('<').next()?;
    written_path(head.split_whitespace().last()?)
}

/// The class literals of the `@ExtensionMethod` a class header carries, each as the
/// chain it is written with.
fn extension_classes(header: &str) -> Vec<String> {
    const ANNOTATION: &str = "ExtensionMethod";
    let Some(at) = header.find(ANNOTATION) else {
        return Vec::new();
    };
    let Some(args) = header[at + ANNOTATION.len()..]
        .trim_start()
        .strip_prefix('(')
        .and_then(|rest| rest.split(')').next())
    else {
        return Vec::new();
    };
    let mut pieces: Vec<&str> = args.split(".class").collect();
    pieces.pop();
    pieces
        .into_iter()
        .filter_map(|piece| {
            piece
                .rsplit(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '.')))
                .next()
                .filter(|chain| !chain.is_empty())
        })
        .map(str::to_string)
        .collect()
}

/// The outer names of every type `written` names, at any depth: a response body
/// carries `Page<User>`, `Map<String, List<User>>` and a project's own wrapper
/// alike, so each type argument is a payload.
fn payload_types(written: &str, out: &mut Vec<String>) {
    let mut written = written.trim();
    if let Some(bound) = written.strip_prefix('?') {
        let bound = bound.trim();
        written = bound
            .strip_prefix("extends")
            .or_else(|| bound.strip_prefix("super"))
            .unwrap_or(bound)
            .trim();
    }
    if written.starts_with('<') {
        let mut level = 0usize;
        let end = written.char_indices().find(|(_, c)| {
            level = match c {
                '<' => level + 1,
                '>' => level.saturating_sub(1),
                _ => level,
            };
            level == 0
        });
        written = end.map_or("", |(at, _)| written[at + 1..].trim());
    }
    let (head, arguments) = match written.split_once('<') {
        Some((head, rest)) => (head, rest.rsplit_once('>').map(|(inside, _)| inside)),
        None => (written, None),
    };
    let Some(name) = outer_name(head) else {
        return;
    };
    out.push(name);
    let Some(inside) = arguments else {
        return;
    };
    let mut level = 0usize;
    let mut start = 0;
    for (at, c) in inside.char_indices() {
        match c {
            '<' => level += 1,
            '>' => level = level.saturating_sub(1),
            ',' if level == 0 => {
                payload_types(&inside[start..at], out);
                start = at + 1;
            }
            _ => {}
        }
    }
    payload_types(&inside[start..], out);
}

pub(super) fn parse_sig(kind: DefKind, text: &str) -> Sig {
    let flat = Flat::new(text, jvm::JAVA);
    let tokens: Vec<(usize, usize, String)> = flat
        .tokens()
        .into_iter()
        .filter(|(_, _, t)| !t.starts_with('@'))
        .collect();
    let first = tokens
        .iter()
        .position(|(_, _, t)| !MODIFIERS.contains(&t.as_str()));
    let mut sig = Sig {
        words: tokens[..first.unwrap_or(tokens.len())]
            .iter()
            .map(|(_, _, t)| t.clone())
            .collect(),
        ..Sig::default()
    };
    let Some(first) = first else {
        return sig;
    };
    match kind {
        DefKind::Class | DefKind::Interface | DefKind::Enum | DefKind::Struct => {
            if TYPE_KEYWORDS.contains(&tokens[first].2.as_str()) {
                sig.keyword = Some(tokens[first].2.clone());
            }
            if let Some((_, name_end, _)) = tokens.get(first + 1) {
                sig.type_params = type_params(
                    &flat,
                    flat.next_at(*name_end).filter(|at| flat.chars[*at] == '<'),
                );
            }
            sig.extensions = extension_classes(text);
            let mut listing = false;
            for (_, _, token) in &tokens[(first + 1).min(tokens.len())..] {
                match token.as_str() {
                    "extends" | "implements" => listing = true,
                    "permits" | "throws" => listing = false,
                    _ if listing => sig.supers.extend(outer_name(token)),
                    _ => {}
                }
            }
        }
        DefKind::Method | DefKind::Constructor | DefKind::Function => {
            let named = tokens
                .iter()
                .position(|(_, end, _)| flat.next_char(*end) == Some('('));
            if let Some(named) = named {
                let before = tokens[named].0;
                sig.type_params = type_params(
                    &flat,
                    (0..before).find(|i| {
                        flat.chars[*i] == '<'
                            && flat.depth[*i] == 0
                            && (*i == 0 || flat.chars[*i - 1].is_whitespace())
                    }),
                );
                if kind != DefKind::Constructor && named > 0 {
                    let written: String = flat.chars[tokens[first].0..tokens[named].0]
                        .iter()
                        .collect();
                    payload_types(&written, &mut sig.returns);
                    let (_, end, written) = &tokens[named - 1];
                    if !MODIFIERS.contains(&written.as_str()) && flat.next_char(*end) != Some('[') {
                        sig.declared = written_path(written);
                    }
                }
                let open = (tokens[named].1..flat.chars.len()).find(|i| flat.chars[*i] == '(');
                if let Some(open) = open {
                    let close = (open + 1..flat.chars.len())
                        .find(|i| flat.chars[*i] == ')' && flat.depth[*i] == flat.depth[open] + 1);
                    if let Some(close) = close {
                        let inside: String = flat.chars[open + 1..close].iter().collect();
                        let level = flat.depth[open] + 1;
                        let commas = (open + 1..close)
                            .filter(|i| flat.chars[*i] == ',' && flat.depth[*i] == level)
                            .count();
                        let count = if inside.trim().is_empty() {
                            0
                        } else {
                            commas + 1
                        };
                        sig.params = Some((count, inside.contains("...")));
                    }
                }
            }
        }
        DefKind::Field | DefKind::Constant | DefKind::Property | DefKind::Variable => {
            if flat.next_char(tokens[first].1) != Some('[') {
                sig.declared = written_path(&tokens[first].2);
            }
        }
        _ => {}
    }
    sig
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_header_gives_its_modifiers_and_supertypes() {
        let sig = parse_sig(
            DefKind::Class,
            "@Service public final class Store<T> extends Base<T> implements Repo, a.b.Closeable",
        );
        assert_eq!(sig.words, ["public", "final"]);
        assert_eq!(sig.keyword.as_deref(), Some("class"));
        assert_eq!(sig.supers, ["Base", "Repo", "Closeable"]);
    }

    #[test]
    fn an_interface_extends_several_and_a_record_is_a_keyword() {
        let sig = parse_sig(DefKind::Interface, "public interface Port extends A, B<C>");
        assert_eq!(sig.supers, ["A", "B"]);
        let record = parse_sig(DefKind::Class, "record Pair(int a, int b) implements Cmp");
        assert_eq!(record.keyword.as_deref(), Some("record"));
        assert_eq!(record.supers, ["Cmp"]);
    }

    #[test]
    fn a_method_gives_its_return_type_and_its_parameters() {
        let sig = parse_sig(
            DefKind::Method,
            "@Override public static <T> List<T> find(Map<String, Long> by, int... rest) throws IOException",
        );
        assert_eq!(sig.words, ["public", "static"]);
        assert_eq!(sig.declared.as_deref(), Some("List"));
        assert_eq!(sig.params, Some((2, true)));
        let none = parse_sig(DefKind::Method, "void run()");
        assert_eq!(
            (none.declared.as_deref(), none.params),
            (Some("void"), Some((0, false)))
        );
        let array = parse_sig(DefKind::Method, "String[] names()");
        assert_eq!(array.declared, None);
        let qualified = parse_sig(DefKind::Method, "public Lib.Inner<T> make()");
        assert_eq!(qualified.declared.as_deref(), Some("Lib.Inner"));
    }

    #[test]
    fn a_header_gives_its_type_parameters_with_their_bounds() {
        let method = parse_sig(
            DefKind::Method,
            "public static <U, T extends Comparable<T> & a.b.Repo> Map<String, T> find(T t)",
        );
        assert_eq!(
            method.type_params,
            [
                ("U".to_string(), vec![]),
                (
                    "T".to_string(),
                    vec!["Comparable".to_string(), "a.b.Repo".to_string()]
                ),
            ]
        );
        let class = parse_sig(
            DefKind::Class,
            "class Store<T extends Repo> extends Base<Other>",
        );
        assert_eq!(
            class.type_params,
            [("T".to_string(), vec!["Repo".to_string()])]
        );
        let plain = parse_sig(DefKind::Method, "List<String> names()");
        assert!(plain.type_params.is_empty());
    }

    #[test]
    fn a_constructor_has_no_return_type_and_a_field_its_written_type() {
        let ctor = parse_sig(DefKind::Constructor, "public Store(Repo repo, int size)");
        assert_eq!((ctor.declared, ctor.params), (None, Some((2, false))));
        let field = parse_sig(
            DefKind::Field,
            "private final Map<String, Repo> repos = new HashMap<>();",
        );
        assert_eq!(field.declared.as_deref(), Some("Map"));
        assert!(field.has("final"));
    }
}
