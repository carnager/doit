//! UI-independent launcher state: query editing, ranking, selection.

use crate::history::History;
use crate::item::{Action, Item};
use crate::matcher::{Hit, Ranker, route, unscope};
use crate::providers::{Source, SourceMsg, ToProvider};

pub enum Outcome {
    /// Activate an item (optionally one of its secondary actions).
    Item { idx: usize, action: Option<usize> },
    /// Return the raw query (Ctrl+Enter, or Enter with no matches).
    Custom(String),
    /// The provider took care of the activation; nothing to print or run.
    Handled,
    /// Several marked items (multi-select).
    Multi(Vec<usize>),
    Cancel,
}

#[derive(PartialEq)]
pub enum View {
    List,
    Actions { item: usize, selected: usize },
}

pub struct State {
    pub items: Vec<Item>,
    pub sources: Vec<Source>,
    pub hits: Vec<Hit>,
    pub query: String,
    pub cursor: usize,
    pub selected: usize,
    pub scroll: usize,
    pub rows: usize,
    /// Columns of the current layout: 1 for a list, more for a grid.
    pub cols: usize,
    pub view: View,
    pub prompt: Option<String>,
    pub placeholder: String,
    pub ranker: Ranker,
    pub history: History,
    pub outcome: Option<Outcome>,
    pub dirty: bool,
    /// Filtering is deferred to once per event-loop iteration.
    needs_rank: bool,
    query_changed: bool,
    last_routed: Option<String>,
    pub active: Vec<bool>,
    pub hover: Option<usize>,
    pub allow_custom: bool,
    /// Multi-select: Ctrl+Space marks items, Enter returns all of them.
    pub multi: bool,
    pub marked: Vec<usize>,
    /// Mask the input (passwords).
    pub password: bool,
    /// Exit status for custom keys (Alt+1…9 → 10…18).
    pub exit_code: Option<u8>,
    /// Accept as soon as exactly one item is left (once all sources are done).
    pub auto_select: bool,
    /// Select the first item with this title once it shows up.
    pub preselect: Option<String>,
    pub message: Option<String>,
    /// Alt+Tab style: Tab cycles, releasing the held modifier accepts,
    /// and the second item (the previous window) starts selected.
    pub switcher: bool,
    initial_done: bool,
    /// Accept as soon as there is something to accept (modifier released early).
    pub accept_when_ready: bool,
    /// Switcher: the selection was moved (Tab, arrows, the keybind again).
    pub switcher_moved: bool,
    /// Drill-downs, for going back: where each one came from.
    trail: Vec<Crumb>,
    /// After going back, select this item (by key) once it shows up again.
    restore: Option<String>,
}

/// One step of navigation: the query left, the item selected there, and the
/// query arrived at (while it's unchanged, nothing was typed since).
struct Crumb {
    from: String,
    selected: Option<String>,
    to: String,
}

impl State {
    pub fn new(sources: Vec<Source>, history: History, rows: usize) -> State {
        let active = sources.iter().map(|s| s.prefix.is_none()).collect();
        State {
            items: vec![],
            sources,
            hits: vec![],
            query: String::new(),
            cursor: 0,
            selected: 0,
            scroll: 0,
            rows,
            cols: 1,
            view: View::List,
            prompt: None,
            placeholder: "Search…".into(),
            ranker: Ranker::new(),
            history,
            outcome: None,
            dirty: true,
            needs_rank: true,
            query_changed: false,
            last_routed: None,
            active,
            hover: None,
            allow_custom: true,
            multi: false,
            marked: vec![],
            password: false,
            exit_code: None,
            auto_select: false,
            preselect: None,
            message: None,
            switcher: false,
            initial_done: false,
            accept_when_ready: false,
            switcher_moved: false,
            trail: vec![],
            restore: None,
        }
    }

