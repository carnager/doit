//! Executables on $PATH.

use super::{Batcher, SourceMsg};
use crate::item::Item;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;

pub fn spawn(tx: Sender<SourceMsg>, idx: usize) {
    std::thread::spawn(move || {
        let mut b = Batcher::new(tx, idx);
        let mut seen = HashSet::new();
        let Some(path) = std::env::var_os("PATH") else { return };
        for dir in std::env::split_paths(&path) {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut names: Vec<(String, std::path::PathBuf)> = vec![];
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if seen.contains(&name) {
                    continue;
                }
                let Ok(md) = std::fs::metadata(e.path()) else { continue };
                if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                    seen.insert(name.clone());
                    names.push((name, e.path()));
                }
            }
            names.sort();
            for (name, p) in names {
                let item = Item {
                    id: Some(format!("run:{name}")),
                    subtitle: Some(p.to_string_lossy().into_owned()),
                    icon: Some("utilities-terminal".into()),
                    argv: Some(vec![name.clone()]),
                    title: name,
                    ..Default::default()
                };
                if !b.push(item) {
                    return;
                }
            }
        }
        b.flush();
        let _ = b.tx().send(SourceMsg::Done(idx));
    });
}
