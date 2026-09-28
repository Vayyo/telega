// Release builds on Windows run without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod archive;
mod av;
mod instance;
mod lock;
mod paths;
mod plugins;
mod settings;
mod td;

use app::App;

fn main() -> iced::Result {
    // The instance lock lives in the data directory, so prepare it first.
    if let Err(error) = paths::init() {
        startup_error(&error);
    }

    // One client per data directory: the launcher entry is easy to trigger
    // twice, and two clients would fight over the same TDLib database. The
    // guard lives until the client exits.
    let _instance = match instance::acquire() {
        Ok(instance::Lock::Held(guard)) => guard,
        Ok(instance::Lock::Busy) => {
            already_running();
            return Ok(());
        }
        Err(error) => startup_error(&error),
    };

    // Daemon: several windows; the app opens the main one itself and quits
    // when it is closed.
    iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(App::theme)
        .run()
}

/// The client is already up: say so instead of opening a second window on
/// the same data directory.
fn already_running() {
    eprintln!("клиент уже запущен — второе окно не нужно");
    let _ = notify_rust::Notification::new()
        .appname("Telega")
        .summary("Telega уже запущена")
        .body("Окно клиента открыто")
        .show();
}

/// A missing lock means TDLib must never be started on this directory.
fn startup_error(error: &str) -> ! {
    eprintln!("Telega: {error}");
    let _ = notify_rust::Notification::new()
        .appname("Telega")
        .summary("Telega: ошибка запуска")
        .body(error)
        .show();
    std::process::exit(1);
}
