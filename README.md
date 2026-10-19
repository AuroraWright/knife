# knife

A Rust reimplementation of the Tsukihime -A piece of blue glass moon- Switch engine.

## Running

```sh
CARGO_INCREMENTAL=0 cargo build --release -p knife
./target/release/knife   # reads switch_romfs/ and switch_exefs/ (see --romfs / --exefs)
```

`build/Tsukihime.command` starts the copied build with normal saves. `build/Tsukihime (Unlocked).command` starts it
with everything unlocked (see `tools/unlocked_save.py`). Saves go to `KNIFE_SAVE_DIR` when it is set.

## Saves

knife uses the Switch game's save file, `savedata.bin` (0x3E7000 bytes: settings, unlocks, read text and 140
save records), so a `savedata.bin` copied from a Switch loads as is, and the reverse also works. It is the only
file in the save directory; flowchart jumps restart the node's scene as on the Switch. As on the Switch, save list
thumbnails are drawn from each record's stage, and a save resumes at the start of its page. The format is documented in `knife/src/savedata.rs`
and `re/NOTES.md`.

## Controls

The keyboard stands in for the Switch controller, with the same key for the same button everywhere; each
button then does what it does in the Switch game on that screen (the game's own map is Config → 操作説明,
`conf_manual0_ja`). The left mouse button is the touch screen, and the mouse wheel / trackpad scroll is
the right stick.

| Key | Switch |
|---|---|
| Arrow keys | D-pad / left stick |
| Enter | A (confirm, advance text) |
| Escape | B (cancel, back) |
| B | X |
| M | Y |
| H / J | L / R |
| T / Y | ZL / ZR |
| K / L, mouse wheel / trackpad | right stick up / down (scroll) |
| U / I | Minus / Plus |
| F | toggle fullscreen |
| Left Ctrl (hold) | skip text while held |

### In the story

| Switch | Action |
|---|---|
| A | advance text; shows a hidden message window |
| B | stop skip mode, else hide / show the message window |
| X | backlog |
| Y | system menu |
| L | auto mode on / off |
| R | skip mode on / off (stops at unread text unless the config allows skipping it) |
| ZL (hold) | rewind, faster the longer it is held |
| ZR (hold) | fast forward, faster the longer it is held |
| D-pad up, right stick up | open the mode panel |
| D-pad down, right stick down | advance text, close the mode panel |
| D-pad left / right, A | with the mode panel opened by button: pick a button, press it (rewind and skip act while A is held) |
| D-pad up / down | move between choices (left / right on a No / Yes question) |

Touch (left mouse button):

| Gesture | Action |
|---|---|
| tap | advance text |
| drag left / right from the middle of the screen | stretch the arrow: rewind / fast forward, faster the further it stretches |
| flick down from the top edge, or pointer at the top edge | open the mode panel |
| flick up, or tap below the panel | close the mode panel |
| flick right from the left edge | system menu |
| flick left from the right edge | backlog |
| pointer at the bottom edge | hide the message window (back when the pointer moves up) |
| right click | system menu |

The mode panel holds previous scene, rewind (hold), auto, skip (hold), next scene, Q.SAVE, Q.LOAD and voice
replay.

### Menus and gallery

| Switch | Action |
|---|---|
| D-pad / left stick | move the cursor |
| A | confirm (hold A + left / right to drag a config slider) |
| B | back |
| L / R | config tabs, and L / R elsewhere (CG viewer: previous / next picture) |
| right stick | scroll lists, the flowchart, the backlog; zoom in the CG viewer |
| Y, ZL | also close the system menu |
| X, ZL | also close the backlog |
| Minus | CG viewer: toggle between fit and fill zoom |

The system menu is touch-only on the Switch, so mouse hover does not move its cursor; a click opens a tab.

## Debug options

`knife --help` lists them. Useful ones: `--load-slot N`, `--scene NAME`, `--shots DIR --shot-every MS
--exit-after MS` for screenshots, and `--keys MS:KEY,...` for scripted input. `--keys` accepts the buttons up, down, left,
right, a, b, x, y, l, r, zl, zr, rup, rdown, minus and plus (tapped, or held from `NAME+` to `NAME-`), and
menu, click, press, release and move/X/Y in canvas pixels. `--skip-movies` lets every story movie be skipped
(the Switch plays an unwatched opening, prologue or ending through), `--jump-node N` starts at a flowchart node, `--var F822=0,...` sets script variables after the start (F822 = 0 shows the
one-message-per-window layout of records made without START_SCRIPT).

Research notes live in `re/NOTES.md`.
