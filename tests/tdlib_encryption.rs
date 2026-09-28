//! The local database encryption the client's password relies on, checked
//! against the real TDLib in a temporary directory (no network, no
//! account): a database written with a key opens with that key only.

use std::time::Duration;

use tdlib_rs::enums::{AuthorizationState, Update};
use tokio::sync::mpsc;

/// Removes the temp directory when the test ends, even if an assertion
/// above panicked first.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Authorization states of all clients, by client id.
fn listen() -> mpsc::UnboundedReceiver<(i32, AuthorizationState)> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let received = tokio::task::spawn_blocking(tdlib_rs::receive)
                .await
                .unwrap();
            if let Some((Update::AuthorizationState(u), id)) = received
                && tx.send((id, u.authorization_state)).is_err()
            {
                return;
            }
        }
    });
    rx
}

async fn wait_for(
    rx: &mut mpsc::UnboundedReceiver<(i32, AuthorizationState)>,
    client: i32,
    wanted: fn(&AuthorizationState) -> bool,
) {
    loop {
        let (id, state) = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("no authorization state within 20 s")
            .unwrap();
        if id == client && wanted(&state) {
            return;
        }
    }
}

async fn start(
    rx: &mut mpsc::UnboundedReceiver<(i32, AuthorizationState)>,
    dir: &std::path::Path,
    key: &str,
) -> (i32, Result<(), tdlib_rs::types::Error>) {
    let client = tdlib_rs::create_client();
    tdlib_rs::functions::set_log_verbosity_level(0, client)
        .await
        .unwrap();
    wait_for(rx, client, |s| {
        *s == AuthorizationState::WaitTdlibParameters
    })
    .await;
    let result = tdlib_rs::functions::set_tdlib_parameters(
        false,
        dir.join("db").to_string_lossy().into_owned(),
        dir.join("files").to_string_lossy().into_owned(),
        key.to_owned(),
        true,
        true,
        true,
        false,
        94575,
        "a3406de8d171bb422bb6ddf3bbd800e2".into(),
        "en".into(),
        "test".into(),
        String::new(),
        "0".into(),
        client,
    )
    .await;
    (client, result)
}

async fn close(rx: &mut mpsc::UnboundedReceiver<(i32, AuthorizationState)>, client: i32) {
    tdlib_rs::functions::close(client).await.unwrap();
    wait_for(rx, client, |s| *s == AuthorizationState::Closed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn database_opens_with_its_key_only() {
    let dir =
        TempDir(std::env::temp_dir().join(format!("telega-tdlib-key-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let mut rx = listen();
    // Keys are bytes, sent base64-encoded.
    let key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    let other = "Hx4dHBsaGRgXFhUUExIREA8ODQwLCgkIBwYFBAMCAQA=";

    let (client, result) = start(&mut rx, &dir.0, key).await;
    result.expect("a new database takes any key");
    close(&mut rx, client).await;

    let (client, result) = start(&mut rx, &dir.0, other).await;
    let error = result.expect_err("a wrong key must not open the database");
    assert!(
        error.message.to_lowercase().contains("encryption"),
        "{}",
        error.message
    );
    close(&mut rx, client).await;

    // The right key opens it. (Re-keying needs a logged-in account, so it
    // is not covered offline.)
    let (client, result) = start(&mut rx, &dir.0, key).await;
    result.expect("the right key opens the database");
    close(&mut rx, client).await;
}
