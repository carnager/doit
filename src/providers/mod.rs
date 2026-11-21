//! Item sources. Each runs on its own thread and streams items into the UI
//! event loop, so the window appears instantly and fills as data arrives.

pub mod apps;
pub mod run;
pub mod script;
pub mod stdin;

use crate::item::Item;
use smithay_client_toolkit::reexports::calloop::channel::Sender;

pub enum SourceMsg {
    Append(Vec<Item>),
    Replace(usize, Vec<Item>),
    Prompt(usize, String),
    Placeholder(usize, String),
    /// The provider handles activations of its items itself.
    Activations(usize),
    /// The provider navigates: replace the query (its prefix is kept).
    SetQuery(usize, String),
    /// A drilled-in view: (query text it starts with, label for the pill).
    Scope(usize, Option<(String, String)>),
    /// Show this provider's items as a grid: (columns, icon height/width); None: list.
    Grid(usize, Option<(usize, f32)>),
    /// The provider filters its items itself.
    Live(usize, bool),
    /// A line of text in the footer while this provider is active.
    Message(usize, Option<String>),
    /// This source has sent everything it is going to send.
    Done(usize),
}

/// Messages from doit to a script provider.
pub enum ToProvider {
    Query(String),
    Activate { id: Option<String>, action: Option<String>, query: String },
}

pub struct Source {
    pub name: String,
    pub prefix: Option<String>,
    /// Source filters itself; skip fuzzy filtering of its items.
    pub live: bool,
    pub icon: Option<String>,
    pub placeholder: Option<String>,
    pub prompt: Option<String>,
    /// Channel for forwarding queries to live/script sources.
    pub query_tx: Option<std::sync::mpsc::Sender<ToProvider>>,
    /// Joined on exit so a final activation can finish.
    pub worker: Option<std::thread::JoinHandle<()>>,
    pub activations: bool,
    pub scope: Option<(String, String)>,
    pub grid: Option<(usize, f32)>,
    pub message: Option<String>,
    pub done: bool,
}

impl Source {
    pub fn new(name: impl Into<String>) -> Source {
        Source { name: name.into(), prefix: None, live: false, icon: None, placeholder: None, prompt: None, query_tx: None, worker: None, activations: false, scope: None, grid: None, message: None, done: false }
    }
}

/// Batches items so huge streams don't flood the event loop with messages.
pub struct Batcher {
    tx: Sender<SourceMsg>,
    idx: usize,
    buf: Vec<Item>,
    last: std::time::Instant,
}

impl Batcher {
    pub fn new(tx: Sender<SourceMsg>, idx: usize) -> Self {
        Batcher { tx, idx, buf: Vec::new(), last: std::time::Instant::now() }
    }
    pub fn push(&mut self, mut item: Item) -> bool {
        item.source = self.idx;
        self.buf.push(item);
        if self.buf.len() >= 4096 || self.last.elapsed().as_millis() >= 12 {
            return self.flush();
        }
        true
    }
    pub fn tx(&self) -> &Sender<SourceMsg> {
        &self.tx
    }
    pub fn flush(&mut self) -> bool {
        self.last = std::time::Instant::now();
        if self.buf.is_empty() {
            return true;
        }
        self.tx.send(SourceMsg::Append(std::mem::take(&mut self.buf))).is_ok()
    }
}

/// Run a shell command fully detached from doit.
pub fn spawn_shell(cmd: &str) {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new("sh");
    c.arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let _ = c.spawn();
}
