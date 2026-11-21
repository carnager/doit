#![allow(clippy::too_many_arguments)]

mod config;
mod fonts;
mod history;
mod icons;
mod instance;
mod item;
mod matcher;
mod providers;
mod render;
mod state;
mod wayland;

use anyhow::{Result, bail};
use clap::Parser;
use config::Config;
use item::Item;
use providers::Source;
use smithay_client_toolkit::reexports::calloop::channel;
use state::{Outcome, State};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};

/// doit — a fast, structured launcher for Wayland.
#[derive(Parser)]
#[command(version, about, after_help = AFTER_HELP)]
struct Cli {
    /// Comma-separated modes: apps, run, or provider names from config.toml.
    #[arg(short, long, value_delimiter = ',')]
    modes: Vec<String>,
    /// Run a provider command for this invocation only (no config needed).
    #[arg(short = 'P', long, value_name = "CMD")]
    provider: Vec<String>,
    /// Read items from stdin (plain lines or JSON objects). Implied when stdin is piped.
    #[arg(short, long)]
    dmenu: bool,
    /// Label shown in the input field.
    #[arg(short, long)]
    prompt: Option<String>,
    /// Placeholder text for the empty input.
    #[arg(long)]
    placeholder: Option<String>,
    /// Initial query.
    #[arg(short, long, default_value = "")]
    query: String,
    /// Theme name or path to a .toml file.
    #[arg(short, long)]
    theme: Option<String>,
    /// Output format for selections that aren't executed.
    #[arg(short, long, value_enum, default_value_t = Format::Plain)]
    format: Format,
    /// Never execute; always print the selection.
    #[arg(long)]
    no_exec: bool,
    /// Mark several items with Ctrl+Space; Enter prints all marked ones.
    #[arg(short = 'M', long)]
    multi: bool,
    /// Mask the input; with no items, Enter returns what was typed.
    #[arg(long)]
    password: bool,
    /// Accept as soon as only one item matches.
    #[arg(short = 'a', long)]
    auto_select: bool,
    /// Preselect the first item with this title.
    #[arg(short = 's', long, value_name = "TITLE")]
    select: Option<String>,
    /// Alt+Tab style: Tab cycles, releasing the held modifier accepts, the
    /// second item starts selected. Bind it to a key with a modifier.
    /// `--switcher prev` moves backwards (bind it to Shift+Alt+Tab).
    #[arg(long, value_name = "DIRECTION", num_args = 0..=1, default_missing_value = "next")]
    switcher: Option<String>,
    /// A line of text shown in the footer.
    #[arg(long, value_name = "TEXT")]
    message: Option<String>,
    /// Disallow returning free-form input (Ctrl+Enter).
    #[arg(long)]
    no_custom: bool,
    /// Frecency namespace (enables ranking by usage in dmenu mode).
    #[arg(long)]
    history: Option<String>,
    /// Number of visible rows (overrides theme).
    #[arg(short, long)]
    lines: Option<usize>,
    /// Width in logical pixels (overrides theme).
    #[arg(short, long)]
    width: Option<u32>,
    /// List built-in themes and exit.
    #[arg(long)]
    list_themes: bool,
    /// Print the resolved theme as TOML and exit.
    #[arg(long)]
    dump_theme: bool,
    /// Render one frame offscreen to a PNG instead of opening a window.
    #[arg(long, value_name = "PNG", hide = true)]
    screenshot: Option<std::path::PathBuf>,
    /// Scale factor for --screenshot.
    #[arg(long, default_value_t = 2.0, hide = true)]
    screenshot_scale: f32,
}

const AFTER_HELP: &str = "\
Exit status: 0 on selection, 1 when cancelled, 2 on error,
10–18 when accepted with Alt+1…9 (the selection is printed, not run).

Examples:
  doit                          launch applications
  doit -m apps,run              apps and $PATH binaries in one list
  ls | doit -p files            dmenu mode
  echo '{\"title\":\"Lock\",\"icon\":\"system-lock-screen\",\"exec\":\"loginctl lock-session\"}' | doit
";

#[derive(Clone, Copy, clap::ValueEnum, PartialEq)]
enum Format {
    /// The item title (dmenu compatible)
    Plain,
    /// The full item as JSON, plus `action` and `query`
    Json,
    /// The item id (falls back to title)
    Id,
    /// The zero-based index of the item in the input
    Index,
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("doit: {e:#}");
            ExitCode::from(2)
        }
    }
}

pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn real_main() -> Result<ExitCode> {
    START.get_or_init(std::time::Instant::now);
    let cli = Cli::parse();
    if cli.list_themes {
        for (n, _) in config::BUILTIN_THEMES {
            println!("{n}");
        }
        if let Ok(rd) = std::fs::read_dir(config::config_dir().join("themes")) {
            for e in rd.flatten() {
                if let Some(n) = e.file_name().to_str().and_then(|n| n.strip_suffix(".toml")) {
                    println!("{n}  (user)");
                }
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut cfg = Config::load(cli.theme.as_deref())?;
    if cli.dump_theme {
        println!("{}", config::dump_theme(&cfg.theme));
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(l) = cli.lines {
        cfg.theme.rows = l; // 0: just the input, no list
    }
    if let Some(w) = cli.width {
        cfg.theme.width = w.max(200);
    }

    let dmenu = cli.dmenu
        || (cli.modes.is_empty() && cli.provider.is_empty() && (cli.password || stdin_has_data()));
    // A launcher (not a script's dmenu call) toggles: if one is open, close it.
    let instance = if dmenu || cli.screenshot.is_some() {
        None
    } else {
        let msg = match cli.switcher.as_deref() {
            Some("prev") => "prev",
            Some(_) => "next",
            None => "quit",
        };
        match instance::claim(msg) {
            Ok(instance::Claim::ClosedOther) => return Ok(ExitCode::from(1)),
            Ok(instance::Claim::Owner(i)) => Some(i),
            Err(_) => None, // no runtime dir etc.: just run
        }
    };

    // Start loading fonts + icon theme immediately; it overlaps Wayland setup.
    let (rtx, rrx) = std::sync::mpsc::channel();
    {
        let icon_theme = cfg.theme.icon_theme.clone();
        let theme = cfg.theme.clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let icons = std::thread::spawn(move || icons::Icons::new(icon_theme.as_deref()));
            let fonts = fonts::font_system(&theme.font);
            let tf = t.elapsed();
            let icons = icons.join().expect("icon thread");
            let ti = t.elapsed();
            let mut r = render::Renderer::new(fonts, icons);
            r.warm_up(&theme);
            if std::env::var_os("DOIT_TIMING").is_some() {
                eprintln!("fonts {:?}, icon theme {:?}, warm-up {:?}", tf, ti - tf, t.elapsed() - ti);
            }
            let _ = rtx.send(r);
        });
    }

    let (tx, rx) = channel::channel::<providers::SourceMsg>();
    let mut sources: Vec<Source> = vec![];
    let modes: Vec<String> = if dmenu || (cli.modes.is_empty() && !cli.provider.is_empty()) {
        vec![]
    } else if cli.modes.is_empty() {
        let mut m = cfg.modes.clone();
        m.extend(cfg.providers.iter().filter(|p| p.default || p.prefix.is_some()).map(|p| p.name.clone()));
        m
    } else {
        cli.modes.clone()
    };
    if dmenu {
        let mut s = Source::new("dmenu");
        s.name = cli.prompt.clone().unwrap_or_else(|| "dmenu".into());
        providers::stdin::spawn(tx.clone(), 0);
        sources.push(s);
    }
    for m in &modes {
        if sources.iter().any(|s| &s.name == m) {
            continue;
        }
        let idx = sources.len();
        match m.as_str() {
            "apps" | "drun" => {
                providers::apps::spawn(tx.clone(), idx);
                sources.push(Source::new("apps"));
            }
            "run" => {
                providers::run::spawn(tx.clone(), idx);
                let mut s = Source::new("run");
                // Only prefix-route run when combined with other modes.
                if modes.len() > 1 {
                    s.prefix = Some(">".into());
                }
                sources.push(s);
            }
            name => {
                let Some(p) = cfg.providers.iter().find(|p| p.name == name) else {
                    bail!("unknown mode '{name}' (define it as [[provider]] in config.toml)");
                };
                let mut s = Source::new(p.name.clone());
                s.prefix = if modes.len() > 1 { p.prefix.clone() } else { None };
                s.live = p.live;
                s.icon = p.icon.clone();
                let (qtx, worker) = providers::script::spawn(&p.exec, tx.clone(), idx);
                s.query_tx = Some(qtx);
                s.worker = worker;
                sources.push(s);
            }
        }
    }
    for cmd in &cli.provider {
        let idx = sources.len();
        let mut s = Source::new(cli.prompt.clone().unwrap_or_else(|| "provider".into()));
        let (qtx, worker) = providers::script::spawn(cmd, tx.clone(), idx);
        s.query_tx = Some(qtx);
        s.worker = worker;
        sources.push(s);
    }
    if sources.is_empty() {
        bail!("no modes selected");
    }
    drop(tx);

    let history = match (&cli.history, dmenu, cfg.frecency) {
        (Some(ns), _, _) => history::History::load(ns),
        (None, false, true) => history::History::load("launch"),
        _ => history::History::disabled(),
    };

    let mut st = State::new(sources, history, cfg.theme.rows);
    // Free-form input is meaningful for dmenu/scripts, not for launching apps.
    st.allow_custom = !cli.no_custom
        && (dmenu || !cli.provider.is_empty() || !cfg.providers.is_empty() && modes.iter().any(|m| m != "apps" && m != "run"));
    st.multi = cli.multi;
    st.password = cli.password;
    st.auto_select = cli.auto_select;
    st.preselect = cli.select.clone();
    st.message = cli.message.clone();
    st.switcher = cli.switcher.is_some();
    if let Some(p) = &cli.placeholder {
        st.placeholder = p.clone();
    } else if !dmenu && modes.len() == 1 && modes[0] == "apps" {
        st.placeholder = "Search apps…".into();
    }
    if dmenu {
        st.prompt = cli.prompt.clone();
    }
    st.query = cli.query.clone();
    st.cursor = st.query.len();

    if let Some(out) = &cli.screenshot {
        let mut st = screenshot(st, &cfg, rrx, rx, out, cli.screenshot_scale)?;
        if std::env::var_os("DOIT_ACCEPT").is_some() {
            st.accept();
            return finish(st, &cfg, &cli);
        }
        return Ok(ExitCode::SUCCESS);
    }

    let st = wayland::run(st, cfg.theme.clone(), rrx, rx, instance.as_ref().map(|i| &i.listener))?;
    drop(instance);
    finish(st, &cfg, &cli)
}

fn finish(mut st: State, cfg: &Config, cli: &Cli) -> Result<ExitCode> {
    let code = finish_outcome(&mut st, cfg, cli);
    st.shutdown_providers();
    code
}

fn finish_outcome(st: &mut State, cfg: &Config, cli: &Cli) -> Result<ExitCode> {
    let outcome = st.outcome.take().unwrap_or(Outcome::Cancel);
    let query = st.effective_query().to_string();
    match outcome {
        Outcome::Cancel => Ok(ExitCode::from(1)),
        Outcome::Handled => Ok(ExitCode::SUCCESS),
        Outcome::Multi(idxs) => {
            for idx in idxs {
                let item = st.items[idx].clone();
                let input_index = st.items[..idx].iter().filter(|i| i.source == item.source).count();
                let executed = !cli.no_exec && activate(cfg, &item, None)?;
                st.history.bump(item.key());
                if !executed {
                    print_item(cli.format, &item, None, &query, input_index);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Outcome::Custom(q) => {
            match cli.format {
                Format::Json => println!("{}", serde_json::json!({ "custom": true, "query": q })),
                _ => println!("{q}"),
            }
            Ok(st.exit_code.map_or(ExitCode::SUCCESS, ExitCode::from))
        }
        Outcome::Item { idx, action } => {
            let item = st.items[idx].clone();
            let input_index = st.items[..idx].iter().filter(|i| i.source == item.source).count();
            let act = action.and_then(|a| item.actions.get(a).cloned());
            // Custom keys hand the choice back to the script instead of running it.
            let executed = !cli.no_exec && st.exit_code.is_none() && activate(cfg, &item, act.as_ref())?;
            st.history.bump(item.key());
            if !executed {
                print_item(cli.format, &item, act.as_ref(), &query, input_index);
            }
            Ok(st.exit_code.map_or(ExitCode::SUCCESS, ExitCode::from))
        }
    }
}

fn print_item(fmt: Format, item: &Item, act: Option<&item::Action>, query: &str, index: usize) {
    match fmt {
        Format::Plain => println!("{}", item.title),
        Format::Id => println!("{}", item.key()),
        Format::Index => println!("{index}"),
        Format::Json => {
            let mut v = serde_json::to_value(item).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.insert("query".into(), query.into());
                o.insert("index".into(), index.into());
                if let Some(a) = act {
                    o.insert("action".into(), serde_json::to_value(a).unwrap_or_default());
                }
            }
            println!("{v}");
        }
    }
}

/// Returns Ok(true) if something was launched.
fn activate(cfg: &Config, item: &Item, act: Option<&item::Action>) -> Result<bool> {
    let argv: Vec<String> = match act {
        Some(a) => match &a.exec {
            Some(e) => vec!["sh".into(), "-c".into(), e.clone()],
            None => return Ok(false),
        },
        None => match (&item.argv, &item.exec) {
            (Some(argv), _) => argv.clone(),
            (None, Some(e)) => vec!["sh".into(), "-c".into(), e.clone()],
            (None, None) => return Ok(false),
        },
    };
    let mut full = cfg.launch_prefix.clone();
    if item.terminal {
        full.extend(terminal_argv(cfg));
    }
    full.extend(argv);
    let mut cmd = Command::new(&full[0]);
    cmd.args(&full[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(config::home())
        .process_group(0);
    // SAFETY: setsid is async-signal-safe; detaches the child from our session.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| anyhow::anyhow!("launching {:?}: {e}", full[0]))?;
    Ok(true)
}

fn terminal_argv(cfg: &Config) -> Vec<String> {
    let term = cfg
        .terminal
        .clone()
        .or_else(|| std::env::var("TERMINAL").ok().filter(|t| !t.is_empty()))
        .or_else(|| {
            ["xdg-terminal-exec", "foot", "kitty", "alacritty", "ghostty", "wezterm", "konsole", "gnome-terminal", "xterm"]
                .iter()
                .find(|t| which(t))
                .map(|t| t.to_string())
        })
        .unwrap_or_else(|| "xterm".into());
    let mut parts: Vec<String> = term.split_whitespace().map(String::from).collect();
    if parts.len() == 1 {
        let base = std::path::Path::new(&parts[0]).file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        match base.as_str() {
            "foot" | "kitty" | "xdg-terminal-exec" => {}
            "wezterm" => parts.extend(["start".into(), "--".into()]),
            "gnome-terminal" | "kgx" => parts.push("--".into()),
            _ => parts.push("-e".into()),
        }
    }
    parts
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn screenshot(
    mut st: State,
    cfg: &Config,
    rrx: std::sync::mpsc::Receiver<render::Renderer>,
    rx: channel::Channel<providers::SourceMsg>,
    out: &std::path::Path,
    scale: f32,
) -> Result<State> {
    use smithay_client_toolkit::reexports::calloop::{EventLoop, channel::Event};
    let mut ev: EventLoop<State> = EventLoop::try_new()?;
    ev.handle()
        .insert_source(rx, |e, _, st: &mut State| {
            if let Event::Msg(m) = e {
                st.on_source(m)
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let t0 = std::time::Instant::now();
    let wait = std::env::var("DOIT_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    while t0.elapsed() < std::time::Duration::from_millis(wait) {
        st.rank_if_needed();
        ev.dispatch(Some(std::time::Duration::from_millis(20)), &mut st)?;
    }
    st.rank_if_needed();
    if let Ok(n) = std::env::var("DOIT_SELECT") {
        st.move_sel(n.parse().unwrap_or(0), false);
    }
    if std::env::var_os("DOIT_ACTIONS").is_some() {
        st.toggle_actions();
    }
    let mut r = rrx.recv()?;
    let l = render::Layout::new(&cfg.theme);
    let (w, h) = l.surface_size();
    let mut pm = tiny_skia::Pixmap::new((w as f32 * scale) as u32, (h as f32 * scale) as u32).unwrap();
    let t = std::time::Instant::now();
    r.draw(&mut pm, &mut st, &cfg.theme, &l, scale);
    eprintln!("first frame: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    r.draw(&mut pm, &mut st, &cfg.theme, &l, scale);
    eprintln!("warm frame: {:?}", t.elapsed());
    pm.save_png(out)?;
    Ok(st)
}

/// True when stdin is a pipe, file or socket — not a TTY and not /dev/null
/// (compositor keybinds usually start us with /dev/null).
fn stdin_has_data() -> bool {
    // SAFETY: fstat on fd 0 with a zeroed, properly sized stat buffer.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &mut st) != 0 {
            return false;
        }
        matches!(st.st_mode & libc::S_IFMT, libc::S_IFIFO | libc::S_IFREG | libc::S_IFSOCK)
    }
}
