//! TypeScript and JavaScript: `package.json` (dependencies, workspaces,
//! `exports`), `pnpm-workspace.yaml`, and the `tsconfig` family (`extends`,
//! `baseUrl`, `paths`, `references`).

use serde_json::Value;

use super::{
    Alias, Draft, Ecosystem, Export, Files, Late, Result, RootOrigin, dir_of, expand_members,
    has_extension, join, name_of,
};

const MAX_EXTENDS_DEPTH: u8 = 8;

pub(super) fn discover(files: &Files, draft: &mut Draft) -> Result<()> {
    for manifest in files.named("package.json") {
        let root = dir_of(manifest);
        let Some(text) = files.read(manifest)? else {
            continue;
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => read_package(files, root, &value, draft),
            Err(error) => draft.unknown(
                root,
                Ecosystem::Node,
                manifest,
                format!("not valid JSON: {error}"),
            ),
        }
    }
    for manifest in files.named("pnpm-workspace.yaml") {
        let root = dir_of(manifest);
        let Some(text) = files.read(manifest)? else {
            continue;
        };
        let members = expand_members(files, root, &pnpm_packages(&text));
        draft.project(root, Ecosystem::Node);
        for member in members
            .into_iter()
            .filter(|m| files.has(&format!("{m}/package.json")))
        {
            draft.project(root, Ecosystem::Node).link_member(member);
        }
    }
    let configs: Vec<&str> = files
        .all()
        .filter(|p| {
            let name = name_of(p);
            name == "jsconfig.json" || (name.starts_with("tsconfig") && has_extension(name, "json"))
        })
        .collect();
    for config in configs {
        read_tsconfig(files, config, draft)?;
    }
    let bundlers: Vec<&str> = files
        .all()
        .filter(|p| {
            let name = name_of(p);
            (name.starts_with("vite.config.") || name.starts_with("vitest.config."))
                && [".ts", ".js", ".mts", ".mjs", ".cts", ".cjs"]
                    .iter()
                    .any(|e| name.ends_with(e))
        })
        .collect();
    for config in bundlers {
        let Some(text) = files.read(config)? else {
            continue;
        };
        let dir = dir_of(config);
        let (found, readable) = vite_aliases(&text);
        let mut aliases = Vec::new();
        for (find, replacement) in found {
            let Some(target) = join(dir, &replacement) else {
                continue;
            };
            let find = find.trim_end_matches('/');
            aliases.push(Alias {
                pattern: find.to_string(),
                targets: vec![target.clone()],
            });
            aliases.push(Alias {
                pattern: format!("{find}/*"),
                targets: vec![format!("{target}/*")],
            });
        }
        draft.late(Late::Vite {
            dir: dir.to_string(),
            aliases,
            incomplete: !readable,
        });
    }
    Ok(())
}

/// What a bundler config states, read as text and never run: the `find` and
/// `replacement` of each entry of `resolve.alias`, as `{ '@pkg': path.resolve(
/// __dirname, '../pkg') }` or `[{ find: '@pkg', replacement: ... }]`. A
/// replacement is the string literals it is built from, joined by `/`. The flag
/// is false when an entry could not be read (a regex `find`, a computed or
/// absolute replacement).
fn vite_aliases(text: &str) -> (Vec<(String, String)>, bool) {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut readable = true;
    let mut at = 0;
    while let Some(hit) = text[at..].find("alias") {
        let start = at + hit;
        at = start + "alias".len();
        let before = bytes.get(start.wrapping_sub(1)).copied();
        if before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$') {
            continue;
        }
        let mut cursor = skip_blanks(bytes, at);
        if matches!(bytes.get(cursor), Some(b'"' | b'\'' | b'`')) {
            cursor = skip_blanks(bytes, cursor + 1);
        }
        if bytes.get(cursor) != Some(&b':') {
            continue;
        }
        cursor = skip_blanks(bytes, cursor + 1);
        match bytes.get(cursor) {
            Some(b'{') => readable &= alias_object(bytes, cursor + 1, &mut found).is_some(),
            Some(b'[') => readable &= alias_array(bytes, cursor + 1, &mut found).is_some(),
            _ => readable = false,
        }
    }
    (found, readable)
}

