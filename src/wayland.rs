//! Wayland layer-shell frontend.

use crate::config::{Position, Theme};
use crate::providers::SourceMsg;
use crate::render::{Layout, Renderer};
use crate::state::State;
use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        DataDeviceManagerState,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
    },
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, LoopHandle, channel::{Channel, Event as ChEvent}},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_keyboard, wl_output,
                wl_pointer, wl_seat, wl_shm, wl_surface,
            },
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::Pixmap;

/// How long Mod+Tab must be held, without cycling, before releasing it keeps
/// the switcher open for searching rather than switching.
const SEARCH_PAUSE: std::time::Duration = std::time::Duration::from_millis(400);

pub struct App {
    registry: RegistryState,
    seat: SeatState,
    output: OutputState,
    shm: Shm,
    pool: Option<SlotPool>,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    has_fractional: bool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    data_devices: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    loop_handle: LoopHandle<'static, App>,
    mods: Modifiers,
    /// Switcher: None until the first modifier report, then whether a
    /// modifier was held when we got the keyboard (so its release accepts).
    armed: Option<bool>,
    opened: std::time::Instant,
    configured: bool,
    /// Scale in 120ths (wp_fractional_scale) — 120 == 1.0.
    scale120: u32,
    pub state: State,
    theme: Theme,
    layout: Layout,
    renderer: Option<Renderer>,
    renderer_rx: Option<std::sync::mpsc::Receiver<Renderer>>,
    pixmap: Option<Pixmap>,
    pub exit: bool,
    top_margin_set: bool,
    first_frame_done: bool,
}

struct FracData;
struct Noop;

impl Dispatch2<WpFractionalScaleV1, App> for FracData {
    fn event(&self, app: &mut App, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev
            && scale != app.scale120
        {
            app.scale120 = scale;
            app.state.dirty = true;
        }
    }
}
impl Dispatch2<WpFractionalScaleManagerV1, App> for Noop {
    fn event(&self, _: &mut App, _: &WpFractionalScaleManagerV1, _: <WpFractionalScaleManagerV1 as smithay_client_toolkit::reexports::client::Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}
impl Dispatch2<WpViewporter, App> for Noop {
    fn event(&self, _: &mut App, _: &WpViewporter, _: <WpViewporter as smithay_client_toolkit::reexports::client::Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}
impl Dispatch2<WpViewport, App> for Noop {
    fn event(&self, _: &mut App, _: &WpViewport, _: <WpViewport as smithay_client_toolkit::reexports::client::Proxy>::Event, _: &Connection, _: &QueueHandle<App>) {}
}

pub fn run(
    state: State,
    theme: Theme,
    renderer_rx: std::sync::mpsc::Receiver<Renderer>,
    source_rx: Channel<SourceMsg>,
    instance: Option<&std::os::unix::net::UnixListener>,
) -> Result<State> {
    let conn = Connection::connect_to_env().context("cannot connect to a Wayland compositor")?;
    let (globals, queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle()).map_err(|e| anyhow::anyhow!("{e}"))?;

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("compositor lacks wlr-layer-shell")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm missing")?;

    let surface = compositor.create_surface(&qh);
    let frac_mgr: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, Noop).ok();
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, Noop).ok();
    let (viewport, has_fractional) = match (&frac_mgr, &viewporter) {
        (Some(m), Some(v)) => {
            m.get_fractional_scale(&surface, &qh, FracData);
            (Some(v.get_viewport(&surface, &qh, Noop)), true)
        }
        _ => (None, false),
    };

    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("doit"), None);
    let layout = Layout::new(&theme);
    let (w, h) = layout.surface_size();
    match theme.position {
        Position::Center => layer.set_anchor(Anchor::empty()),
        Position::Top => layer.set_anchor(Anchor::TOP),
    }
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_exclusive_zone(-1);
    layer.set_size(w, h);
    layer.commit();

    event_loop
        .handle()
        .insert_source(source_rx, |ev, _, app: &mut App| {
            if let ChEvent::Msg(m) = ev {
                app.state.on_source(m);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if let Some(l) = instance {
        use smithay_client_toolkit::reexports::calloop::generic::Generic;
        use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction};
        let l = l.try_clone()?;
        event_loop
            .handle()
            .insert_source(Generic::new(l, Interest::READ, Mode::Level), |_, l, app: &mut App| {
                if let Ok((stream, _)) = l.accept() {
                    use std::io::BufRead;
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(100)));
                    let mut line = String::new();
                    let _ = std::io::BufReader::new(stream).read_line(&mut line);
                    match line.trim() {
                        "next" if app.state.switcher => app.state.switcher_step(1),
                        "prev" if app.state.switcher => app.state.switcher_step(-1),
                        _ => app.state.outcome = Some(crate::state::Outcome::Cancel),
                    }
                }
                Ok(PostAction::Continue)
            })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    let mut app = App {
        registry: RegistryState::new(&globals),
        seat: SeatState::new(&globals, &qh),
        output: OutputState::new(&globals, &qh),
        shm,
        pool: None,
        layer,
        viewport,
        has_fractional,
        keyboard: None,
        pointer: None,
        data_devices: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        loop_handle: event_loop.handle(),
        mods: Modifiers::default(),
        armed: None,
        opened: std::time::Instant::now(),
        configured: false,
        scale120: 120,
        state,
        theme,
        layout,
        renderer: None,
        renderer_rx: Some(renderer_rx),
        pixmap: None,
        exit: false,
        top_margin_set: false,
        first_frame_done: false,
    };

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
        app.state.rank_if_needed();
        if app.state.outcome.is_some() {
            break;
        }
        if app.state.dirty && app.configured {
            app.draw(&qh);
        }
    }
    // Unmap immediately so a launched app's window isn't covered.
    app.layer.wl_surface().attach(None, 0, 0);
    app.layer.commit();
    let _ = conn.flush();
    Ok(app.state)
}

