//! The structured item model. Everything doit shows is an `Item`, whether it
//! came from a desktop file, $PATH, stdin, or a provider script.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Action {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    /// Second line; defaults to the command. `""` shows none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Stay open after running this action.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep_open: bool,
    /// Choosing this action replaces the query instead (drill down).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_query: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    /// Stable identity, used for frecency. Defaults to the title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Icon name (from the icon theme) or absolute path to an image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Extra terms that match but aren't displayed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Command to run when activated. Without it, the item is printed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    /// Run `exec` inside the configured terminal.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
    /// Secondary actions, shown with Tab.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Small label shown on the right, e.g. a shortcut or category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// Never filtered out by the query (calculator results etc).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sticky: bool,
    /// Ranking bias added to the match score.
    #[serde(skip_serializing_if = "is_zero")]
    pub bias: f32,
    /// 0–1: drawn as a thin bar on the icon (watch progress, downloads…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// Label for Enter, e.g. "Play" (shown in hints and the actions list).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    /// Stay open after activation (remote-control style items).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep_open: bool,
    /// Activation replaces the query with this instead (drill down).
    /// Relative to the provider: its prefix is kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_query: Option<String>,
    /// Opaque payload, echoed back in JSON output.
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,

    /// Internal: argv for desktop-entry launches (bypasses the shell).
    #[serde(skip)]
    pub argv: Option<Vec<String>>,
    /// Internal: index of the source that produced this item.
    #[serde(skip)]
    pub source: usize,
}

fn is_zero(f: &f32) -> bool {
    *f == 0.0
}

impl Item {
    pub fn plain(title: impl Into<String>) -> Item {
        Item { title: title.into(), ..Default::default() }
    }

    pub fn key(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.title)
    }

    /// Parse one protocol line: a JSON object is an item, anything else a plain title.
    pub fn from_line(line: &str) -> Option<Item> {
        let t = line.trim_end_matches(['\r', '\n']);
        if t.is_empty() {
            return None;
        }
        if t.trim_start().starts_with('{')
            && let Ok(item) = serde_json::from_str::<Item>(t)
        {
            return Some(item);
        }
        Some(Item::plain(t))
    }
}