fn skip_blanks(bytes: &[u8], mut at: usize) -> usize {
    while let Some(&b) = bytes.get(at) {
        if b.is_ascii_whitespace() {
            at += 1;
        } else if b == b'/' && bytes.get(at + 1) == Some(&b'/') {
            while bytes.get(at).is_some_and(|c| *c != b'\n') {
                at += 1;
            }
        } else if b == b'/' && bytes.get(at + 1) == Some(&b'*') {
            at += 2;
            while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
                at += 1;
            }
            at += 2;
        } else {
            break;
        }
    }
    at.min(bytes.len())
}

/// A string literal from its opening quote: its text and the index after it.
fn string_literal(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    let quote = *bytes.get(at)?;
    if !matches!(quote, b'"' | b'\'' | b'`') {
        return None;
    }
    let mut end = at + 1;
    let mut out = Vec::new();
    while let Some(&b) = bytes.get(end) {
        if b == b'\\' {
            out.extend(bytes.get(end + 1));
            end += 2;
        } else if b == quote {
            return Some((String::from_utf8_lossy(&out).into_owned(), end + 1));
        } else {
            out.push(b);
            end += 1;
        }
    }
    None
}

/// What an alias key or value is: the string literals of the expression up to
/// the `,` or closing bracket that ends it, whether it opens as a regex, and
/// the index of that end.
struct Written {
    strings: Vec<String>,
    regex: bool,
    end: usize,
}

