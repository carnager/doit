//! External provider processes speaking the doit line protocol.
//!
//! doit → provider (stdin):   {"query":"text"}            on every query change
//!                            {"activate":{"id","action"},"query"}
//!                                                        when the provider declared activations
//! provider → doit:           {"activations":true}        "send me activations instead of
//!                                                        running/printing my items"
//! provider → doit (stdout):  {"title":"…", …}             append an item
//!                            {"items":[…]}               replace all items
//!                            {"clear":true}              remove all items
//!                            {"set_query":"…"}           navigate (while this provider is active)
//!                            {"scope":{"match","label"}} queries starting with `match` are a
//!                                                        drilled-in view: hidden behind a pill,
//!                                                        the rest is what doit matches; null ends it
//!                            {"layout":{"columns":N,"aspect":1.5}}
//!                                                        grid of N columns, icons h/w = aspect;
//!                                                        {"layout":null} list
//!                            {"live":true|false}         whether the provider filters itself
//!                            {"message":"…"|null}        a line in the footer
//!                            {"prompt":"…"} / {"placeholder":"…"}
//!                            any non-JSON line            plain item

use super::{Batcher, SourceMsg, ToProvider};
use crate::item::Item;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

pub type Spawned = (std::sync::mpsc::Sender<ToProvider>, Option<std::thread::JoinHandle<()>>);

pub fn spawn(exec: &str, tx: Sender<SourceMsg>, idx: usize) -> Spawned {
    let (qtx, qrx) = std::sync::mpsc::channel::<ToProvider>();
    let child = Command::new("sh")
        .arg("-c")
        .arg(crate::config::expand_tilde(exec))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("doit: provider '{exec}': {e}");
            return (qtx, None);
        }
    };
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();

    let worker = std::thread::spawn(move || {
        let mut pending: Vec<ToProvider> = vec![];
        while let Ok(first) = qrx.recv() {
            pending.push(first);
            pending.extend(qrx.try_iter());
            // Coalesce keystroke bursts: of consecutive queries only the last matters.
            let mut msgs: Vec<ToProvider> = vec![];
            for m in pending.drain(..) {
                if let (ToProvider::Query(_), Some(ToProvider::Query(_))) = (&m, msgs.last()) {
                    msgs.pop();
                }
                msgs.push(m);
            }
            let Some(w) = stdin.as_mut() else { continue };
            for m in msgs {
                let line = match m {
                    ToProvider::Query(q) => serde_json::json!({ "query": q }),
                    ToProvider::Activate { id, action, query } => {
                        serde_json::json!({ "activate": { "id": id, "action": action }, "query": query })
                    }
                };
                if writeln!(w, "{line}").and_then(|_| w.flush()).is_err() {
                    stdin = None; // static provider exited; that's fine
                    break;
                }
            }
        }
        // doit is closing: EOF on stdin, then give the provider a moment to
        // finish what it was asked (e.g. a last activation) before killing it.
        drop(stdin);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = child.kill();
        let _ = child.wait();
    });

    std::thread::spawn(move || {
        let mut b = Batcher::new(tx.clone(), idx);
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t.starts_with('{')
                && let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(t)
            {
                if let Some(items) = obj.get("items") {
                    b.flush();
                    let mut items: Vec<Item> = serde_json::from_value(items.clone()).unwrap_or_default();
                    items.iter_mut().for_each(|i| i.source = idx);
                    let _ = tx.send(SourceMsg::Replace(idx, items));
                    continue;
                }
                if obj.get("clear").and_then(|v| v.as_bool()) == Some(true) {
                    b.flush();
                    let _ = tx.send(SourceMsg::Replace(idx, vec![]));
                    continue;
                }
                if let Some(v) = obj.get("scope") {
                    let scope = v.as_object().and_then(|o| {
                        let m = o.get("match")?.as_str()?.to_string();
                        let label = o.get("label").and_then(|l| l.as_str()).unwrap_or(&m).to_string();
                        Some((m, label))
                    });
                    let _ = tx.send(SourceMsg::Scope(idx, scope));
                }
                if let Some(v) = obj.get("layout") {
                    // A number, or "auto": as many ~150 px cells as fit (0 here).
                    let cols = match v.get("columns") {
                        Some(serde_json::Value::String(s)) if s == "auto" => Some(0),
                        c => c.and_then(|c| c.as_u64()).filter(|c| *c > 1).map(|c| c as usize),
                    };
                    let aspect = v.get("aspect").and_then(|a| a.as_f64()).map_or(1.0, |a| a.clamp(0.3, 3.0) as f32);
                    let _ = tx.send(SourceMsg::Grid(idx, cols.map(|c| (c, aspect))));
                }
                if let Some(v) = obj.get("message") {
                    let _ = tx.send(SourceMsg::Message(idx, v.as_str().map(String::from)));
                }
                if let Some(v) = obj.get("live").and_then(|v| v.as_bool()) {
                    let _ = tx.send(SourceMsg::Live(idx, v));
                }
                if let Some(q) = obj.get("set_query").and_then(|v| v.as_str()) {
                    let _ = tx.send(SourceMsg::SetQuery(idx, q.into()));
                }
                if obj.get("activations").and_then(|v| v.as_bool()) == Some(true) {
                    let _ = tx.send(SourceMsg::Activations(idx));
                }
                if let Some(p) = obj.get("prompt").and_then(|v| v.as_str()) {
                    let _ = tx.send(SourceMsg::Prompt(idx, p.into()));
                }
                if let Some(p) = obj.get("placeholder").and_then(|v| v.as_str()) {
                    let _ = tx.send(SourceMsg::Placeholder(idx, p.into()));
                }
                if obj.contains_key("title")
                    && let Ok(item) = serde_json::from_value::<Item>(serde_json::Value::Object(obj))
                {
                    b.push(item);
                    b.flush();
                }
                continue;
            }
            b.push(Item::plain(t));
        }
        b.flush();
        let _ = tx.send(SourceMsg::Done(idx));
    });
    (qtx, Some(worker))
}
