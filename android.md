# knife on Android

knife runs on Android 8.0 (API 26) or newer, on arm64 phones and tablets. The screen is the Switch's touch screen,
the Back button is B, and a connected controller acts as the Switch controller (see the Controls section of
[README.md](README.md)). The app is landscape only, full screen, and keeps the screen awake while it runs.

## How it is put together

- `knife` is both a library (`knife_app`) and the desktop executable, which only runs the library. Nothing about the
  desktop build changes for Android.
- `knife-android` is a small crate that builds `libknife_android.so`. Its `android_main` calls the library's
  `run_android`. It is empty on every other platform.
- `android/` is the Gradle project. Its `MainActivity` extends `NativeActivity`, which loads the library, and adds
  the full screen mode. It also sets `RUST_MIN_STACK` to 8 MiB before the game thread starts, because the program
  is written for the 8 MiB stack of a desktop main thread (Android would give it 2 MiB).
- Gradle runs `cargo ndk` before everything else (`cargoBuild` in `android/app/build.gradle`), puts the library
  in `android/app/src/main/jniLibs/arm64-v8a` and copies the NDK's `libc++_shared.so` beside it.

## What you need

| | |
|---|---|
| Android SDK | platform 35 (`compileSdk`); the app runs on API 26+ and targets API 34 |
| Android NDK | `27.2.12479018`, as pinned in `android/app/build.gradle` (SDK Manager → SDK Tools → NDK (Side by side)) |
| JDK | 17 |
| Gradle | 8.9 or newer (the Android Gradle Plugin, 8.7.3, needs it). The repository has no Gradle wrapper: use your own `gradle`, or Android Studio, which brings one |
| Rust | the toolchain of `rust-toolchain.toml` (rustup picks it up by itself) plus the Android target |
| cargo-ndk | builds for the NDK's linker and sysroot |

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
```

`cargo`, `rustup` and `gradle` have to be on the `PATH` of the program that runs Gradle. When you start Android
Studio from a launcher rather than a terminal, it may not see `~/.cargo/bin`; start it from a terminal then.

Gradle needs to know the SDK folder. Android Studio writes it to `android/local.properties` when it opens the
project; by hand:

```properties
sdk.dir=/home/you/Android/Sdk
```

(`local.properties` is not committed.) You can also set `ANDROID_HOME`. The build passes the NDK it finds to
`cargo ndk` itself, so `ANDROID_NDK_HOME` need not be set.

## Building

```sh
cd android
gradle assembleRelease
```

The first build compiles the whole engine for the phone and takes a while; later ones are quick. The result is

```
android/app/build/outputs/apk/release/app-release.apk
```

`assembleDebug` makes `app/build/outputs/apk/debug/app-debug.apk` the same way. Both contain a release build of the
engine: a debug build of it is too slow to play. In Android Studio, open the `android` folder, pick a device and
press Run.

To build the engine alone, from the repository's root:

```sh
cargo ndk --target arm64-v8a --platform 26 --output-dir android/app/src/main/jniLibs build --release -p knife-android
```

### Signing

The release APK is signed with the debug key (`signingConfig signingConfigs.debug`), so that it installs without a
keystore. That is fine for your own phone. To publish an APK, add your own `signingConfigs` entry and point the
`release` build type at it.

### Other architectures

Only arm64 is built. For another ABI, add it to `abiFilters` and to `--target` of `cargoBuild` in
`android/app/build.gradle`, and install its Rust target (`armeabi-v7a` is `armv7-linux-androideabi`, `x86_64` is
`x86_64-linux-android`). Only arm64 has been tried.

## Installing

With the phone connected over USB debugging (or wireless debugging):

```sh
adb install -r android/app/build/outputs/apk/release/app-release.apk
```

Or copy the APK to the phone and open it there (allow installing from that app when Android asks).

## The game data

Start the app once, so that it makes its folder (it shows an error or a black screen without game data; that is
expected). The folder is

```
/sdcard/Android/data/dev.knife.app/files
```

It takes the place of the executable's folder on the desktop. In it:

| Path | |
|---|---|
| `assets/` | the game data: the extracted romfs plus the system fonts (see Game data in [README.md](README.md)) |
| `mod_assets/` | optional replacement files (see Mod/translation support) |
| `knife.ini` | optional settings (see below) |
| `knife/savedata000` | the save file (`knife/overseas/savedata000` for Tsukihime's overseas release, `knife/witch/savedata000` for Witch on the Holy Night) |

Copy the game data from a computer, either through the file transfer mode (MTP) of the USB connection or with adb:

```sh
F=/sdcard/Android/data/dev.knife.app/files
adb shell mkdir -p $F/assets
adb push <romfs folder>/. $F/assets/
```

The romfs is large; a fast cable and a phone with enough free space help. Newer Android versions stop the phone's own file
managers from opening other apps' `Android/data` folders, but a computer over USB and `adb` still reach it. If the app is uninstalled, Android deletes this folder with it,
saves included: copy `knife/` off the phone first, or set `save-file` in `knife.ini` to a place that stays (a
folder you made in the shared storage, for example).

A save file from a Switch loads as it is, and the other way round, as on the desktop.

## Settings

The options of the README work through `knife.ini` in the files folder; there is no command line. Relative paths
in it are resolved from that folder. For example:

```ini
language = en
skip-intro = true
save-file = /sdcard/Documents/knife/savedata000
```

The default is `resolution = original`: the game renders at the Switch's 1920x1080 and is scaled to the screen,
which keeps the picture identical to the Switch's. It runs at 60 frames a second on any refresh rate. `resolution = window`
renders at the screen's resolution instead (at least 1920x1080), which is heavier.

## Using it

- A finger is the Switch's touch screen: the story's taps, drags and flicks from the edges work as in the Controls
  section of the README. Android's own edge swipes (the Back gesture, pulling down the system bars) take
  precedence over the game's flicks from the same edges; swipe a little in from the edge or use the game's mode
  panel from the top.
- Back is B. The buttons of a controller are the Switch's, as on the desktop (the d-pad and the buttons, not the
  sticks).
- Movies are decoded by the phone's MediaCodec. If one does not play, the log says why; `skip-movies = true` lets
  every story movie be skipped.

## Problems

Read the log first:

```sh
adb logcat -s knife
```

The first line says which folder the app uses (`files folder: ...`). Panics are logged there too.

| Symptom | Likely cause |
|---|---|
| `SDK location not found` | no `sdk.dir` in `android/local.properties` and no `ANDROID_HOME` |
| `cargo: command not found`, or `cargo ndk` is missing | Gradle cannot see `~/.cargo/bin`: start it (or Android Studio) from a terminal, and check `cargo ndk --version` |
| NDK not found, or the wrong version | install `27.2.12479018` from the SDK Manager, or change `ndkVersion` and rebuild |
| `can't find crate for core` / `linker not found` for `aarch64-linux-android` | `rustup target add aarch64-linux-android` |
| Gradle complains about its own version | use 8.9 or newer |
| The app starts and stops at once, log mentions missing files | the game data is not in `assets` in the files folder, or lacks the system fonts |
| Only some text is shown, boxes instead of letters | `nintendo_udsg-r_std_003.bfttf` (and for the Chinese languages the other fonts) is missing from `assets` |
| The install fails with a signature conflict | the installed app was signed with another key: copy `knife/` (the saves) off the phone, then `adb uninstall dev.knife.app` and install again |
