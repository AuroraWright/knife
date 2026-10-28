# Changes

## 0.9.8 (2026-10-28)

- Text is drawn the way the Switch draws it: the glyphs are rendered by a port of the font rasteriser the game
  uses, so the letter shapes, hinting and outlines match the Switch for Japanese, English and Chinese text in
  both games.
- Lines break by the games' own rules in every language of Tsukihime and Witch on the Holy Night: words that
  reach the edge of the text box now move to the next line instead of being split.
- Story images, thumbnails and the title's parts are placed and scaled as on the Switch, a fraction of a pixel
  that showed as slightly shifted characters and soft edges.
- The system menu's cursor line has the Switch's length and position.
- Skipping no longer stalls: images, graded images and sound effects are read ahead while skipping, and
  decoding uses every CPU core.
- Choices and the TeaTime menu no longer blink their glow while skipping.

## 0.9.7 (2026-10-27)

- The game renders at the window's size instead of always at 1080p and scaling the picture, so text and
  images stay sharp in large and fullscreen windows.

## 0.9.6 (2026-10-27)

- Mod and translation support: files in a `mod_assets` folder (or the folder set with `-m` /
  `--mod-assets-dir`) replace the game data's files at the same path, including scripts, archives, movies,
  sound and fonts. See the README.

## 0.9.5 (2026-10-27)

- Menu, gallery and CG viewer animations match the Switch: the focus flash and pulse on title entries, menu
  tabs, config text and dialog buttons, the system menu's cursor light, the title's bounce, the gallery
  guide's fades and the CG viewer's wipe between pictures.
- Closing the CG viewer no longer leaves the gallery unresponsive.
- Music player: Play restarts the selected track (a paused track no longer stays silent while shown as
  playing), the volume slider follows a drag, and the pointer no longer lights music list rows under the
  panel.

## 0.9.4 (2026-10-26)

- The mouse no longer selects things that the controller can't select at that moment.

## 0.9.3 (2026-10-26)

- Tsukihime's main menu timing matches the Switch.
- The pointer stops highlighting entries while the config, CG gallery, music list or flowchart scrolls.
- Witch on the Holy Night: the backlog scrolls one entry per entry's height of trackpad travel.

## 0.9.2 (2026-10-26)

- A hidden or minimised window keeps the game running at 60 frames per second without drawing, and a long
  pause no longer makes menus jump ahead.

## 0.9.1 (2026-10-25)

- Witch on the Holy Night support: its title, system menu, save and load, config, backlog, CG, sound and
  archive modes, TeaTime menu, script commands, fonts and languages (Japanese, English, Simplified and
  Traditional Chinese), and the unlocked save's witch mode.
- Rewind and skip behave as on the Switch: holding ZL / ZR runs at full speed from the first frame, and held
  skips and rewinds step through the same history, so the backlog has the same depth.
- A right click acts as B on every screen, so it closes the mode panel instead of opening the system menu.
- Witch on the Holy Night: loading a save no longer shows the loaded message twice in the backlog.
- The engine write-up covers Witch on the Holy Night.

## 0.9.0 (2026-10-24)

First release: a reimplementation of the Switch engine of Tsukihime -A piece of blue glass moon- that plays
the game from its Switch data.

- Both releases of Tsukihime: the Japanese one and the overseas one (Japanese, English, Simplified and
  Traditional Chinese, switched with `-l` / `--language` or live from the config).
- The title, system menu, flowchart, gallery, CG viewer, music player, config, backlog, choices, QA bubbles
  and story movies, with controller, keyboard, mouse and touch controls.
- Saves in the Switch game's own format: a save copied from a Switch loads as is, and the reverse also works.
- Colours, screen effects, sound mixing and script timing checked against the Switch game.
- Movies decode through the platform (VideoToolbox, Media Foundation, GStreamer, or `ffmpeg` as a fallback).
- `--text-websocket` serves the displayed text to text hookers.
- Settings in a `knife.ini` next to the executable.
- Builds for macOS, Windows and Linux.
