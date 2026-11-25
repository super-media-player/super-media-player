# Making skins

Super Media Player has two kinds of skin:

1. **Colour skins**: a small `.skin` text file that recolours one of the
   views. No programming and no rebuild: drop the file in the skins folder
   and choose *View › Reload skins*.
2. **New layouts**: a new view with its own shape and controls, written in
   [Slint](https://slint.dev) like the built-in ones. It needs a rebuild.

The built-in views (layouts) are *Media player*, *Compact*, *Classic deck*
and *Silver deck*.

## 1. Colour skins

### Where they go

| | |
|---|---|
| Linux | `~/.config/super-media-player/skins/` |
| Windows | `%APPDATA%\super-media-player\skins\` |

*View › Open skins folder* (or *Settings › Your own skins › Open folder*)
creates the folder and opens it. It also writes:

- `README.txt`, a short version of this section;
- `examples/`: the built-in skins, and an `all-colours-<layout>.skin` for
  each layout that lists **every colour name with its default**. The
  quickest way to start is to copy one of those up a folder, rename it,
  and change it.

### The format

```ini
# Lines starting with # are comments.
name   = Silver deck – Night shift   # shown in the menus
layout = silver                      # player, compact, classic or silver
author = Your name                   # optional

case-1 = #3a3f47                     # a colour of the layout
led    = #39e0ff
theme.accent = #39e0ff               # a colour of another palette
```

- Colours are `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (the last pair is
  the opacity) or `transparent`.
- Anything you leave out keeps the layout's own colour, so a skin can be
  two lines long.
- Names ending in `-1`, `-2`, `-3`… are the stops of one gradient, from top
  (or centre) to bottom (or edge). Skins only set colours; the gradients'
  shapes belong to the layout.
- The file name (without `.skin`) is the skin's id, which the settings
  remember. A user skin with a built-in skin's id replaces it.
- A skin with a mistake still loads. The bad lines are skipped, and
  *Reload skins* says what was skipped (the log lists every one).

### Palettes

Each layout takes its colours from one palette. A skin's unprefixed names
belong to its layout's palette. `theme.`, `classic.` or `silver.` name a
palette explicitly.

| Layout | Palette | Defined in |
|---|---|---|
| `player`, `compact` | `theme` | `apps/desktop/ui/theme.slint` (`global Theme`) |
| `classic` | `classic` | `apps/desktop/ui/classic.slint` (`global Classic`) |
| `silver` | `silver` | `apps/desktop/ui/silver.slint` (`global Silver`) |

The `theme` palette also colours the menus, popups, settings, the pulse
line and the playlist text in every layout. A silver skin can set
`theme.accent` to match its lights, for example.

Every `in-out property <color>` (or `<string>`, for fonts) in those
globals is a name a skin can set. The comments there say what each one
colours.

### Built-in skins

They live in `apps/desktop/skins/` and are compiled into the player
(`BUILT_IN` in `apps/desktop/src/skins.rs`):

| File | Layout |
|---|---|
| `aurora.skin` | Media player |
| `ember.skin` | Compact |
| `amber.skin`, `ice.skin` | Classic deck |
| `midnight.skin`, `rose-gold.skin` | Silver deck |

To add one: put the file there, add a line to `BUILT_IN`, and add it to
the shots in `apps/desktop/examples/render-views.rs` to look at it:

```sh
cargo run -p super-media-player --example render-views -- /tmp/views
# then open /tmp/views/skin-<id>.png
```

`cargo test` checks that every built-in skin loads without a skipped line.

## 2. New layouts

A layout is a Slint component that is the whole window: no title bar (the media player draws its own, `ui/titlebar.slint`, in the theme's colours), its
own menu button, window buttons and drag area. `compact.slint` is the
simplest one to copy; `silver.slint` shows a free-form shape with a hole
for the video.

1. **The component.** Create `apps/desktop/ui/<name>.slint` and export a
   component that takes the same properties and callbacks as `CompactView`
   (playback state, playlist, `show-menu`, `minimize`, `close`, …). It also
   reports where mpv should draw video or a visualizer through the
   `vis-x`, `vis-y`, `vis-width` and `vis-height` out properties. Anything
   drawn over that area must leave a hole there (see `Holed` in
   `skin-parts.slint`), since mpv draws underneath the UI.
2. **Its palette.** Put its colours in an `export global <Name> { … }` in
   the same file, as `in-out property <color>`. Build gradients in Slint
   from numbered stop colours (`body-1`, `body-2`, …) so skins only deal in
   colours. Use those properties, not literal colours, in the component.
3. **Show it in `app.slint`.** Import the component and the global, add
   the global to the `export { … }` line, give the layout the next view
   index, and add it next to `silver-view`: an instance with
   `visible: root.view == N`, its entries in `video-x`/`-y`/`-width`/
   `-height`, and its `min-width`/`min-height`. Add its name to the
   default `skin-names` and `skin-views`.
4. **Rust side.**
   - `settings.rs`: add its id to `VIEW_MODES` (same order as the view index).
   - `main.rs`: add its default size to `skin_size`.
   - `skins.rs`: add it to `PLAIN`, add the palette to the `Palette` enum
     (`ALL`, `name`, `of_layout`, `props`), and add a `<name>_props`
     table like `silver_props`. Add the palette to the test
     `tables_match_the_slint_globals` too; it fails until the table lists
     every colour of the global.
5. **Look at it** by adding shots to `render-views.rs`.

The menus come from the skin list, so a new layout and its colour skins
show up in *View* and in *Settings › Skin* without further work.
