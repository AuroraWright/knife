//! The library the Android app's NativeActivity loads (`android.app.lib_name` in its manifest): it calls
//! `android_main` once the activity has started. The program is the `knife_app` library; this only starts it.
#![cfg(target_os = "android")]

use knife_app::AndroidApp;

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    if let Err(error) = knife_app::run_android(app) {
        tracing::error!("{error:#}");
    }
}
