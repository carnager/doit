# doit

A rofi for the next millennium: a native Wayland launcher that speaks
structured items instead of strings.

<p>
  <img src="docs/midnight.png" width="49%"> <img src="docs/daylight.png" width="49%">
</p>

- **Fast:** Rust, no toolkit, about 18 ms to the first frame.
- **Items, not lines:** titles, subtitles, icons, badges and actions as JSON.
  Plain lines still work, so it is a drop-in dmenu.
- **Live providers:** any executable can answer every keystroke, and keep the
  list up to date while it's open.
- **Looks good out of the box,** with fractional scaling, emoji, icon themes
  and four themes that follow your light or dark preference.

```sh
cargo build --release
install -Dm755 target/release/doit ~/.local/bin/doit
doit                  # apps
ls | doit             # dmenu
```

Needs a compositor with `wlr-layer-shell` (niri, sway, Hyprland, river, KDE,
COSMIC, …).

## Modes

Type a prefix to switch mode; backspace leaves it.

| prefix | mode | |
|---|---|---|
| | apps | desktop applications, ranked by use |
| `=` | calc | live results as you type |
| `:` | emoji | in your language; `:smile :tada :rocket` types several |
| `!` | power | lock, suspend, reboot, … |
| `w ` | niri windows | live window list, also as Alt+Tab |
| `m ` | melody | a remote for [Melody](https://github.com/carnager/melody) |
| `v ` | popcorn | a poster grid for [Popcorn](https://github.com/carnager/cinnamon) |
| `j ` | jot | capture and tick off [Jot](https://github.com/carnager/jot) notes |
| `b ` | bahn | train connections and departures from [Transitous](https://transitous.org) |
| `wx ` | weather | now, hourly and ten days from [Open-Meteo](https://open-meteo.com) |

<img src="docs/emoji.png" width="60%">

The modes live in [`providers/`](providers). To turn them on, start from the
example config; from a checkout, copy the providers too:

```sh
cp config.example.toml ~/.config/doit/config.toml
mkdir -p ~/.config/doit/providers && cp providers/* ~/.config/doit/providers/
```

On Arch, `doit-git` from [repo.mooned.de](https://repo.mooned.de) brings the
providers along (in `/usr/lib/doit/providers`) and the example config at
`/usr/share/doc/doit-git/config.example.toml`.

**[How it works →](docs/how-it-works.md)** Keys, flags, the item format, the
provider protocol, the bundled modes and theming.

## License

MIT, see [LICENSE](LICENSE).