fn written(bytes: &[u8], start: usize) -> Option<Written> {
    let mut at = skip_blanks(bytes, start);
    let regex = bytes.get(at) == Some(&b'/');
    let mut strings = Vec::new();
    let mut depth = 0usize;
    while let Some(&b) = bytes.get(at) {
        match b {
            b'"' | b'\'' | b'`' => {
                let (text, next) = string_literal(bytes, at)?;
                strings.push(text);
                at = next;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => break,
            _ => {}
        }
        at += 1;
    }
    Some(Written {
        strings,
        regex,
        end: at,
    })
}

/// The replacement a written expression stands for, relative to its config.
fn replacement(value: &Written) -> Option<String> {
    let joined = value.strings.join("/");
    (!value.strings.is_empty() && !joined.starts_with('/')).then_some(joined)
}

/// The entries of `{ '@pkg': ..., '@other': ... }` after its `{`.
fn alias_object(bytes: &[u8], mut at: usize, out: &mut Vec<(String, String)>) -> Option<()> {
    let mut ok = true;
    loop {
        at = skip_blanks(bytes, at);
        match bytes.get(at)? {
            b'}' => return ok.then_some(()),
            b',' => at += 1,
            _ => {
                let (key, next) = match string_literal(bytes, at) {
                    Some(found) => found,
                    None => {
                        let end = bytes[at..].iter().position(|b| {
                            !(b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
                        })?;
                        if end == 0 {
                            return None;
                        }
                        (
                            String::from_utf8_lossy(&bytes[at..at + end]).into_owned(),
                            at + end,
                        )
                    }
                };
                at = skip_blanks(bytes, next);
                if bytes.get(at) != Some(&b':') {
                    return None;
                }
                let value = written(bytes, at + 1)?;
                match replacement(&value) {
                    Some(to) if !value.regex => out.push((key, to)),
                    _ => ok = false,
                }
                at = value.end;
            }
        }
    }
}

/// The entries of `[{ find: '@pkg', replacement: ... }]` after its `[`.
fn alias_array(bytes: &[u8], mut at: usize, out: &mut Vec<(String, String)>) -> Option<()> {
    let mut ok = true;
    loop {
        at = skip_blanks(bytes, at);
        match bytes.get(at)? {
            b']' => return ok.then_some(()),
            b',' => at += 1,
            b'{' => {
                at += 1;
                let (mut find, mut to): (Option<Written>, Option<Written>) = (None, None);
                loop {
                    at = skip_blanks(bytes, at);
                    match bytes.get(at)? {
                        b'}' => {
                            at += 1;
                            break;
                        }
                        b',' => at += 1,
                        _ => {
                            let end = bytes[at..]
                                .iter()
                                .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))?;
                            let key = String::from_utf8_lossy(&bytes[at..at + end]).into_owned();
                            at = skip_blanks(bytes, at + end);
                            if bytes.get(at) != Some(&b':') {
                                return None;
                            }
                            let value = written(bytes, at + 1)?;
                            at = value.end;
                            match key.as_str() {
                                "find" => find = Some(value),
                                "replacement" => to = Some(value),
                                _ => {}
                            }
                        }
                    }
                }
                match (find, to) {
                    (Some(f), Some(t)) if !f.regex && f.strings.len() == 1 => {
                        match replacement(&t) {
                            Some(to) if !t.regex => out.push((f.strings[0].clone(), to)),
                            _ => ok = false,
                        }
                    }
                    _ => ok = false,
                }
            }
            _ => return None,
        }
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn read_package(files: &Files, root: &str, package: &Value, draft: &mut Draft) {
    let globs = match package.get("workspaces") {
        Some(Value::Object(object)) => strings(object.get("packages")),
        other => strings(other),
    };
    let members: Vec<String> = expand_members(files, root, &globs)
        .into_iter()
        .filter(|m| files.has(&format!("{m}/package.json")))
        .collect();

    let mut exports = Vec::new();
    for field in ["main", "module", "types", "typings", "source"] {
        if let Some(target) = package.get(field).and_then(Value::as_str) {
            exports.extend(join(root, target).map(|t| Export {
                subpath: ".".to_string(),
                target: t,
            }));
        }
    }
    if let Some(value) = package.get("exports") {
        collect_exports(root, value, ".", &mut exports);
    }
    if let Some(Value::Object(imports)) = package.get("imports") {
        for (subpath, value) in imports {
            collect_exports(root, value, subpath, &mut exports);
        }
    }

    let project = draft.project(root, Ecosystem::Node);
    project.root(root.to_string(), RootOrigin::Manifest);
    if let Some(name) = package.get("name").and_then(Value::as_str) {
        project.name(Ecosystem::Node, name.to_string());
    }
    for field in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(Value::Object(dependencies)) = package.get(field) else {
            continue;
        };
        for (name, spec) in dependencies {
            project.link_name(Ecosystem::Node, name.clone());
            let local = spec.as_str().and_then(|s| {
                ["file:", "link:", "portal:"]
                    .iter()
                    .find_map(|prefix| s.strip_prefix(prefix))
            });
            if let Some(dir) = local.and_then(|rel| join(root, rel)) {
                project.link_dir(dir);
            }
        }
    }
    for member in members {
        project.link_member(member);
    }
    for export in exports {
        if !project.exports.contains(&export) {
            project.exports.push(export);
        }
    }
}

/// Walks an `exports` value: a string or array is the target of `subpath`; an
/// object is a map of subpaths when its keys start with `.`, else a map of
/// conditions that all lead to targets of `subpath`.
fn collect_exports(root: &str, value: &Value, subpath: &str, out: &mut Vec<Export>) {
    match value {
        Value::String(target) => {
            if let Some(target) = join(root, target) {
                out.push(Export {
                    subpath: subpath.to_string(),
                    target,
                });
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_exports(root, item, subpath, out);
            }
        }
        Value::Object(map) => {
            let subpaths = map.keys().any(|k| k.starts_with('.'));
            for (key, item) in map {
                let next = if subpaths { key.as_str() } else { subpath };
                collect_exports(root, item, next, out);
            }
        }
        _ => {}
    }
}

/// The `packages:` list of a `pnpm-workspace.yaml`, read as the plain
/// block sequence it is in practice.
fn pnpm_packages(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with(char::is_whitespace) && !trimmed.starts_with('-') {
            inside = trimmed.trim_end_matches(':') == "packages" && trimmed.ends_with(':');
            continue;
        }
        if inside {
            if let Some(item) = trimmed.strip_prefix('-') {
                let item = item.split('#').next().unwrap_or("").trim();
                let item = item.trim_matches(|c| c == '\'' || c == '"');
                if !item.is_empty() {
                    out.push(item.to_string());
                }
            }
        }
    }
    out
}

/// JSON with comments and trailing commas, which is what a `tsconfig` is.
fn jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut clean = String::with_capacity(text.len());
    let (mut i, mut in_string) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            clean.push(c);
            if c == '\\' && i + 1 < chars.len() {
                clean.push(chars[i + 1]);
                i += 2;
                continue;
            }
            in_string = c != '"';
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_string = true;
                clean.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                clean.push(c);
                i += 1;
            }
        }
    }
    let chars: Vec<char> = clean.chars().collect();
    let mut out = String::with_capacity(clean.len());
    let (mut i, mut in_string) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            in_string = c != '"';
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

