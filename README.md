# knife

A Rust reimplementation of the Tsukihime -A piece of blue glass moon- Switch engine.

## Running

```sh
CARGO_INCREMENTAL=0 cargo build --release -p knife
./target/release/knife   # reads switch_romfs/ and switch_exefs/ (see --romfs / --exefs)
```

`build/Tsukihime.command` starts the copied build with normal saves. `build/Tsukihime (Unlocked).command` starts it
with everything unlocked (see `tools/unlocked_save.py`). Saves go to `KNIFE_SAVE_DIR` when it is set.

The menus, help lines and music list use the Switch's shared system font, which is not in the game data. Dump
`nintendo_udsg-r_std_003.bfttf` from the firmware's FontStandard archive (title 0100000000000811, the NCA of
that title under an emulator's `bis/system/Contents/registered/`) into `switch_sysfont/`:

```sh
hactoolnet -k prod.keys --romfsdir switch_sysfont <FontStandard>.nca
```

knife reads it from `switch_sysfont/` in the working directory (or `--sysfont PATH`).

Movies are decoded by the platform: VideoToolbox on macOS, Media Foundation on Windows (N editions need the Media
Feature Pack) and GStreamer elsewhere (with an H.264 plugin such as gst-libav, and the GStreamer development
packages to build). An `ffmpeg` executable on the `PATH` is the fallback; `--features ffmpeg` always uses it.

## Saves

knife uses the Switch game's save file, `savedata.bin` (0x3E7000 bytes: settings, unlocks, read text and 140
save records), so a `savedata.bin` copied from a Switch loads as is, and the reverse also works. It is the only
file in the save directory; flowchart jumps restart the node's scene as on the Switch. As on the Switch, save list
thumbnails are drawn from each record's stage, and a save resumes at the start of its page. The format is documented in `knife/src/savedata.rs`
and `re/NOTES.md`.

## Controls

The keyboard stands in for the Switch controller, with the same key for the same button everywhere; each
button then does what it does in the Switch game on that screen (the game's own map is Config → 操作説明,
`conf_manual0_ja`). The left mouse button is the touch screen; the mouse wheel / trackpad tilts the left
stick (scrolling lists, the flowchart and config, panning the CG viewer) and does nothing else; in the save
list it scrolls the frames instead, a row per notch or per 1.5 save frame heights of trackpad travel, keeping
the cursor on its save while that stays in view.

Game controllers work too (gilrs, as in shin; on macOS the GameController framework, which keeps
working after another app has used the pad): every controller is the same Switch controller, button for
button — A / B / X / Y as the controller labels them, Plus / Minus (Start / Back), L / R, ZL / ZR, the d-pad,
both sticks and R3. As in the game, the right stick moves cursors like the d-pad while the left stick scrolls
lists and pans the CG viewer. Held directions repeat like on the Switch (after 15 frames, then every 5).

| Key | Switch |
|---|---|
| Arrow keys | D-pad |
| W / A / S / D | left stick (full tilt) |
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
| B | stop skip mode, else hide / show the message window |
| X | backlog |
| Y | system menu (not until the first message after a story start or a load) |
| L | auto mode on / off |
| R | skip mode on / off (stops at unread text unless the config allows skipping it) |
| ZL (hold) | rewind, faster the longer it is held |
| ZR (hold) | fast forward, faster the longer it is held |
| D-pad / right stick up | open the mode panel |
| D-pad / right stick down | advance text, close the mode panel |
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
replay. Next / previous scene darken the screen for 20 frames before the jump.

During the staff roll only A (advance) and ZR (hold: fast forward at a fixed speed) work, as on the Switch.

### Menus and gallery

| Switch | Action |
|---|---|
| D-pad / right stick | move the cursor |
| A | confirm (hold A + left / right to drag a config slider) |
| B | back |
| L / R | config tabs, and L / R elsewhere (CG viewer: previous / next picture) |
| left stick | scroll the CG grid, music list, flowchart, config and save list; CG grid: left / right also move the cursor |
| CG viewer | L / R or left / right: previous / next picture; left stick: move; right stick up / down: zoom; Minus: fill the screen; R3: fit |
| Y, ZL | also close the system menu |
| X, ZL | also close the backlog |

The system menu is touch-only on the Switch, so mouse hover does not move its cursor; a click opens a tab.
A mouse drag (a touch drag on the Switch) scrolls the flowchart, CG grid, music list and config, and a flick
coasts; dragging the scroll bar's slider scrolls the flowchart, CG grid and music list. A gallery movie stops with B or Plus only.

## Debug options

`knife --help` lists them. Useful ones: `--load-slot N`, `--scene NAME`, `--shots DIR --shot-every MS
--exit-after MS` for screenshots, and `--keys MS:KEY,...` for scripted input. `--keys` accepts the buttons up, down, left,
right, a, b, x, y, l, r, zl, zr, rup, rdown, minus and plus (tapped, or held from `NAME+` to `NAME-`), and
menu, click, press, release, move/X/Y in canvas pixels and wheel/N (mouse wheel notches, + up). `--skip-movies` lets every story movie be skipped
(the Switch plays an unwatched opening, prologue or ending through), `--jump-node N` starts at a flowchart node, `--var F822=0,...` sets script variables after the start (F822 = 0 shows the
one-message-per-window layout of records made without START_SCRIPT).

## Comparing with the Switch original (Astris)

`tools/emu/` drives the original in the Astris emulator and captures it, for side-by-side checks against
knife. Setup: start `python3 tools/emu/dsu_server.py` (a DSU / cemuhook controller server on 127.0.0.1:26760,
control socket 26761), set Astris Input to DSU Controller at 127.0.0.1:26760 and pick slot 0, and run the
game on the save in `switch_emu_save` (knife only ever uses a copy of it). Astris reads the controller only
while focused; the commands bring it to the front.

- `tools/emu/emu press a` (also hold / release / stick L X Y / reset): one input to the Switch.
- `tools/emu/emu shot out.png`: the game area at 1920x1080.
- `tools/emu/emu tap X Y [MS]` / `drag X0 Y0 X1 Y1 [MS]`: a touch at game coordinates (1920x1080). Astris
  turns the mouse into the touch screen, but only for events posted at the HID level, and the first touch after
  it gains focus only switches touch on. Between touches the resting pointer still counts as the Switch's
  mouse (a pointer left at the top reopens the story's mode panel), so both park it at the picture's centre
  afterwards. A drag holds still for 100 ms after the press, since Astris registers a touch a few frames late.
- `tools/emu/emu run NAME "500:down,1000:a"`: the inputs (knife `--keys` syntax) with captures every
  `--every` ms until `--until` ms into `build/emu/NAME/` (real capture times in `emu_times.txt`).
- `tools/emu/emu knife NAME "KEYS" --title`: the same keys through knife on a copy of the emulator save;
  `--title` boots to the title like a normal start, and other trailing arguments (e.g. `--load-slot 7`) pass
  through to knife. `tools/emu/emu compare NAME` puts both side by side.

Window captures come in the display's colour space (their ICC profile, Display P3 here); `emu.py` converts
them to sRGB so they compare with knife pixel for pixel. QuickTime recordings of the emulator window are not
colour-faithful. The emulator's live save is `savedata000` in both
`~/Library/Containers/V380-Ori.Astris/Data/Library/Application Support/Ryujinx/bis/user/save/0000000000000004/{0,1}/`
(`switch_emu_save` is a separate copy). Replace it only while the game is stopped (the running game writes it
back), then restart: Astris's stop button, then double-click the game in the list. The load list shows the
registered slots sorted by save date.

Research notes live in `re/NOTES.md`.
