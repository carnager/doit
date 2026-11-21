//! Desktop applications from XDG `.desktop` files.

use super::SourceMsg;
use crate::item::{Action, Item};
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn spawn(tx: Sender<SourceMsg>, idx: usize) {
    std::thread::spawn(move || {
        let mut items = scan();
        items.sort_by_cached_key(|i| i.title.to_lowercase());
        for it in &mut items {
            it.source = idx;
        }
        let _ = tx.send(SourceMsg::Append(items));
        let _ = tx.send(SourceMsg::Done(idx));
    });
}

pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    let home_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".local/share"));
    dirs.push(home_data);
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() { "/usr/local/share:/usr/share".into() } else { sys };
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs
}

fn scan() -> Vec<Item> {
    let locale = Locale::from_env();
    let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(256);
    for dir in data_dirs() {
        let mut files = vec![];
        collect(&dir.join("applications"), "", &mut files);
        for (id, path) in files {
            if !seen.insert(id.clone()) {
                continue; // earlier dirs take precedence
            }
            if let Some(item) = parse(&path, &id, &locale, &desktops, &path_dirs) {
                out.push(item);
            }
        }
    }
    out
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect(&p, &format!("{prefix}{name}-"), out);
        } else if name.ends_with(".desktop") {
            out.push((format!("{prefix}{name}"), p));
        }
    }
}

struct Locale {
    variants: Vec<String>,
}

impl Locale {
    fn from_env() -> Locale {
        let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default();
        // lang_COUNTRY.ENCODING@MODIFIER
        let no_enc: String = {
            let (base, modi) = raw.split_once('@').map_or((raw.as_str(), None), |(a, b)| (a, Some(b)));
            let base = base.split('.').next().unwrap_or("");
            match modi {
                Some(m) => format!("{base}@{m}"),
                None => base.to_string(),
            }
        };
        let mut variants = vec![];
        if !no_enc.is_empty() && no_enc != "C" && no_enc != "POSIX" {
            variants.push(no_enc.clone());
            if let Some((lang, _)) = no_enc.split_once('_') {
                variants.push(lang.to_string());
            }
        }
        Locale { variants }
    }
}

type Group = HashMap<String, String>;

fn get<'a>(g: &'a Group, key: &str, loc: &Locale) -> Option<&'a str> {
    for v in &loc.variants {
        if let Some(s) = g.get(&format!("{key}[{v}]")) {
            return Some(s);
        }
    }
    g.get(key).map(|s| s.as_str())
}

