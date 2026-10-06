//! File masks as Far understands them (far/filemasks.cpp,
//! far/processname.cpp): `*.rs;*.toml`, `,` or `;` between masks, quotes
//! for masks with separators, `|` before exclusions, `/regex/i`, mask
//! groups `<arc>`, `<temp>`, `<exec>` and `%PATHEXT%`.

/// A parsed mask list.
#[derive(Debug)]
pub struct FileMasks {
    include: Vec<Masks>,
    exclude: Vec<Masks>,
}

#[derive(Debug)]
enum Masks {
    Wildcards(Vec<String>),
    Regex(regex::Regex),
}

impl Masks {
    fn matches(&self, name: &str) -> bool {
        match self {
            Masks::Wildcards(list) => list.iter().any(|m| cmp_name(m, name)),
            Masks::Regex(re) => re.is_match(name),
        }
    }

    /// Far's masks::assign: a regex, or a list of wildcards.
    fn parse(text: &str) -> Option<Self> {
        if text.starts_with('/') {
            return parse_regex(text).map(Masks::Regex);
        }
        let text = expand_pathext(text);
        let list: Vec<String> = split_masks(&text)
            .into_iter()
            .filter(|m| !m.is_empty())
            .map(|m| {
                if m == "*.*" {
                    "*".to_string()
                } else {
                    // "**" is the same as "*".
                    let mut out = String::new();
                    for c in m.chars() {
                        if !(c == '*' && out.ends_with('*')) {
                            out.push(c);
                        }
                    }
                    out
                }
            })
            .collect();
        (!list.is_empty()).then_some(Masks::Wildcards(list))
    }
}

/// Far's default mask groups (config.cpp ApplyDefaultMaskGroups).
const GROUPS: [(&str, &str); 3] = [
    (
        "arc",
        "*.zip,*.rar,*.[7bgxl]z,*.[bg]zip,*.tar,*.t[agbxl]z,*.z,*.ar[cj],*.r[0-9][0-9],*.a[0-9][0-9],*.bz2,*.cab,*.jar,*.lha,*.lzh,*.ha,*.ac[bei],*.pa[ck],*.rk,*.cpio,*.rpm,*.zoo,*.hqx,*.sit,*.ice,*.uc2,*.ain,*.imp,*.777,*.ufa,*.boa,*.bs[2a],*.sea,*.[ah]pk,*.ddi,*.x2,*.rkv,*.[lw]sz,*.h[ay]p,*.lim,*.sqz,*.chz,*.aa[br],*.zst",
    ),
    ("temp", "*.bak,*.tmp"),
    ("exec", "*.exe,*.cmd,*.bat,*.com,%PATHEXT%"),
];

impl FileMasks {
    /// Parses a mask list; `None` when it has no masks or a bad regex
    /// (Far then says MIncorrectMask).
    pub fn parse(text: &str) -> Option<Self> {
        let text = expand_groups(text);
        let mut rest = text.as_str();
        let mut include = Vec::new();
        let mut exclude = Vec::new();
        let (mut simple_in, mut simple_ex) = (String::new(), String::new());
        let mut excluding = false;
        while !rest.is_empty() {
            rest = rest.trim_start_matches([' ', ',', ';']);
            let re = take_regex(&mut rest);
            if !re.is_empty() {
                let m = Masks::parse(re)?;
                let dest = if excluding {
                    &mut exclude
                } else {
                    &mut include
                };
                dest.push(m);
            }
            rest = rest.trim_start_matches([' ', ',', ';']);
            let end = rest.find(['/', '|']).unwrap_or(rest.len());
            let (masks, tail) = rest.split_at(end);
            rest = tail;
            if !masks.is_empty() {
                let dest = if excluding {
                    &mut simple_ex
                } else {
                    &mut simple_in
                };
                dest.push_str(masks);
            }
            if let Some(tail) = rest.strip_prefix('|') {
                if excluding {
                    break;
                }
                excluding = true;
                rest = tail;
            }
        }
        if !simple_in.is_empty() {
            include.push(Masks::parse(&simple_in)?);
        }
        if !simple_ex.is_empty() {
            exclude.push(Masks::parse(&simple_ex)?);
        }
        if include.is_empty() && !exclude.is_empty() {
            include.push(Masks::Wildcards(vec!["*".into()]));
        }
        (!include.is_empty()).then_some(Self { include, exclude })
    }

    pub fn matches(&self, name: &str) -> bool {
        !self.exclude.iter().any(|m| m.matches(name))
            && self.include.iter().any(|m| m.matches(name))
    }
}

/// `<group>` → its masks (a group used twice expands to nothing).
fn expand_groups(text: &str) -> String {
    let mut out = text.to_string();
    let mut used = Vec::new();
    while let Some(l) = out.find('<') {
        let Some(r) = out[l..].find('>').map(|r| l + r) else {
            break;
        };
        let name = out[l + 1..r].to_lowercase();
        let value = if used.contains(&name) {
            ""
        } else {
            GROUPS
                .iter()
                .find(|(g, _)| *g == name)
                .map_or("", |(_, v)| *v)
        };
        used.push(name);
        out.replace_range(l..=r, value);
    }
    out
}

/// `%PATHEXT%` → `*.COM,*.EXE,…` from the environment.
fn expand_pathext(text: &str) -> String {
    const NAME: &str = "%pathext%";
    let Some(i) = text.to_lowercase().find(NAME) else {
        return text.to_string();
    };
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_default()
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| format!("*{e}"))
        .collect();
    format!(
        "{}{}{}",
        &text[..i],
        exts.join(","),
        &text[i + NAME.len()..]
    )
}

