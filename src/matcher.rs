//! Fuzzy ranking over structured items (title > keywords > subtitle),
//! blended with frecency.

use crate::history::History;
use crate::item::Item;
use crate::providers::Source;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Ranker {
    matcher: Matcher,
    buf: Vec<char>,
    pattern: Pattern,
    query: String,
    min_secondary: u32,
}

#[derive(Clone, Copy)]
pub struct Hit {
    pub idx: usize,
    pub score: f32,
}

/// Which sources a query targets, and the query with any prefix stripped.
pub fn route<'q>(sources: &[Source], query: &'q str) -> (Vec<bool>, &'q str) {
    for (i, s) in sources.iter().enumerate() {
        if let Some(p) = &s.prefix
            && !p.is_empty()
            && let Some(rest) = query.strip_prefix(p.as_str())
        {
            let mut mask = vec![false; sources.len()];
            mask[i] = true;
            return (mask, rest.trim_start());
        }
    }
    (sources.iter().map(|s| s.prefix.is_none()).collect(), query)
}

/// The routed query with the sole active source's scope (a drilled-in view,
/// shown as a pill) stripped: what is actually matched.
pub fn unscope<'q>(sources: &[Source], active: &[bool], q: &'q str) -> &'q str {
    let mut on = active.iter().enumerate().filter(|(_, a)| **a);
    if let (Some((i, _)), None) = (on.next(), on.next())
        && let Some((m, _)) = &sources[i].scope
        && let Some(rest) = q.strip_prefix(m.as_str())
    {
        return rest;
    }
    q
}

impl Ranker {
    pub fn new() -> Ranker {
        let mut cfg = Config::DEFAULT;
        cfg.prefer_prefix = true;
        Ranker {
            matcher: Matcher::new(cfg),
            buf: Vec::new(),
            pattern: Pattern::default(),
            query: String::new(),
            min_secondary: 0,
        }
    }

    pub fn set_query(&mut self, q: &str) {
        if q != self.query {
            self.query = q.to_string();
            self.pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
            // ~ a contiguous run (16/char) minus slack for a gap or two.
            let n = q.chars().filter(|c| !c.is_whitespace()).count() as u32;
            self.min_secondary = (n * 16).saturating_sub(8);
        }
    }

    fn score_str(&mut self, s: &str) -> Option<u32> {
        self.pattern.score(Utf32Str::new(s, &mut self.buf), &mut self.matcher)
    }

    pub fn rank(&mut self, items: &[Item], sources: &[Source], active: &[bool], hist: &History) -> Vec<Hit> {
        let empty = self.pattern.atoms.is_empty();
        let mut hits = Vec::with_capacity(if empty { items.len() } else { items.len() / 4 });
        for (idx, it) in items.iter().enumerate() {
            if !active.get(it.source).copied().unwrap_or(false) {
                continue;
            }
            let live = sources.get(it.source).is_some_and(|s| s.live);
            let base = if empty || live || it.sticky {
                if live || it.sticky { 100_000.0 } else { 0.0 }
            } else {
                let t = self.score_str(&it.title).map(|s| s as f32);
                let mut best = t.unwrap_or(-1.0);
                // Secondary fields only count when the match is tight, so a
                // scattered hit in a long description doesn't add noise.
                let min = self.min_secondary;
                for k in &it.keywords {
                    if let Some(s) = self.score_str(k).filter(|&s| s >= min) {
                        best = best.max(s as f32 * 0.7);
                    }
                }
                if let Some(sub) = &it.subtitle
                    && let Some(s) = self.score_str(sub).filter(|&s| s >= min)
                {
                    best = best.max(s as f32 * 0.45);
                }
                if best < 0.0 {
                    continue;
                }
                best
            };
            let score = base + hist.score(it.key()) + it.bias;
            hits.push(Hit { idx, score });
        }
        // Stable: equal scores keep input order (dmenu semantics).
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits
    }

    /// Char indices in `s` matched by the current pattern, for highlighting.
    pub fn indices(&mut self, s: &str) -> Vec<u32> {
        let mut out = vec![];
        if self.pattern.atoms.is_empty() {
            return out;
        }
        self.pattern.indices(Utf32Str::new(s, &mut self.buf), &mut self.matcher, &mut out);
        out.sort_unstable();
        out.dedup();
        out
    }
}

