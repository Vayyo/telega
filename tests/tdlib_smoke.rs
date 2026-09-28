//! Smoke test of the native TDLib library: it loads (all shared libraries
//! resolve), a client starts and reports its first authorization state.
//! No network and no account are needed.

use std::time::Duration;

use tdlib_rs::enums::{AuthorizationState, Update};

#[tokio::test(flavor = "multi_thread")]
async fn tdlib_client_starts() {
    let client_id = tdlib_rs::create_client();

    // Responses are delivered by `receive` too, so it must run before any request.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let received = tokio::task::spawn_blocking(tdlib_rs::receive)
                .await
                .unwrap();
            if let Some((Update::AuthorizationState(u), id)) = received
                && id == client_id
                && tx.send(u.authorization_state).is_err()
            {
                return;
            }
        }
    });

    // A client emits nothing until it receives its first request.
    tdlib_rs::functions::set_log_verbosity_level(0, client_id)
        .await
        .expect("TDLib rejected the first request");

    let state = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("TDLib did not report an authorization state within 10 s")
        .unwrap();
    assert_eq!(state, AuthorizationState::WaitTdlibParameters);
}
