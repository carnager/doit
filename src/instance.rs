//! One launcher at a time: starting doit while one is open closes it
//! instead (press the keybind again to dismiss). dmenu-style invocations
//! from scripts don't take part.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

pub struct Instance {
    pub listener: UnixListener,
    path: PathBuf,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("doit.sock")
}

pub enum Claim {
    /// We are the launcher; quit when someone connects.
    Owner(Instance),
    /// Another launcher was open and has been told what to do.
    ClosedOther,
}

/// `message` is what a second launch tells the open one: "quit" for the
/// launcher toggle, "next"/"prev" for the switcher (its keybind fires again
/// while Alt is held, since niri handles binds before doit sees the keys).
pub fn claim(message: &str) -> std::io::Result<Claim> {
    let path = socket_path();
    if let Ok(mut s) = UnixStream::connect(&path) {
        let _ = s.write_all(format!("{message}\n").as_bytes());
        return Ok(Claim::ClosedOther);
    }
    let _ = std::fs::remove_file(&path); // stale, from a crash
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    Ok(Claim::Owner(Instance { listener, path }))
}