    pub fn on_source(&mut self, msg: SourceMsg) {
        match msg {
            SourceMsg::Append(mut v) => self.items.append(&mut v),
            SourceMsg::Replace(src, v) => {
                // Marks are item indices; keep them valid across the shuffle.
                let marked: Vec<(usize, String)> =
                    self.marked.iter().map(|&m| (self.items[m].source, self.items[m].key().to_string())).collect();
                self.items.retain(|i| i.source != src);
                self.items.extend(v);
                self.marked = marked
                    .iter()
                    .filter_map(|(s, k)| self.items.iter().position(|i| i.source == *s && i.key() == k))
                    .collect();
            }
            SourceMsg::SetQuery(i, q) => {
                if self.sole_source().is_some_and(|s| std::ptr::eq(s, &self.sources[i])) {
                    self.navigate(&q);
                }
            }
            SourceMsg::Scope(i, sc) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.scope = sc
                }
            }
            SourceMsg::Grid(i, g) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.grid = g
                }
            }
            SourceMsg::Live(i, l) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.live = l
                }
            }
            SourceMsg::Message(i, m) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.message = m
                }
            }
            SourceMsg::Done(i) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.done = true
                }
            }
            SourceMsg::Activations(i) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.activations = true
                }
            }
            SourceMsg::Prompt(i, p) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.prompt = Some(p)
                }
            }
            SourceMsg::Placeholder(i, p) => {
                if let Some(s) = self.sources.get_mut(i) {
                    s.placeholder = Some(p)
                }
            }
        }
        self.needs_rank = true;
        self.dirty = true;
    }

    /// The source whose prefix the current query uses, if any.
    pub fn routed_source(&self) -> Option<&Source> {
        let n = self.active.iter().filter(|a| **a).count();
        if n == 1 {
            let i = self.active.iter().position(|a| *a)?;
            let s = &self.sources[i];
            if s.prefix.is_some() {
                return Some(s);
            }
        }
        None
    }

    /// The single source currently receiving the query, if exactly one is active.
    pub fn sole_source(&self) -> Option<&Source> {
        let mut it = self.active.iter().enumerate().filter(|(_, a)| **a);
        let (i, _) = it.next()?;
        it.next().is_none().then(|| &self.sources[i])
    }

    pub fn placeholder(&self) -> &str {
        self.sole_source().and_then(|s| s.placeholder.as_deref()).unwrap_or(&self.placeholder)
    }

    pub fn prompt(&self) -> Option<&str> {
        self.sole_source().and_then(|s| s.prompt.as_deref()).or(self.prompt.as_deref())
    }

    /// Bytes of `query` taken by the routing prefix alone.
    fn route_len(&self) -> usize {
        self.query.len() - route(&self.sources, &self.query).1.len()
    }

    /// Bytes of `query` hidden behind the pill: routing prefix plus scope.
    pub fn prefix_len(&self) -> usize {
        self.query.len() - self.effective_query().len()
    }

    /// The query as providers see it: routing prefix stripped, scope kept.
    pub fn routed_query(&self) -> &str {
        route(&self.sources, &self.query).1
    }

    /// What is matched and shown: prefix and scope stripped.
    pub fn effective_query(&self) -> &str {
        let (active, q) = route(&self.sources, &self.query);
        unscope(&self.sources, &active, q)
    }

    /// The pill label of the current scope, if a drilled-in view is active.
    pub fn scope_label(&self) -> Option<&str> {
        let s = self.sole_source()?;
        let (m, label) = s.scope.as_ref()?;
        self.routed_query().starts_with(m.as_str()).then_some(label.as_str())
    }

    /// Grid (columns, icon aspect) for the current view, if the sole source asked for one.
    pub fn grid(&self) -> Option<(usize, f32)> {
        if self.view != View::List {
            return None;
        }
        self.sole_source().and_then(|s| s.grid)
    }

    pub fn rank_if_needed(&mut self) {
        if !self.needs_rank {
            if self.accept_when_ready && !self.hits.is_empty() && self.outcome.is_none() {
                self.accept_when_ready = false;
                self.accept();
            }
            return;
        }
        self.needs_rank = false;
        let (active, q) = route(&self.sources, &self.query);
        let q = q.to_string();
        // Forward query to sources that listen (scripts).
        if self.last_routed.as_deref() != Some(q.as_str()) || active != self.active {
            for (i, s) in self.sources.iter().enumerate() {
                if active[i]
                    && let Some(tx) = &s.query_tx
                {
                    let _ = tx.send(ToProvider::Query(q.clone()));
                }
            }
            self.last_routed = Some(q.clone());
        }
        self.active = active;
        let matched = unscope(&self.sources, &self.active, &q).to_string();
        self.ranker.set_query(&matched);
        let prev = self.hits.get(self.selected).map(|h| h.idx);
        self.hits = self.ranker.rank(&self.items, &self.sources, &self.active, &self.history);
        // Keep the selection on the same item while results stream in.
        self.selected = prev
            .and_then(|p| self.hits.iter().take(self.page() * 4).position(|h| h.idx == p))
            .filter(|_| !self.query_changed)
            .unwrap_or(0);
        self.query_changed = false;
        if self.switcher && !self.initial_done && !self.hits.is_empty() {
            self.initial_done = true;
            self.selected = usize::from(self.hits.len() > 1);
        }
        if self.accept_when_ready && !self.hits.is_empty() && self.outcome.is_none() {
            self.accept_when_ready = false;
            self.accept();
        }
        if let Some(want) = &self.restore
            && let Some(pos) = self.hits.iter().position(|h| self.items[h.idx].key() == want)
        {
            self.selected = pos;
            self.restore = None;
        }
        if let Some(want) = &self.preselect
            && let Some(pos) = self.hits.iter().position(|h| &self.items[h.idx].title == want)
        {
            self.selected = pos;
            self.preselect = None;
        }
        self.clamp_scroll();
        self.dirty = true;
        if self.auto_select && self.hits.len() == 1 && self.sources.iter().all(|s| s.done) && self.outcome.is_none() {
            self.accept();
        }
    }

    /// The footer message: the active provider's, else the one given on the command line.
    pub fn message(&self) -> Option<&str> {
        self.sole_source().and_then(|s| s.message.as_deref()).or(self.message.as_deref())
    }

    /// Move the selection in the switcher, remembering that the user cycled.
    pub fn switcher_step(&mut self, delta: i32) {
        self.switcher_moved = true;
        self.move_sel(delta, true);
    }

    /// Ctrl+Space: mark or unmark the selected item, then move on.
    pub fn toggle_mark(&mut self) {
        if !self.multi || self.view != View::List {
            return;
        }
        if let Some(h) = self.hits.get(self.selected) {
            match self.marked.iter().position(|&m| m == h.idx) {
                Some(p) => {
                    self.marked.remove(p);
                }
                None => self.marked.push(h.idx),
            }
            self.move_sel(1, false);
            self.dirty = true;
        }
    }

    /// Alt+1…9: accept with exit status 10…18 (scripts branch on it).
    pub fn accept_custom_key(&mut self, n: u8) {
        self.exit_code = Some(9 + n);
        self.rank_if_needed();
        self.outcome = match self.hits.get(self.selected) {
            Some(h) if self.view == View::List => Some(Outcome::Item { idx: h.idx, action: None }),
            _ => Some(Outcome::Custom(self.effective_query().to_string())),
        };
    }

    /// Items visible at once.
    pub fn page(&self) -> usize {
        self.rows * self.cols
    }

    /// Scrolling moves by whole rows of `cols` items.
    fn clamp_scroll(&mut self) {
        let n = self.len();
        if n == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        let c = self.cols.max(1);
        self.selected = self.selected.min(n - 1);
        let (sel_row, mut top) = (self.selected / c, self.scroll / c);
        if sel_row < top {
            top = sel_row;
        } else if sel_row >= top + self.rows {
            top = sel_row + 1 - self.rows;
        }
        top = top.min(n.div_ceil(c).saturating_sub(self.rows));
        self.scroll = top * c;
    }

    /// Called by the renderer when the layout's geometry changes.
    pub fn set_geometry(&mut self, rows: usize, cols: usize) {
        if (rows, cols) != (self.rows, self.cols) {
            self.rows = rows.max(1);
            self.cols = cols.max(1);
            self.clamp_scroll();
        }
    }

    pub fn len(&self) -> usize {
        match &self.view {
            View::List => self.hits.len(),
            View::Actions { item, .. } => self.actions_for(*item).len(),
        }
    }

    /// Primary activation plus the item's secondary actions.
    pub fn actions_for(&self, idx: usize) -> Vec<Action> {
        let it = &self.items[idx];
        let mut v = vec![Action {
            title: it.verb.clone().unwrap_or_else(|| {
                if it.argv.is_some() || it.exec.is_some() || self.sources[it.source].activations {
                    "Open".into()
                } else {
                    "Select".into()
                }
            }),
            icon: it.icon.clone(),
            ..Default::default()
        }];
        v.extend(it.actions.iter().cloned());
        v
    }

    fn edited(&mut self) {
        self.needs_rank = true;
        self.query_changed = true;
        self.dirty = true;
        self.scroll = 0;
        self.selected = 0;
    }

    pub fn insert(&mut self, s: &str) {
        if let View::Actions { .. } = self.view {
            self.view = View::List;
        }
        self.restore = None;
        self.query.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.edited();
    }

    fn prev_boundary(&self, i: usize) -> usize {
        self.query[..i].char_indices().next_back().map(|(p, _)| p).unwrap_or(0)
    }
    fn next_boundary(&self, i: usize) -> usize {
        self.query[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }
    fn word_left(&self, i: usize) -> usize {
        let s = &self.query[..i];
        let t = s.trim_end_matches(|c: char| !c.is_alphanumeric());
        t.trim_end_matches(|c: char| c.is_alphanumeric()).len()
    }
    fn word_right(&self, i: usize) -> usize {
        let s = &self.query[i..];
        let skip = s.len() - s.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let rest = &s[skip..];
        i + skip + (rest.len() - rest.trim_start_matches(|c: char| c.is_alphanumeric()).len())
    }

    pub fn backspace(&mut self, word: bool) {
        if let View::Actions { .. } = self.view {
            self.view = View::List;
            self.dirty = true;
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let pl = self.prefix_len();
        if pl > 0 && self.cursor <= pl {
            // Backspace on the pill: leave the scope, else the provider.
            let rl = self.route_len();
            let from = if pl > rl { rl } else { 0 };
            self.query.replace_range(from..pl, "");
            self.cursor = from;
            self.edited();
            return;
        }
        let start = if word { self.word_left(self.cursor) } else { self.prev_boundary(self.cursor) };
        let start = start.max(pl);
        self.query.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.edited();
    }

    pub fn delete(&mut self, word: bool) {
        if self.cursor >= self.query.len() {
            return;
        }
        let end = if word { self.word_right(self.cursor) } else { self.next_boundary(self.cursor) };
        self.query.replace_range(self.cursor..end, "");
        self.edited();
    }

    pub fn clear_line(&mut self) {
        let pl = self.prefix_len().min(self.cursor);
        self.query.replace_range(pl..self.cursor, "");
        self.cursor = pl;
        self.edited();
    }

    pub fn move_cursor(&mut self, dir: i32, word: bool) {
        self.cursor = match (dir < 0, word) {
            (true, false) => self.prev_boundary(self.cursor),
            (true, true) => self.word_left(self.cursor),
            (false, false) => self.next_boundary(self.cursor),
            (false, true) => self.word_right(self.cursor),
        }
        .max(self.prefix_len());
        self.dirty = true;
    }

    pub fn home(&mut self) {
        self.cursor = self.prefix_len();
        self.dirty = true;
    }
    pub fn end(&mut self) {
        self.cursor = self.query.len();
        self.dirty = true;
    }

    pub fn move_sel(&mut self, delta: i32, wrap: bool) {
        let n = self.len();
        if n == 0 {
            return;
        }
        let cur = match &self.view {
            View::List => self.selected,
            View::Actions { selected, .. } => *selected,
        } as i64;
        let mut next = cur + delta as i64;
        if wrap {
            next = next.rem_euclid(n as i64);
        } else {
            next = next.clamp(0, n as i64 - 1);
        }
        match &mut self.view {
            View::List => {
                self.selected = next as usize;
                self.clamp_scroll();
            }
            View::Actions { selected, .. } => *selected = next as usize,
        }
        self.hover = None;
        self.dirty = true;
    }

    /// Scroll by `delta` rows.
    pub fn scroll_by(&mut self, delta: i32) {
        if self.view != View::List {
            return self.move_sel(delta, false);
        }
        let c = self.cols.max(1);
        let max_row = self.hits.len().div_ceil(c).saturating_sub(self.rows);
        let row = (self.scroll / c) as i64 + delta as i64;
        self.scroll = row.clamp(0, max_row as i64) as usize * c;
        let last = (self.scroll + self.page()).min(self.hits.len()).saturating_sub(1);
        self.selected = self.selected.clamp(self.scroll, last.max(self.scroll));
        self.dirty = true;
    }

    pub fn has_actions(&self) -> bool {
        self.hits.get(self.selected).is_some_and(|h| !self.items[h.idx].actions.is_empty())
    }

    pub fn toggle_actions(&mut self) {
        match self.view {
            View::List => {
                if let Some(h) = self.hits.get(self.selected) {
                    self.view = View::Actions { item: h.idx, selected: 0 };
                }
            }
            View::Actions { .. } => self.view = View::List,
        }
        self.hover = None;
        self.dirty = true;
    }

    pub fn back(&mut self) {
        match self.view {
            View::Actions { .. } => {
                self.view = View::List;
                self.dirty = true;
            }
            View::List => self.outcome = Some(Outcome::Cancel),
        }
    }

    pub fn accept(&mut self) {
        self.rank_if_needed();
        if self.multi && !self.marked.is_empty() && self.view == View::List {
            self.outcome = Some(Outcome::Multi(self.marked.clone()));
            return;
        }
        let (idx, action) = match self.view {
            View::Actions { item, selected } => (item, if selected == 0 { None } else { Some(selected - 1) }),
            View::List => match self.hits.get(self.selected) {
                Some(h) => (h.idx, None),
                None => {
                    if self.custom_allowed() && !self.query.is_empty() {
                        self.outcome = Some(Outcome::Custom(self.effective_query().to_string()));
                    }
                    return;
                }
            },
        };
        if self.intercept(idx, action) {
            return;
        }
        self.outcome = Some(Outcome::Item { idx, action });
    }

    /// Activations doit handles without exiting: drill-down, keep-open items,
    /// and items whose provider wants its activations back. True if handled.
    fn intercept(&mut self, idx: usize, action: Option<usize>) -> bool {
        let it = &self.items[idx];
        let act = action.and_then(|a| it.actions.get(a));
        if let Some(q) = act.map_or(it.set_query.clone(), |a| a.set_query.clone()) {
            self.navigate(&q);
            return true;
        }
        let keep_open = act.map_or(it.keep_open, |a| a.keep_open);
        let src = &self.sources[it.source];
        if src.activations {
            if let Some(tx) = &src.query_tx {
                let _ = tx.send(ToProvider::Activate {
                    id: it.id.clone(),
                    action: act.and_then(|a| a.id.clone()),
                    query: self.routed_query().to_string(),
                });
            }
            if keep_open {
                self.view = View::List;
                self.dirty = true;
            } else {
                self.outcome = Some(Outcome::Handled);
            }
            return true;
        }
        if keep_open {
            if let Some(cmd) = act.map_or(it.exec.as_ref(), |a| a.exec.as_ref()) {
                crate::providers::spawn_shell(cmd);
            }
            self.view = View::List;
            self.dirty = true;
            return true;
        }
        false
    }

    /// Replace the query, keeping the routing prefix (provider-relative).
    /// Remembered for going back; arriving where an earlier step started
    /// (a provider returning home, say) shortens the trail to there.
    fn navigate(&mut self, q: &str) {
        let prefix = self.query[..self.route_len()].to_string();
        let to = prefix + q;
        if to == self.query {
            // the same view again: a refresh, not a step
        } else if let Some(i) = self.trail.iter().position(|c| c.from == to) {
            self.trail.truncate(i);
        } else {
            let selected = self.selected_item().map(|it| it.key().to_string());
            self.trail.push(Crumb { from: std::mem::take(&mut self.query), selected, to: to.clone() });
        }
        self.restore = None;
        self.query = to;
        self.cursor = self.query.len();
        self.view = View::List;
        self.edited();
    }

    fn selected_item(&self) -> Option<&Item> {
        self.hits.get(self.selected).map(|h| &self.items[h.idx])
    }

    /// Back where the last drill-down started, that item selected again.
    pub fn go_back(&mut self) -> bool {
        let Some(c) = self.trail.pop() else { return false };
        self.query = c.from;
        self.cursor = self.query.len();
        self.restore = c.selected;
        self.view = View::List;
        self.edited();
        true
    }

    /// Left goes back: out of the actions, or up a drill-down when nothing
    /// was typed since it or the cursor is at the start (in a grid: from
    /// the first column). Otherwise it moves the cursor or the selection.
    pub fn left(&mut self, word: bool) {
        if let View::Actions { .. } = self.view {
            self.view = View::List;
            self.dirty = true;
            return;
        }
        let untouched = self.trail.last().is_some_and(|c| c.to == self.query);
        if self.grid().is_some() && !word {
            if self.selected.is_multiple_of(self.cols.max(1)) && self.go_back() {
                return;
            }
            return self.move_sel(-1, false);
        }
        if (untouched || self.cursor <= self.prefix_len()) && self.go_back() {
            return;
        }
        self.move_cursor(-1, word);
    }

    /// Close provider stdins and wait (bounded) for them to finish.
    pub fn shutdown_providers(&mut self) {
        for s in &mut self.sources {
            s.query_tx = None;
        }
        for s in &mut self.sources {
            if let Some(w) = s.worker.take() {
                let _ = w.join();
            }
        }
    }

    /// Free-form input makes sense unless the provider in charge handles
    /// its own activations.
    pub fn custom_allowed(&self) -> bool {
        self.allow_custom && !self.sole_source().is_some_and(|s| s.activations)
    }

    pub fn accept_custom(&mut self) {
        if self.custom_allowed() {
            self.outcome = Some(Outcome::Custom(self.effective_query().to_string()));
        }
    }

    /// Select (and optionally activate) the visible row `row`.
    pub fn click_row(&mut self, row: usize, activate: bool) {
        match &mut self.view {
            View::List => {
                let i = self.scroll + row;
                if i < self.hits.len() {
                    self.selected = i;
                } else {
                    return;
                }
            }
            View::Actions { item, selected } => {
                if row < self.items[*item].actions.len() + 1 {
                    *selected = row;
                } else {
                    return;
                }
            }
        }
        self.dirty = true;
        if activate {
            self.accept();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Source;

    fn state() -> State {
        let mut calc = Source::new("calc");
        calc.prefix = Some("=".into());
        let mut st = State::new(vec![Source::new("apps"), calc], History::disabled(), 8);
        st.on_source(SourceMsg::Append(vec![Item::plain("Firefox"), Item::plain("Files")]));
        st
    }

    #[test]
    fn editing() {
        let mut st = state();
        st.insert("hello world");
        st.backspace(true);
        assert_eq!(st.query, "hello ");
        st.home();
        st.delete(true);
        assert_eq!(st.query, " ");
        st.insert("ä");
        st.move_cursor(-1, false);
        assert_eq!(st.cursor, 0);
    }

    #[test]
    fn prefix_routing() {
        let mut st = state();
        st.insert("=1+1");
        st.rank_if_needed();
        assert_eq!(st.prefix_len(), 1);
        assert_eq!(st.effective_query(), "1+1");
        assert!(st.hits.is_empty(), "apps hidden while calc is routed");
        st.home();
        assert_eq!(st.cursor, 1);
        st.backspace(false); // on the pill: leave the provider
        assert_eq!(st.query, "1+1");
    }

    fn done(st: &mut State) {
        for i in 0..st.sources.len() {
            st.on_source(SourceMsg::Done(i));
        }
    }

    #[test]
    fn multi_select() {
        let mut st = state();
        st.multi = true;
        st.rank_if_needed();
        st.toggle_mark();
        st.toggle_mark();
        assert_eq!(st.marked.len(), 2);
        st.move_sel(-1, false);
        st.toggle_mark(); // unmark the second
        st.accept();
        assert!(matches!(st.outcome, Some(Outcome::Multi(ref v)) if v.len() == 1));
    }

    #[test]
    fn left_goes_back() {
        let mut st = state();
        st.query = "fi".into();
        st.cursor = 2;
        st.rank_if_needed();
        st.move_sel(1, false);
        let picked = st.items[st.hits[st.selected].idx].key().to_string();
        st.navigate("deeper");
        st.rank_if_needed();
        st.left(false); // nothing typed since: back, the item selected again
        assert_eq!(st.query, "fi");
        st.rank_if_needed();
        assert_eq!(st.items[st.hits[st.selected].idx].key(), picked);
        st.left(false); // no trail left: just the cursor
        assert_eq!((st.query.as_str(), st.cursor), ("fi", 1));

        st.navigate("a");
        st.insert("b"); // typed: Left edits, until the cursor reaches the start
        st.left(false);
        assert_eq!(st.query, "ab");
        st.left(false);
        assert_eq!(st.query, "ab");
        st.left(false);
        assert_eq!(st.query, "fi");

        st.navigate("x");
        st.navigate("y");
        st.navigate("fi"); // a provider going home: the trail ends there
        assert!(!st.go_back());
    }

    #[test]
    fn custom_key_and_preselect() {
        let mut st = state();
        st.preselect = Some("Files".into());
        st.rank_if_needed();
        assert_eq!(st.items[st.hits[st.selected].idx].title, "Files");
        st.accept_custom_key(3);
        assert_eq!(st.exit_code, Some(12));
        assert!(matches!(st.outcome, Some(Outcome::Item { .. })));
    }

    #[test]
    fn auto_select_waits_for_sources() {
        let mut st = state();
        st.auto_select = true;
        st.insert("firef");
        st.rank_if_needed();
        assert!(st.outcome.is_none(), "sources still streaming");
        done(&mut st);
        st.rank_if_needed();
        assert!(matches!(st.outcome, Some(Outcome::Item { .. })));
    }

    #[test]
    fn ranking() {
        let mut st = state();
        st.insert("fil");
        st.rank_if_needed();
        assert_eq!(st.items[st.hits[0].idx].title, "Files");
    }
}
