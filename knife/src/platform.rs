//! What differs between the platforms outside the window: where the program's own files are (`assets`,
//! `knife.ini`, the saves) and, on Android, the log.

use std::path::PathBuf;

/// The executable's path, whose folder holds `assets`, `mod_assets` and `knife.ini` by default.
///
/// Android has no such file: the app's files folder stands in for the executable's, so these live in
/// `Android/data/<package>/files` of the shared storage, where the app may read and write without asking.
#[cfg(not(target_os = "android"))]
pub fn executable() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

#[cfg(target_os = "android")]
pub fn executable() -> Option<PathBuf> {
    android::files_dir().map(|dir| dir.join("knife"))
}

/// The OS's application data folder, which `knife/savedata000` goes in by default (None when it has none).
#[cfg(not(target_os = "android"))]
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir()
}

/// On Android the saves are beside the game data, so that both are one folder to look at and to back up.
/// (They go when the app is uninstalled; `save-file` in `knife.ini` puts them elsewhere.)
#[cfg(target_os = "android")]
pub fn data_dir() -> Option<PathBuf> {
    android::files_dir()
}

#[cfg(target_os = "android")]
pub mod android {
    use std::{
        ffi::{CString, c_char, c_int},
        io,
        path::PathBuf,
        sync::OnceLock,
    };

    use tracing::Level;
    use tracing_subscriber::fmt::MakeWriter;
    use winit::platform::android::activity::AndroidApp;

    static FILES_DIR: OnceLock<PathBuf> = OnceLock::new();

    /// The app's folder for files: `Android/data/<package>/files` on shared storage (reachable from a file
    /// manager or a computer, and for `adb push`), else the app's private one.
    pub fn files_dir() -> Option<PathBuf> {
        FILES_DIR.get().cloned()
    }

    /// Once, from `run_android`: find the app's folders and send the log to logcat.
    pub fn init(app: &AndroidApp) {
        if let Some(dir) = app.external_data_path().or_else(|| app.internal_data_path()) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = FILES_DIR.set(dir);
        }
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info,wgpu=warn,naga=warn"))
            .with_ansi(false)
            .without_time()
            .with_writer(Logcat)
            .init();
        // a panic's message goes to stderr, which is nowhere on Android
        std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
        tracing::info!("files folder: {:?}", files_dir());
    }

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    const TAG: &std::ffi::CStr = c"knife";

    /// `tracing` events as log lines in logcat (`adb logcat -s knife`), at their level's priority.
    #[derive(Clone, Copy)]
    pub struct Logcat;

    /// A writer for one event.
    pub struct LogLine(c_int);

    impl io::Write for LogLine {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            for line in String::from_utf8_lossy(buf).lines().filter(|line| !line.is_empty()) {
                if let Ok(text) = CString::new(line.replace('\0', " ")) {
                    // SAFETY: the tag and the text are NUL-terminated strings that outlive the call.
                    unsafe { __android_log_write(self.0, TAG.as_ptr(), text.as_ptr()) };
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Logcat {
        type Writer = LogLine;

        fn make_writer(&'a self) -> LogLine {
            LogLine(4)
        }

        fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> LogLine {
            let level = *meta.level();
            // android/log.h: ANDROID_LOG_ERROR 6, WARN 5, INFO 4, DEBUG 3, VERBOSE 2
            LogLine(if level == Level::ERROR {
                6
            } else if level == Level::WARN {
                5
            } else if level == Level::INFO {
                4
            } else if level == Level::DEBUG {
                3
            } else {
                2
            })
        }
    }
}
