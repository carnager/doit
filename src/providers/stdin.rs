//! dmenu-compatible stdin: plain lines, or JSON item objects (one per line).

use super::{Batcher, SourceMsg};
use crate::item::Item;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use std::io::BufRead;

pub fn spawn(tx: Sender<SourceMsg>, idx: usize) {
    std::thread::spawn(move || {
        let mut b = Batcher::new(tx, idx);
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match lock.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(item) = Item::from_line(&line)
                        && !b.push(item)
                    {
                        return;
                    }
                }
            }
        }
        b.flush();
        let _ = b.tx().send(SourceMsg::Done(idx));
    });
}
