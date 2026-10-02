// Copyright 2020 - developers of the `grammers` project.
// Copyright 2021 - developers of the `tdlib-rs` project.
// Copyright 2024 - developers of the `tgt` and `tdlib-rs` projects.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
pub mod build;
mod generated;
mod observer;
mod response;
mod tdjson;

pub use generated::{enums, functions, types};

use enums::Update;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};

static EXTRA_COUNTER: AtomicU32 = AtomicU32::new(0);
static OBSERVER: Lazy<observer::Observer> = Lazy::new(observer::Observer::new);

/// Create a TdLib client returning its id. Note that to start receiving
/// updates for a client you need to send at least a request with it first.
pub fn create_client() -> i32 {
    tdjson::create_client()
}

/// Receive a single update or response from TdLib. If it's an update, it
/// returns a tuple with the `Update` and the associated `client_id`.
/// Note that to start receiving updates for a client you need to send
/// at least a request with it first.
pub fn receive() -> Option<(Update, i32)> {
    tdjson::receive(2.0).and_then(|raw| response::parse_response(&raw, &OBSERVER))
}

/// Sensitive app requests avoid generated String arguments, which otherwise
/// remain live in their async frames until the corresponding reply arrives.
pub async fn send_secret_request(client_id: i32, request: Value) -> Result<(), types::Error> {
    let response = send_request(client_id, request).await;
    match response.get("@type").and_then(Value::as_str) {
        Some("ok") => Ok(()),
        Some("error") => Err(serde_json::from_value(response).unwrap_or_else(|_| types::Error {
            code: 500,
            message: "Invalid TDLib error response".into(),
        })),
        _ => Err(types::Error {
            code: 500,
            message: "Invalid TDLib response".into(),
        }),
    }
}

fn wipe_request(value: &mut Value) {
    match value {
        Value::String(string) => zeroize::Zeroize::zeroize(string),
        Value::Array(items) => items.iter_mut().for_each(wipe_request),
        Value::Object(fields) => fields.values_mut().for_each(wipe_request),
        _ => {}
    }
}

pub(crate) async fn send_request(client_id: i32, mut request: Value) -> Value {
    let extra = EXTRA_COUNTER.fetch_add(1, Ordering::Relaxed);
    request["@extra"] = Value::from(extra);

    let receiver = OBSERVER.subscribe(extra);
    let wire = request.to_string();
    wipe_request(&mut request);
    tdjson::send(client_id, wire);

    receiver.await.expect("TDLib response subscriber unexpectedly canceled")
}

