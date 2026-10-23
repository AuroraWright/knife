# knife

A Rust reimplementation of the engine of Tsukihime -A piece of blue glass moon- for Nintendo Switch. It plays
the game from the Switch release's data and aims to behave exactly like the original: it is heavily based on a
decompilation of the original Switch executable, and its behaviour is checked against the Switch game running in
an emulator.

![The title screen of the overseas release in knife](screenshot.png)

Both releases are supported: the Japanese one and the overseas one (English, Simplified and Traditional
Chinese besides Japanese).

knife is inspired by [shin](https://github.com/DCNick3/shin), DCNick3's reimplementation of the engine of
the Switch visual novels by Entergram, and shares some of its code.

## Game data

knife needs the game data from your own copy of the game:

1. Extract the romfs of the Switch release.
2. Add the Switch's shared system font, which the menus and help lines use. Dump
   `nintendo_udsg-r_std_003.bfttf` from the firmware's FontStandard archive (system title 0100000000000811)
   and copy it into the extracted romfs folder, for example:

   ```sh
   hactoolnet -k prod.keys --romfsdir fontstandard <FontStandard>.nca
   cp fontstandard/nintendo_udsg-r_std_003.bfttf <romfs folder>/
   ```

By default knife looks for this folder as `assets` next to the executable (inside a macOS app bundle, also
`Contents/Resources/assets`).

## Building and running

```sh
cargo build --release -p knife
./target/release/knife -a <romfs folder>
```

Movies are decoded by the platform: VideoToolbox on macOS, Media Foundation on Windows (N editions need the
Media Feature Pack) and GStreamer elsewhere (with an H.264 plugin such as gst-libav, plus the GStreamer
development packages to build). An `ffmpeg` executable on the `PATH` is used as a fallback.

### Options

| Option | |
|---|---|
| `-a`, `--assets-dir DIR` | the game data folder |
| `-l`, `--language ja\|en\|zc\|zt` | the language, as the console language and the config's language buttons pick it (overseas release only; by default the config's setting) |
| `-s`, `--save-file PATH` | the save file (by default `knife/savedata000` in the system's app data folder, `knife/overseas/savedata000` for the overseas release) |
| `-t`, `--text-websocket HOST:PORT` | serve the displayed text over WebSocket for text hookers (e.g. `127.0.0.1:6677`) |
| `--skip-intro` | leave out the TYPE-MOON logo movie and the notice before the title |
| `--skip-movies` | let every story movie be skipped, even the ones the Switch plays through the first time |

With `--text-websocket`, every client receives each message as plain text and each choice's options one per
line; a client that connects late first gets the most recent text.

### knife.ini

A `knife.ini` file next to the executable (or in `Contents/Resources` inside a macOS app bundle) can set any of
the options above, one per line, using the long option name without `--`:

```ini
assets-dir = assets
language = en
text-websocket = 127.0.0.1:6677
skip-intro = true
```

Lines starting with `;` or `#` are comments, an empty value leaves an option unset, on / off options take
true or false, and relative paths are resolved from the folder containing `knife.ini`. Options given on the
command line take precedence. See [`knife.ini.example`](knife.ini.example).

## Controls

The keyboard stands in for the Switch controller, and each button does what it does in the Switch game on that
screen (the game's own map is in Config → 操作説明 / controls). The left mouse button is the touch screen; the
mouse wheel or trackpad tilts the left stick.

Game controllers work too: every controller acts as the Switch controller, button for button (A / B / X / Y as
the controller labels them, Plus / Minus as Start / Back).

| Key | Switch |
|---|---|
| Arrow keys | D-pad |
| W / A / S / D | left stick |
| Enter | A (confirm, advance text) |
| Escape | B (cancel, back) |
| B | X |
| M | Y |
| H / J | L / R |
| T / Y | ZL / ZR |
| K / L | right stick up / down |
| O | R3 |
| U / I | Minus / Plus |
| F | toggle fullscreen |
| Left Ctrl (hold) | skip text while held |

### In the story

| Switch | Action |
|---|---|
| A | advance text; shows a hidden message window |
| B | stop skipping, else hide / show the message window |
| X | backlog |
| Y | system menu |
| L | auto mode on / off |
| R | skip mode on / off |
| ZL (hold) | rewind, faster the longer it is held |
| ZR (hold) | fast forward, faster the longer it is held |
| D-pad / right stick up | open the mode panel |
| D-pad / right stick down | advance text, close the mode panel |
| D-pad up / down | move between choices |

| Touch (mouse) | Action |
|---|---|
| tap | advance text |
| drag left / right from the middle | rewind / fast forward, faster the further it is dragged |
| flick down from the top edge, or pointer at the top edge | open the mode panel |
| flick up, or tap below the panel | close the mode panel |
| flick right from the left edge, or right click | system menu |
| flick left from the right edge | backlog |
| pointer at the bottom edge | hide the message window |

The mode panel holds previous scene, rewind, auto, skip, next scene, quick save, quick load and voice replay.

### Menus and gallery

| Switch | Action |
|---|---|
| D-pad / right stick | move the cursor |
| A | confirm (hold A + left / right to drag a config slider) |
| B | back |
| L / R | config tabs; previous / next picture in the CG viewer |
| left stick | scroll lists, the flowchart and the config; move the picture in the CG viewer |
| right stick up / down | zoom in the CG viewer |

A mouse drag scrolls the flowchart, CG list, music list and config, like a touch drag on the Switch.

## License

knife is licensed under the [Mozilla Public License 2.0](LICENSE).