impl App {
    fn scale(&self) -> f32 {
        self.scale120 as f32 / 120.0
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.renderer.is_none() {
            // Fonts load on a background thread; block only on first paint.
            if let Some(rx) = self.renderer_rx.take() {
                self.renderer = rx.recv().ok();
            }
            if self.renderer.is_none() {
                return;
            }
        }
        self.state.dirty = false;
        let s = self.scale();
        let (lw, lh) = self.layout.surface_size();
        let pw = (lw as f32 * s).round() as u32;
        let ph = (lh as f32 * s).round() as u32;
        if self.pixmap.as_ref().map(|p| (p.width(), p.height())) != Some((pw, ph)) {
            self.pixmap = Pixmap::new(pw, ph);
        }
        let Some(pm) = self.pixmap.as_mut() else { return };
        let r = self.renderer.as_mut().unwrap();
        r.draw(pm, &mut self.state, &self.theme, &self.layout, s);

        let stride = pw as i32 * 4;
        if self.pool.is_none() {
            self.pool = SlotPool::new((stride * ph as i32 * 2) as usize, &self.shm).ok();
        }
        let Some(pool) = self.pool.as_mut() else { return };
        let Ok((buffer, canvas)) = pool.create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888) else {
            return;
        };
        // RGBA (premultiplied) → BGRA little-endian ARGB8888
        for (d, s) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(pm.data().as_chunks::<4>().0) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
        let surf = self.layer.wl_surface();
        if let Some(vp) = &self.viewport {
            vp.set_destination(lw as i32, lh as i32);
            surf.set_buffer_scale(1);
        } else {
            surf.set_buffer_scale(self.scale120 as i32 / 120);
        }
        surf.damage_buffer(0, 0, pw as i32, ph as i32);
        let _ = buffer.attach_to(surf);
        self.layer.commit();
        if !self.first_frame_done {
            self.first_frame_done = true;
            if std::env::var_os("DOIT_TIMING").is_some()
                && let Some(t) = crate::START.get()
            {
                eprintln!("first frame committed after {:?} (scale {})", t.elapsed(), self.scale());
            }
        }
    }

    /// Insert the clipboard's text at the cursor (first line, trimmed).
    fn paste(&mut self) {
        use smithay_client_toolkit::reexports::calloop::PostAction;
        use std::io::Read;
        let Some(offer) = self.data_device.as_ref().and_then(|d| d.data().selection_offer()) else { return };
        let mime = offer.with_mime_types(|types| {
            ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"]
                .iter()
                .find(|m| types.iter().any(|t| t == *m))
                .map(|m| m.to_string())
        });
        let Some(mime) = mime else { return };
        let Ok(pipe) = offer.receive(mime) else { return };
        let mut buf: Vec<u8> = Vec::new();
        let _ = self.loop_handle.insert_source(pipe, move |_, f, app: &mut App| {
            // SAFETY: the file stays owned by the pipe; we only read from it.
            let f: &mut std::fs::File = unsafe { f.get_mut() };
            let mut chunk = [0u8; 8192];
            match f.read(&mut chunk) {
                Ok(0) => {
                    let text = String::from_utf8_lossy(&buf);
                    let line = text.lines().next().unwrap_or("").trim();
                    if !line.is_empty() {
                        app.state.insert(line);
                    }
                    PostAction::Remove
                }
                Ok(n) if buf.len() < 1 << 20 => {
                    buf.extend_from_slice(&chunk[..n]);
                    PostAction::Continue
                }
                Ok(_) => PostAction::Continue, // absurdly large; keep draining
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted || e.kind() == std::io::ErrorKind::WouldBlock => {
                    PostAction::Continue
                }
                Err(_) => PostAction::Remove,
            }
        });
    }

    fn handle_key(&mut self, ev: &KeyEvent) {
        if (self.mods.ctrl && ev.keysym == Keysym::v) || (self.mods.shift && ev.keysym == Keysym::Insert) {
            return self.paste();
        }
        let m = self.mods;
        let st = &mut self.state;
        let k = ev.keysym;
        match k {
            Keysym::Escape => st.back(),
            Keysym::space if m.ctrl => st.toggle_mark(),
            k if m.alt && (Keysym::_1.raw()..=Keysym::_9.raw()).contains(&k.raw()) => {
                st.accept_custom_key((k.raw() - Keysym::_0.raw()) as u8)
            }
            Keysym::Return | Keysym::KP_Enter => {
                if m.ctrl || m.shift {
                    st.accept_custom()
                } else {
                    st.accept()
                }
            }
            Keysym::Up | Keysym::KP_Up if st.grid().is_some() => st.move_sel(-(st.cols as i32), false),
            Keysym::Down | Keysym::KP_Down if st.grid().is_some() => st.move_sel(st.cols as i32, false),
            Keysym::Right if st.grid().is_some() && !m.ctrl => st.move_sel(1, false),
            Keysym::Up | Keysym::KP_Up if st.switcher => st.switcher_step(-1),
            Keysym::Down | Keysym::KP_Down if st.switcher => st.switcher_step(1),
            Keysym::ISO_Left_Tab if st.switcher => st.switcher_step(-1),
            Keysym::Tab if st.switcher => st.switcher_step(1),
            Keysym::Up | Keysym::KP_Up => st.move_sel(-1, true),
            Keysym::Down | Keysym::KP_Down => st.move_sel(1, true),
            Keysym::ISO_Left_Tab => st.move_sel(-1, true),
            Keysym::Tab => {
                if st.has_actions() || st.view != crate::state::View::List {
                    st.toggle_actions()
                } else {
                    st.move_sel(1, true)
                }
            }
            Keysym::Page_Up => st.move_sel(-(st.page() as i32), false),
            Keysym::Page_Down => st.move_sel(st.page() as i32, false),
            Keysym::Left | Keysym::KP_Left => st.left(m.ctrl),
            Keysym::Right => {
                // Right at end of input opens actions, like many modern launchers.
                if st.cursor == st.query.len() && !m.ctrl && st.view == crate::state::View::List && st.has_actions() {
                    st.toggle_actions()
                } else {
                    st.move_cursor(1, m.ctrl)
                }
            }
            Keysym::Home if m.ctrl => st.move_sel(-(st.hits.len() as i32), false),
            Keysym::End if m.ctrl => st.move_sel(st.hits.len() as i32, false),
            Keysym::Home => st.home(),
            Keysym::End => st.end(),
            Keysym::BackSpace => st.backspace(m.ctrl || m.alt),
            Keysym::Delete => st.delete(m.ctrl),
            _ if m.ctrl => match k {
                Keysym::n | Keysym::j => st.move_sel(1, true),
                Keysym::p | Keysym::k => st.move_sel(-1, true),
                Keysym::a => st.home(),
                Keysym::e => st.end(),
                Keysym::b => st.move_cursor(-1, false),
                Keysym::f => st.move_cursor(1, false),
                Keysym::w => st.backspace(true),
                Keysym::h => st.backspace(false),
                Keysym::u => st.clear_line(),
                Keysym::d => st.delete(false),
                Keysym::bracketleft | Keysym::c | Keysym::g => st.back(),
                Keysym::m => st.accept(),
                _ => {}
            },
            _ if m.alt => match k {
                Keysym::b => st.move_cursor(-1, true),
                Keysym::f => st.move_cursor(1, true),
                Keysym::d => st.delete(true),
                _ => {}
            },
            _ => {
                if let Some(t) = &ev.utf8
                    && !t.is_empty()
                    && !t.chars().any(|c| c.is_control())
                {
                    st.insert(t);
                }
            }
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, f: i32) {
        if !self.has_fractional && f as u32 * 120 != self.scale120 {
            self.scale120 = f as u32 * 120;
            self.state.dirty = true;
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        if self.theme.position == Position::Top
            && !self.top_margin_set
            && let Some(info) = self.output.info(output)
            && let Some((_, h)) = info.logical_size
        {
            self.top_margin_set = true;
            let m = (h as f32 * self.theme.top_offset - self.layout.margin).max(0.0) as i32;
            self.layer.set_margin(m, 0, 0, 0);
            self.layer.commit();
        }
    }
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
        self.state.outcome = Some(crate::state::Outcome::Cancel);
    }
    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        self.configured = true;
        self.draw(qh);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if self.data_device.is_none()
            && let Some(m) = &self.data_devices
        {
            self.data_device = Some(m.get_data_device(qh, &seat));
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self
                .seat
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|app: &mut App, _, ev| app.handle_key(&ev)),
                )
                .ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if cap == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        self.handle_key(&ev);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        self.handle_key(&ev);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.mods = m;
        if !self.state.switcher {
            return;
        }
        let held = m.alt || m.logo || m.ctrl;
        match self.armed {
            // First report: was the switcher's modifier still down when we appeared?
            None => {
                // Test hook: virtual keyboards (wtype) can't fake a key held
                // before we appeared, so tests can say it was.
                let held = held || std::env::var_os("DOIT_ASSUME_HELD").is_some();
                self.armed = Some(held);
                if !held {
                    // A quick tap: switch to the previous window right away.
                    self.state.accept_when_ready = true;
                    self.state.rank_if_needed();
                }
            }
            Some(true) if !held => {
                self.armed = Some(false);
                // Held without cycling, then let go: the user paused to look,
                // so stay open for searching instead of switching.
                if !self.state.switcher_moved && self.opened.elapsed() >= SEARCH_PAUSE {
                    self.state.message = Some("Type to search · ↵ to switch".into());
                    self.state.dirty = true;
                } else if self.state.view == crate::state::View::List {
                    self.state.accept_when_ready = true;
                    self.state.rank_if_needed();
                }
            }
            _ => {}
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            if &ev.surface != self.layer.wl_surface() {
                continue;
            }
            let (x, y) = ev.position;
            let rows = self.state.rows;
            match ev.kind {
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    let h = self.layout.item_at(&self.theme, self.state.grid(), rows, x, y);
                    if h != self.state.hover {
                        self.state.hover = h;
                        self.state.dirty = true;
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.state.hover = None;
                    self.state.dirty = true;
                }
                PointerEventKind::Press { button, .. } => {
                    if !self.layout.inside_panel(x, y) {
                        self.state.outcome = Some(crate::state::Outcome::Cancel);
                    } else if let Some(r) = self.layout.item_at(&self.theme, self.state.grid(), rows, x, y) {
                        // left: activate · right: actions
                        if button == 0x111 {
                            self.state.click_row(r, false);
                            if self.state.view == crate::state::View::List {
                                self.state.toggle_actions();
                            }
                        } else {
                            self.state.click_row(r, true);
                        }
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let d = if vertical.discrete != 0 {
                        vertical.discrete
                    } else if vertical.value120 != 0 {
                        vertical.value120.signum()
                    } else {
                        (vertical.absolute / 15.0) as i32
                    };
                    if d != 0 {
                        self.state.scroll_by(d);
                    }
                }
                _ => {}
            }
        }
    }
}

impl DataDeviceHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &wl_surface::WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
