# How doit works

## Why

rofi/dmenu speak *strings*. Everything is a line, metadata gets smuggled
through delimiters, and scripts re-parse their own output to find out what you
picked. doit speaks **items**:

```json
{"title":"Firefox","subtitle":"Browse the web","icon":"firefox",
 "keywords":["browser"],"exec":"firefox","actions":[{"title":"Private window","exec":"firefox --private-window"}],
 "badge":"app","data":{"anything":"you want back"}}
```

Plain lines still work, so it's a drop-in dmenu.

## Highlights

- **Fast.** Rust, no toolkit. wlr-layer-shell + shm, tiny-skia, cosmic-text.
  Fonts and the icon theme load on background threads while the
  Wayland connection comes up. The font list is cached on disk and pruned to
  one face per fallback family, so fonts are ready in ~3 ms instead of ~30, visible icons rasterize in parallel, providers
  stream results in. Warm redraws take about 1 ms.
- **Crisp.** Fractional scaling (`wp_fractional_scale_v1` + viewporter), full
  text shaping with font fallback, emoji, RTL. SVG and PNG icons from your
  freedesktop icon theme, with tidy letter tiles when an icon is missing.
- **Smart ranking.** [nucleo](https://github.com/helix-editor/nucleo) fuzzy
  matching (from Helix) across title, keywords and subtitle, weighted, plus
  frecency.
- **Live providers.** Any executable can be a provider. It gets every
  keystroke as JSON and answers with items: calculators, window switchers,
  web search, clipboard history, password stores…
- **Prefix routing.** Type `=` for calc or `!` for the power menu. The prefix
  turns into a pill; backspace on the pill leaves the mode.
- **Actions.** <kbd>Tab</kbd> or <kbd>→</kbd> opens secondary actions
  (e.g. *Close window*, desktop-file actions like *New private window*).
- **Themeable.** Layered TOML themes. Ships with `midnight` (default),
  `daylight`, `nord` and `gruvbox`. Override any single key inline.

## Install

```sh
cargo build --release
install -Dm755 target/release/doit ~/.local/bin/doit
```

Needs a compositor with `wlr-layer-shell` (niri, sway, Hyprland, river,
labwc, wayfire, KDE, COSMIC, …).

niri:

```kdl
binds {
    Mod+D { spawn "doit"; }
}
```

## Usage

```sh
doit                         # applications (default)
doit -m apps,run             # apps + $PATH binaries (type ">" for run)
doit -m windows              # a provider from config.toml
ls | doit -p files           # dmenu mode (auto when stdin is a pipe)
doit -f json < items.jsonl   # get the full item back as JSON
```

| flag | |
|---|---|
| `-m, --modes a,b` | `apps`, `run`, or provider names |
| `-d, --dmenu` | force reading items from stdin |
| `-p, --prompt` | label pill in the input |
| `--placeholder` | placeholder text |
| `-q, --query` | initial query |
| `-t, --theme` | theme name or path to `.toml` |
| `-f, --format` | `plain` (title), `json`, `id`, `index` |
| `-P, --provider CMD` | run a provider just for this invocation, no config needed |
| `--no-exec` | print the selection even if it has `exec` |
| `--no-custom` | disallow returning free-form input |
| `--history NAME` | enable frecency for a dmenu list |
| `-l, --lines` / `-w, --width` | override theme size (`-l 0`: just the input) |
| `-M, --multi` | mark items with <kbd>Ctrl</kbd>+<kbd>Space</kbd>; Enter returns all marked |
| `--password` | masked input; Enter returns what was typed |
| `-a, --auto-select` | accept as soon as one item is left |
| `-s, --select TITLE` | preselect an item |
| `--message TEXT` | a line in the footer |
| `--switcher [prev]` | Alt+Tab style: Tab cycles, releasing the held modifier accepts, the second item starts selected; launching it again while open moves the selection instead of closing |
| `--list-themes`, `--dump-theme` | |

Exit status: `0` selected, `1` cancelled, `2` error, `10`–`18` when accepted
with <kbd>Alt</kbd>+<kbd>1</kbd>…<kbd>9</kbd> (the selection is printed, never
run, so scripts can branch on the key).

Starting the launcher while one is open closes it, so the same keybind opens and
dismisses it. dmenu calls from scripts are exempt.

### Keys

| key | |
|---|---|
| <kbd>↵</kbd> | open / select |
| <kbd>Ctrl</kbd>+<kbd>↵</kbd> | return the typed text as-is |
| <kbd>Tab</kbd>, <kbd>→</kbd> at end of input | actions (Tab moves down if there are none) |
| <kbd>←</kbd> | back: out of the actions, or up a drill-down (an artist, a show) when nothing was typed since or the cursor is at the start; in a grid, from the first column |
| <kbd>↑</kbd> <kbd>↓</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd>/<kbd>P</kbd>/<kbd>J</kbd>/<kbd>K</kbd> | move |
| <kbd>PgUp</kbd> <kbd>PgDn</kbd>, <kbd>Ctrl</kbd>+<kbd>Home</kbd>/<kbd>End</kbd> | jump |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> / <kbd>Ctrl</kbd>+<kbd>⌫</kbd>, <kbd>Ctrl</kbd>+<kbd>U</kbd> | delete word / to start |
| <kbd>Ctrl</kbd>+<kbd>A</kbd>/<kbd>E</kbd>/<kbd>B</kbd>/<kbd>F</kbd>, <kbd>Alt</kbd>+<kbd>B</kbd>/<kbd>F</kbd> | emacs-style cursor |
| <kbd>Ctrl</kbd>+<kbd>V</kbd>, <kbd>Shift</kbd>+<kbd>Insert</kbd> | paste |
| <kbd>Ctrl</kbd>+<kbd>Space</kbd> | mark (with `-M`) |
| <kbd>Alt</kbd>+<kbd>1</kbd>…<kbd>9</kbd> | accept with exit status 10…18 |
| arrows in a grid | move between cells |
| <kbd>Esc</kbd> | back / close |
| mouse | hover, click to open, right-click for actions, wheel to scroll, click outside to close |

## Item format

Every field except `title` is optional.

| field | type | |
|---|---|---|
| `title` | string | main text, fuzzy matched |
| `subtitle` | string | second line, matched with lower weight |
| `icon` | string | icon-theme name or absolute path (svg/png) |
| `keywords` | [string] | hidden match terms |
| `exec` | string | shell command run on activation; without it the item is printed |
| `terminal` | bool | run `exec` in your terminal |
| `actions` | [{title, exec?, icon?, id?}] | secondary actions (Tab) |
| `badge` | string | small label on the right |
| `id` | string | stable identity for frecency and `-f id` |
| `sticky` | bool | never filtered out |
| `bias` | number | added to the ranking score |
| `data` | any | opaque, echoed back with `-f json` |
| `verb` | string | label for Enter, e.g. `Play` |
| `progress` | 0–1 | a thin progress bar on the icon |
| `keep_open` | bool | stay open after activating (remote-control items); also on actions |
| `set_query` | string | activating replaces the query instead (drill down); the provider's prefix is kept |

## Providers

A provider is any command. doit starts it once and keeps it running.

**doit → provider** (stdin), on every query change, coalesced:

```json
{"query":"2+2"}
```

**provider → doit** (stdout), one JSON value per line:

| line | effect |
|---|---|
| `{"title":…}` | append an item |
| `{"items":[…]}` | replace all of this provider's items |
| `{"clear":true}` | remove all of this provider's items |
| `{"placeholder":"…"}`, `{"prompt":"…"}` | shown while this provider is routed |
| `{"activations":true}` | send activations of my items to me instead of running/printing them |
| `{"set_query":"…"}` | navigate (only while this provider is the active one) |
| `{"scope":{"match":"…","label":"…"}}` | queries starting with `match` are a drilled-in view: the text hides behind a pill labelled `label`, doit matches the rest, backspace on the pill leaves it; `null` ends it |
| `{"layout":{"columns":N,"aspect":1.5}}` | show my items as a grid: N columns or `"auto"`, icons `aspect` times as tall as wide (posters: 1.5); `{"layout":null}` for a list |
| `{"live":true}` | I filter my items myself |
| `{"message":"…"}` | a line in the footer while I'm active; `null` clears it |
| anything else | a plain item with that title |

With `activations`, choosing an item sends the provider
`{"activate":{"id":"…","action":"…"|null},"query":"…"}`. Then doit either stays
open (`keep_open`), so the provider can re-send fresh items, or closes. On close
doit shuts the provider's stdin and gives it up to 2 s to finish, so a provider
can do the work over a connection it already holds, with no shell command per
item.

Static providers can just print items and exit. Live providers set
`live = true`, which makes doit show their items unfiltered so the provider
does the matching.

Examples in [`providers/`](../providers): `calc` (live), `niri-windows` (live, Alt+Tab), `power`,
`melody`, `popcorn` and `jot`.

### niri-windows

A window switcher for niri. It follows niri's IPC event stream, so the list
updates live while open, and acts over the socket without spawning `niri msg`.

- **Order:** windows in most-recently-used order, with app names and icons
  resolved from desktop entries, plus workspace and output, floating and
  urgent.
- **Alt+Tab:** `niri-windows --switcher`. Hold Alt and tap Tab to cycle;
  release Alt to switch. A quick tap jumps straight back to the previous
  window. Hold without cycling for a moment, then let go: the switcher stays
  open so you can type to filter, and Enter switches. Bind `--switcher prev`
  to Alt+Shift+Tab.
- **Enter** focuses. **Tab** (or → in switcher mode) offers *Close* (stays
  open), *Bring to this workspace*, *Float/Tile*, *Fullscreen*, and *Move to*
  any other workspace or output.

```kdl
binds {
    Alt+Tab       { spawn "~/.config/doit/providers/niri-windows" "--switcher"; }
    Alt+Shift+Tab { spawn "~/.config/doit/providers/niri-windows" "--switcher" "prev"; }
}
```

### popcorn

Movies and shows from a [Cinnamon/Popcorn](https://github.com/carnager/cinnamon)
server, as a poster grid. Prefix `v `, or `popcorn --launch` for a wide window.

- **Continue watching** comes first, with progress bars, then shows with new
  episodes, your watchlist and recent additions, then the whole library.
  Typing searches all of it locally (titles, original titles, year, genre,
  director).
- **Enter** resumes a movie or episode. On a show, it opens the episodes, with
  *Up next* pinned on top.
- **Tab** offers *Play from start*, *Play trailer*, *Play on* each registered
  TV, *Mark watched/unwatched* and the watchlist.
- **Playback** runs in mpv and reports progress to the server the way
  `popcorn-mpv` does, so resume points and watched state match the TV and phone
  apps.

Credentials live in `~/.config/doit/popcorn.toml` (`server`, `username`,
`password`; keep it `chmod 600`). The library is cached, so the grid shows up
instantly and refreshes in the background.

### melody

A front end for [Melody](https://github.com/carnager/melody) that talks to
`melodyd` over its own protocol. Prefix `m `:

- **empty query:** what's playing, with its cover. Enter pauses or resumes;
  Tab gives next, previous, favourite and stop. Below it are next/previous,
  volume, the speaker in use, and the newest albums. Controls keep doit open,
  like a remote.
- **words:** matching controls, dynamic playlists, saved playlists and
  speakers, then albums, artists and tracks, live, with covers. An exact
  artist match comes first.
- **Enter** plays an album, or a track's album from that track. **Tab** offers
  *Add to queue*, *Add to Up Next* and *Rate…*.
- **an artist:** Enter shows their albums (`artist:NAME`) with *Shuffle NAME*
  on top. `speakers` switches where it plays. `vol 40` sets the volume.
- **Ratings:** *Rate album…* / *Rate track…* (Tab) open a star picker, with
  half stars, and return you where you were. `rate 4.5` / `rate album 4`
  rate what's playing; `rate 0` clears.

It uses the same engine as `melody-cli`: `$MELODY_SERVER` and
`$MELODY_PASSWORD_FILE`, or `--server`, `--local` and `--password-file`. When
doit is launched from the compositor, those variables have to be in the
compositor's environment.

### jot

Notes and todos from a [Jot](https://github.com/carnager/jot) server. Prefix
`j `, or `jot --launch`:

- **type to capture:** the first row adds what you typed, read the way Jot
  reads it: `Dentist tomorrow 10am #health`, `Gym every mon and thu 7am`,
  `Zahnarzt morgen um 10`, `Shopping: milk, eggs, bread`, a trailing ` !` pins.
  What it found among your notes is listed below. *Add it word for word* (Tab)
  skips the parsing; *Add and keep going* stays open.
- **empty query:** your day. Overdue, today and the next week, then pinned
  notes and the rest. A date alone (`tomorrow`, `fri`) shows that day, and
  `#label` shows a label, completing as you type.
- **Enter** completes a task (a repeating one moves to its next date, as Jot
  does) and stays open for the next one. A checklist opens in place: Enter
  ticks, typing adds a check, ← goes back. A note opens in Jot.
- **Tab:** snooze an hour (once it's due), tomorrow, next week, *Change the
  date…* (type it the same way: `fri 18:00`, `every mon`), pin, archive,
  trash, copy, open in Jot. The last change can be undone from the top row.

`~/.config/doit/jot.toml` holds `server = "https://…"` (and, optionally,
`username`). The first time, it asks you to sign in and keeps only the token.
Everything is cached (mode 600) under `~/.cache/doit/jot`, so it opens
instantly and captures offline. Changes reach the server as soon as it
answers, and changes made elsewhere show up while it's open.

## Configuration

`~/.config/doit/config.toml`. See [`config.example.toml`](../config.example.toml).

```toml
theme = "midnight"
modes = ["apps"]

[[provider]]
name = "calc"
exec = "~/.config/doit/providers/calc"
prefix = "="
live = true
```

### Themes

The default, `theme = "auto"`, follows the desktop's light/dark preference
(`theme_light` / `theme_dark`, `daylight` and `midnight` unless set).
Built-ins can be extended or replaced. Drop `~/.config/doit/themes/NAME.toml`
and set `theme = "NAME"`, or tweak a few keys inline:

```toml
[theme]
name = "nord"          # base theme
radius = 24
position = "top"       # or "center"
colors.accent = "#ff8fab"
```

`doit --dump-theme` prints every key with its current value.

## Status

Young but usable. An idea for later: dynamic window height for short lists.
