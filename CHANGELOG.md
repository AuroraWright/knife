# Changes

## 0.9.10 (2026-10-31)

- The Japanese release follows the Switch's update 1.0.3, which brings it close to the overseas release:
  the system menu's new blur, choices laid out and touched as in 1.0.3, the save list's X to delete a save,
  the endings gallery and CG groups, and the config's new radio and toggle art. Extract the game with
  update 1.0.3 applied; data from 1.0.2 still plays.
- `--tsukihimates` applies the code changes of the [Tsukihimates](https://github.com/Tsukihimates/Tsukihime-Translation)
  English patch (Japanese release, with their romfs as the mod assets folder), so their text breaks and
  places lines as their patched game does.
- Fan translations display as on the Switch: form feeds, control codes in the text, accented letters
  built from parts in TrueType fonts, and the fonts' character maps and fallback glyph.
- Control codes in the text move bars, break lines and switch fonts as on the Switch in all three builds.
  In Witch on the Holy Night, bracket names such as `[heart]` stay plain text, and a code that stops the
  Switch game stops knife with an error.
- The bars of `[ber]` and `[zap]` sit a pixel lower, as on the Switch.
- TrueType fonts are hinted by a port of the game's own interpreter, so their letters match the Switch's
  pixel for pixel.
- English lines break over the whole message, so an italic word at the edge of the box moves to the next
  line whole.
- OpenType fonts kern letter pairs as the Switch does, and text edges land on the Switch's sub-pixels.
- Text that fills the game's glyph cache draws as it does on the Switch.
- Story images decode exactly as on the Switch (some JPEG pixels were a level off).
- Witch on the Holy Night: rubies sit 2.4 px higher, as on the Switch, and docked mode shows the HD
  textures at their own size instead of scaled-up SD ones.
- Some script commands in a few scenes of each game read garbled arguments; they now read the right ones.
- Pressing down through the Japanese config's system tab no longer crashes.
- The engine write-up covers the text processor: how a message's markup becomes tags, lines and glyphs.

## 0.9.9 (2026-10-28)

- `-f` / `--fullscreen` (or `fullscreen = true` in `knife.ini`) starts the game in fullscreen.

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