#[derive(Clone, Default)]
struct TsOptions {
    base_url: Option<String>,
    paths: Vec<(String, Vec<String>)>,
    /// The directory of the config that set `paths`, which resolves them
    /// when no `baseUrl` does.
    paths_dir: String,
    /// A config it extends could not be read: a package's, or one that is gone.
    incomplete: bool,
}

fn config_file(files: &Files, from: &str, reference: &str) -> Option<String> {
    let joined = join(dir_of(from), reference)?;
    [
        joined.clone(),
        format!("{joined}.json"),
        format!("{joined}/tsconfig.json"),
    ]
    .into_iter()
    .find(|c| files.has(c))
}

fn ts_options(
    files: &Files,
    config: &str,
    depth: u8,
    draft: &mut Draft,
) -> Result<(TsOptions, Option<Value>)> {
    let Some(text) = files.read(config)? else {
        return Ok((TsOptions::default(), None));
    };
    let value = match serde_json::from_str::<Value>(&jsonc(&text)) {
        Ok(value) => value,
        Err(error) => {
            draft.lose(config, format!("not valid JSON: {error}"));
            return Ok((TsOptions::default(), None));
        }
    };
    let dir = dir_of(config);
    let mut options = TsOptions::default();
    if depth < MAX_EXTENDS_DEPTH {
        for parent in strings(value.get("extends")) {
            if !parent.starts_with('.') {
                options.incomplete = true;
                continue;
            }
            if let Some(parent) = config_file(files, config, &parent) {
                let (inherited, _) = ts_options(files, &parent, depth + 1, draft)?;
                options.incomplete |= inherited.incomplete;
                if inherited.base_url.is_some() {
                    options.base_url = inherited.base_url;
                }
                if !inherited.paths.is_empty() {
                    options.paths = inherited.paths;
                    options.paths_dir = inherited.paths_dir;
                }
            } else {
                options.incomplete = true;
            }
        }
    }
    if let Some(compiler) = value.get("compilerOptions") {
        if let Some(base) = compiler.get("baseUrl").and_then(Value::as_str) {
            options.base_url = join(dir, base);
        }
        if let Some(Value::Object(paths)) = compiler.get("paths") {
            options.paths = paths
                .iter()
                .map(|(pattern, targets)| (pattern.clone(), strings(Some(targets))))
                .collect();
            options.paths_dir = dir.to_string();
        }
    }
    Ok((options, Some(value)))
}

/// The directory a `paths` target names: the part before its wildcard, or
/// the directory of a file target.
fn target_dir(target: &str) -> String {
    match target.find('*') {
        Some(star) => target[..star].trim_end_matches('/').to_string(),
        None => dir_of(target).to_string(),
    }
}

fn read_tsconfig(files: &Files, config: &str, draft: &mut Draft) -> Result<()> {
    let (options, value) = ts_options(files, config, 0, draft)?;
    let Some(value) = value else {
        return Ok(());
    };
    let dir = dir_of(config);
    let base = options
        .base_url
        .clone()
        .unwrap_or_else(|| options.paths_dir.clone());
    let aliases: Vec<Alias> = options
        .paths
        .iter()
        .map(|(pattern, targets)| Alias {
            pattern: pattern.clone(),
            targets: targets.iter().filter_map(|t| join(&base, t)).collect(),
        })
        .collect();
    let mut roots: Vec<String> = options.base_url.iter().cloned().collect();
    roots.extend(
        aliases
            .iter()
            .flat_map(|a| a.targets.iter().map(|t| target_dir(t))),
    );
    let references: Vec<String> = match value.get("references") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("path").and_then(Value::as_str))
            .filter_map(|path| join(dir, path))
            .map(|path| {
                if has_extension(&path, "json") {
                    dir_of(&path).to_string()
                } else {
                    path
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    draft.late(Late::TsConfig {
        dir: dir.to_string(),
        aliases,
        base_url: options.base_url.clone(),
        roots,
        references,
        incomplete: options.incomplete,
    });
    Ok(())
}
