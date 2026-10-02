// Compiles the real vendored Observer and native response parser against
// cached dependencies without invoking Cargo, TDLib, or a receive pump.
pub use tdlib_rs::enums;
#[path = "../../vendor/tdlib-rs/src/observer.rs"]
mod observer;
#[path = "../../vendor/tdlib-rs/src/response.rs"]
mod response;