fn list(s: Option<&str>) -> Vec<String> {
    s.map(|s| s.split(';').filter(|x| !x.is_empty()).map(|x| x.trim().to_string()).collect())
        .unwrap_or_default()
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('s') => out.push(' '),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse(path: &Path, id: &str, loc: &Locale, desktops: &[String], path_dirs: &[PathBuf]) -> Option<Item> {
    let src = std::fs::read_to_string(path).ok()?;
    let mut groups: Vec<(String, Group)> = vec![];
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            groups.push((line[1..line.len() - 1].to_string(), Group::new()));
        } else if let Some((k, v)) = line.split_once('=')
            && let Some((_, g)) = groups.last_mut()
        {
            g.insert(k.trim().to_string(), unescape(v.trim()));
        }
    }
    let main = &groups.iter().find(|(n, _)| n == "Desktop Entry")?.1;
    if main.get("Type").map(|s| s.as_str()) != Some("Application") {
        return None;
    }
    let truthy = |k: &str| main.get(k).is_some_and(|v| v == "true");
    if truthy("NoDisplay") || truthy("Hidden") {
        return None;
    }
    let only = list(main.get("OnlyShowIn").map(|s| s.as_str()));
    if !only.is_empty() && !only.iter().any(|d| desktops.contains(&d.to_ascii_lowercase())) {
        return None;
    }
    let not = list(main.get("NotShowIn").map(|s| s.as_str()));
    if not.iter().any(|d| desktops.contains(&d.to_ascii_lowercase())) {
        return None;
    }
    if let Some(te) = main.get("TryExec")
        && !exists_in_path(te, path_dirs)
    {
        return None;
    }
    let name = get(main, "Name", loc)?.to_string();
    let icon = main.get("Icon").cloned();
    let exec = main.get("Exec")?;
    let argv = parse_exec(exec, icon.as_deref(), &name, path);
    if argv.is_empty() {
        return None;
    }
    let generic = get(main, "GenericName", loc).map(str::to_string);
    let comment = get(main, "Comment", loc).map(str::to_string);
    let mut keywords = list(get(main, "Keywords", loc));
    if let Some(g) = &generic {
        keywords.push(g.clone());
    }
    // Let "code" find "Visual Studio Code" via its binary name.
    if let Some(bin) = Path::new(&argv[0]).file_name() {
        keywords.push(bin.to_string_lossy().into_owned());
    }
    keywords.extend(list(main.get("Categories").map(|s| s.as_str())));

    let mut actions = vec![];
    for a in list(main.get("Actions").map(|s| s.as_str())) {
        if let Some((_, g)) = groups.iter().find(|(n, _)| *n == format!("Desktop Action {a}"))
            && let (Some(t), Some(e)) = (get(g, "Name", loc), g.get("Exec"))
        {
            let argv = parse_exec(e, icon.as_deref(), &name, path);
            actions.push(Action {
                title: t.to_string(),
                exec: Some(shell_join(&argv)),
                icon: g.get("Icon").cloned().or_else(|| icon.clone()),
                id: Some(a.clone()),
                keep_open: false,
                set_query: None,
            });
        }
    }

    Some(Item {
        id: Some(id.trim_end_matches(".desktop").to_string()),
        title: name,
        subtitle: comment.or(generic),
        icon,
        keywords,
        terminal: truthy("Terminal"),
        actions,
        argv: Some(argv),
        ..Default::default()
    })
}

fn exists_in_path(bin: &str, dirs: &[PathBuf]) -> bool {
    if bin.contains('/') {
        return Path::new(bin).exists();
    }
    dirs.iter().any(|d| d.join(bin).exists())
}

/// Split an Exec= value per the Desktop Entry spec and expand field codes.
pub fn parse_exec(exec: &str, icon: Option<&str>, name: &str, path: &Path) -> Vec<String> {
    let mut args = vec![];
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    let mut it = exec.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            '\\' if in_q => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !in_q => {
                if has || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        args.push(cur);
    }
    let mut out = vec![];
    for a in args {
        match a.as_str() {
            "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            "%i" => {
                if let Some(i) = icon {
                    out.push("--icon".into());
                    out.push(i.into());
                }
            }
            _ => {
                let mut s = String::new();
                let mut ch = a.chars();
                while let Some(c) = ch.next() {
                    if c == '%' {
                        match ch.next() {
                            Some('%') => s.push('%'),
                            Some('c') => s.push_str(name),
                            Some('k') => s.push_str(&path.to_string_lossy()),
                            _ => {}
                        }
                    } else {
                        s.push(c);
                    }
                }
                out.push(s);
            }
        }
    }
    out
}

pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Vec<String> {
        parse_exec(s, Some("icn"), "App", Path::new("/x.desktop"))
    }

    #[test]
    fn exec_field_codes() {
        assert_eq!(p("firefox %u"), ["firefox"]);
        assert_eq!(p("app --name=%c %F"), ["app", "--name=App"]);
        assert_eq!(p("app %i"), ["app", "--icon", "icn"]);
        assert_eq!(p("app 100%%"), ["app", "100%"]);
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(p(r#""/opt/My App/bin" --flag "a \"q\" b""#), ["/opt/My App/bin", "--flag", r#"a "q" b"#]);
        assert_eq!(p(r#"sh -c "echo \$HOME""#), ["sh", "-c", "echo $HOME"]);
        assert_eq!(p(r#"x """#), ["x", ""]);
    }

    #[test]
    fn join_roundtrip() {
        assert_eq!(shell_join(&["a".into(), "b c".into(), "it's".into()]), r"a 'b c' 'it'\''s'");
    }
}