/// Takes `/…/options` from the start of `rest` (Far's extract_re).
fn take_regex<'a>(rest: &mut &'a str) -> &'a str {
    if !rest.starts_with('/') {
        return "";
    }
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() && (bytes[i] != b'/' || bytes[i - 1] == b'\\') {
        i += 1;
    }
    if i < bytes.len() {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let (re, tail) = rest.split_at(i);
    *rest = tail;
    re
}

/// `/pattern/options` (Perl style: i, m, s, x) → a regex.
fn parse_regex(text: &str) -> Option<regex::Regex> {
    let body = text.strip_prefix('/')?;
    let end = body.rfind('/')?;
    let (pattern, options) = (&body[..end], &body[end + 1..]);
    let mut b = regex::RegexBuilder::new(pattern);
    for o in options.chars() {
        match o {
            'i' => b.case_insensitive(true),
            'm' => b.multi_line(true),
            's' => b.dot_matches_new_line(true),
            'x' => b.ignore_whitespace(true),
            _ => &mut b,
        };
    }
    b.build().ok()
}

/// Splits on `,` and `;` outside quotes and `[…]`; quotes are removed,
/// spaces trimmed (Far's enum_tokens_with_quotes_t<with_brackets, with_trim>).
fn split_masks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut bracket) = (false, false);
    for c in text.chars() {
        match c {
            '"' if !bracket => quoted = !quoted,
            '[' if !quoted && !bracket => {
                bracket = true;
                cur.push(c);
            }
            ']' if bracket => {
                bracket = false;
                cur.push(c);
            }
            ',' | ';' if !quoted && !bracket => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|m| m.trim().to_string()).collect()
}

/// Far's CmpName in legacy mode: `*`, `?`, `[a-z]` sets, case-insensitive;
/// "x.*" also matches "x", and "x." matches a name without a dot.
pub fn cmp_name(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == "*.*" {
        return true;
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = name.chars().collect();
    if p.is_empty() || s.is_empty() {
        return p.is_empty() && s.is_empty();
    }
    let eq = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let upper = |c: char| c.to_uppercase().next().unwrap_or(c);
    let in_set = |set: &[char], ch: char| {
        let mut i = 0;
        while i < set.len() {
            if i + 2 < set.len() && set[i + 1] == '-' {
                if ch >= upper(set[i]) && ch <= upper(set[i + 2]) {
                    return true;
                }
                i += 3;
                continue;
            }
            if eq(ch, set[i]) {
                return true;
            }
            i += 1;
        }
        false
    };
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star_pi, mut star_si): (Option<usize>, usize) = (None, 0);
    let mut has_dot = false;
    // Far's try_backtrack.
    macro_rules! backtrack {
        () => {
            match star_pi {
                Some(sp) if star_si < s.len() => {
                    star_si += 1;
                    si = star_si;
                    pi = sp;
                    continue;
                }
                _ => return false,
            }
        };
    }
    while si <= s.len() {
        if si < s.len() && s[si] == '.' {
            has_dot = true;
        }
        if pi >= p.len() {
            if si == s.len() {
                return true;
            }
            backtrack!();
        }
        let pc = p[pi];
        if pc == '*' {
            pi += 1;
            star_pi = Some(pi);
            star_si = si;
            continue;
        }
        if pc == '?' {
            if si == s.len() {
                return false;
            }
            pi += 1;
            si += 1;
            continue;
        }
        if pc == '['
            && let Some(len) = p[pi + 1..].iter().position(|&c| c == ']')
        {
            let set = &p[pi + 1..pi + 1 + len];
            if set.is_empty() {
                pi += 2;
                continue;
            }
            if si == s.len() {
                return false;
            }
            if in_set(set, upper(s[si])) {
                pi += len + 2;
                si += 1;
                continue;
            }
            backtrack!();
        }
        if si < s.len() && eq(pc, s[si]) {
            pi += 1;
            si += 1;
            continue;
        }
        if si == s.len() {
            let tail: String = p[pi..].iter().collect();
            if tail == ".*" || (!has_dot && tail == ".") {
                return true;
            }
        }
        backtrack!();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_like_far() {
        assert!(cmp_name("*.txt", "a.TXT"));
        assert!(!cmp_name("*.txt", "a.txt.bak"));
        assert!(cmp_name("test.*", "test"));
        assert!(cmp_name("t*.", "test"));
        assert!(!cmp_name("t*.", "test.x"));
        assert!(cmp_name("[a-cf]*.txt", "Bob.txt"));
        assert!(!cmp_name("[a-cf]*.txt", "dog.txt"));
        assert!(cmp_name("A?Z*", "abz"));
        assert!(cmp_name("*a*a*b", "xaxaab"));
        assert!(cmp_name("*.*.2", "1.1.2"));
    }

    #[test]
    fn mask_lists() {
        let m = FileMasks::parse("*.rs;*.toml").unwrap();
        assert!(m.matches("main.rs") && m.matches("Cargo.toml") && !m.matches("a.md"));
        let m = FileMasks::parse("*.*|*.bak,*.tmp").unwrap();
        assert!(m.matches("a.rs") && m.matches("noext") && !m.matches("x.tmp"));
        let m = FileMasks::parse("|*.md").unwrap();
        assert!(m.matches("a.rs") && !m.matches("README.md"));
        let m = FileMasks::parse("\"a,b.txt\"").unwrap();
        assert!(m.matches("a,b.txt") && !m.matches("a"));
        let m = FileMasks::parse("<temp>").unwrap();
        assert!(m.matches("x.bak") && !m.matches("x.rs"));
        let m = FileMasks::parse("/^\\d+\\.log$/i").unwrap();
        assert!(m.matches("12.LOG") && !m.matches("a12.log"));
        assert!(FileMasks::parse("/(/").is_none());
        assert!(FileMasks::parse("").is_none());
        assert!(FileMasks::parse(" ; ").is_none());
    }
}
