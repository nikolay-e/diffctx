//! The source of a file as an isolation scanner reads it: comments gone, code
//! and string literals kept. A comment that names a path outside the module
//! (`// leaves a module by super::super::m`) reaches nothing.

pub(crate) fn without_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match (c, next) {
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                let mut depth = 0;
                while i < chars.len() {
                    match (chars[i], chars.get(i + 1).copied()) {
                        ('/', Some('*')) => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => i += 1,
                    }
                }
                out.push(' ');
            }
            ('"', _) => i = copy_string(&chars, i, 0, &mut out),
            ('r', _) if raw_hashes(&chars, i).is_some() => {
                let hashes = raw_hashes(&chars, i).unwrap_or(0);
                out.push('r');
                out.extend(std::iter::repeat_n('#', hashes));
                i = copy_string(&chars, i + 1 + hashes, hashes + 1, &mut out);
            }
            ('\'', _) => i = copy_char(&chars, i, &mut out),
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The `#`s of a raw string opening at `at` (`r"`, `r#"`), when one does.
fn raw_hashes(chars: &[char], at: usize) -> Option<usize> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if at > 0 && word(chars[at - 1]) && chars[at - 1] != 'b' {
        return None;
    }
    let hashes = chars[at + 1..].iter().take_while(|c| **c == '#').count();
    (chars.get(at + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// Copies the string whose opening quote is at `at`; a raw string (`closing`
/// is its hashes plus one) ends at a quote followed by its hashes, any other at
/// a quote no backslash escapes. Returns where reading goes on.
fn copy_string(chars: &[char], at: usize, closing: usize, out: &mut String) -> usize {
    let raw = closing > 0;
    out.push('"');
    let mut i = at + 1;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if c == '\\' && !raw {
            if let Some(escaped) = chars.get(i) {
                out.push(*escaped);
                i += 1;
            }
        } else if c == '"'
            && chars[i..].iter().take_while(|h| **h == '#').count() >= closing.saturating_sub(1)
        {
            let hashes = closing.saturating_sub(1);
            out.extend(std::iter::repeat_n('#', hashes));
            return i + hashes;
        }
    }
    i
}

/// Copies a character literal; a lifetime (`'a`) is copied as the quote alone.
fn copy_char(chars: &[char], at: usize, out: &mut String) -> usize {
    let end = match (chars.get(at + 1), chars.get(at + 2)) {
        (Some('\\'), _) => (at + 3..chars.len().min(at + 12)).find(|i| chars[*i] == '\''),
        (Some(_), Some('\'')) => Some(at + 2),
        _ => None,
    };
    match end {
        Some(end) => {
            out.extend(&chars[at..=end]);
            end + 1
        }
        None => {
            out.push('\'');
            at + 1
        }
    }
}
