use android_activity::{AndroidApp, MainEvent, PollEvent};
use anyhow::Result;

mod core;

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("amadeus"),
    );

    if let Err(e) = run(app) {
        log::error!("amadeus-android fatal error: {e:#}");
    }
}

fn run(app: AndroidApp) -> Result<()> {
    core::native_android::NativeAndroidRuntime::new(app)?.run()
}
