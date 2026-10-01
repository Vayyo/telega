//! Smoke test of the native TDLib library: it loads (all shared libraries
//! resolve), a client starts, accepts its parameters and answers requests.
//! No network and no account are needed: a throwaway session directory and a
//! fake `api_id` are enough, because the options below must be accepted, and
//! answered, before the account is entered.

use std::time::Duration;

use tdlib_rs::enums::{AuthorizationState, OptionValue, Update};

/// TDLib answers every request; a test must fail, not hang, when it does not.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

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

    let state = tokio::time::timeout(ANSWER_TIMEOUT, rx.recv())
        .await
        .expect("TDLib did not report an authorization state within 10 s")
        .unwrap();
    assert_eq!(state, AuthorizationState::WaitTdlibParameters);

    // TDLib answers no request (not even getOption) before it has parameters,
    // so they come first, exactly as the client does it. The session is a
    // throwaway directory of this process: the user's data is never touched.
    let session = std::env::temp_dir().join(format!("telega-tdlib-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&session);
    let database = session.join("db");
    let files = session.join("files");
    std::fs::create_dir_all(&database).expect("cannot create the temporary database directory");
    std::fs::create_dir_all(&files).expect("cannot create the temporary files directory");

    tokio::time::timeout(
        ANSWER_TIMEOUT,
        tdlib_rs::functions::set_tdlib_parameters(
            false,
            database.to_string_lossy().into_owned(),
            files.to_string_lossy().into_owned(),
            String::new(),
            true,
            true,
            true,
            false,
            1,
            "00000000000000000000000000000000".into(),
            "en".into(),
            "Desktop".into(),
            String::new(),
            env!("CARGO_PKG_VERSION").into(),
            client_id,
        ),
    )
    .await
    .expect("TDLib did not answer setTdlibParameters within 10 s")
    .expect("TDLib rejected setTdlibParameters");

    // Default policy: 10 GiB expressed in KiB, 90 days in seconds, and no
    // separate file-count cap. Also exercise the 100 GiB slider maximum.
    for (name, value) in [
        ("storage_max_files_size", 10_485_760),
        ("storage_max_time_from_last_access", 7_776_000),
        ("storage_max_file_count", i32::MAX as i64),
        ("storage_max_files_size", 104_857_600),
    ] {
        tokio::time::timeout(
            ANSWER_TIMEOUT,
            tdlib_rs::functions::set_option(
                name.into(),
                Some(OptionValue::Integer(tdlib_rs::types::OptionValueInteger {
                    value,
                })),
                client_id,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("TDLib did not answer setOption {name} within 10 s"))
        .unwrap_or_else(|error| panic!("TDLib rejected setOption {name} = {value}: {error:?}"));

        let stored = tokio::time::timeout(
            ANSWER_TIMEOUT,
            tdlib_rs::functions::get_option(name.into(), client_id),
        )
        .await
        .unwrap_or_else(|_| panic!("TDLib did not answer getOption {name} within 10 s"))
        .unwrap_or_else(|error| panic!("TDLib rejected getOption {name}: {error:?}"));
        assert_eq!(
            stored,
            OptionValue::Integer(tdlib_rs::types::OptionValueInteger { value }),
            "TDLib did not keep {name} = {value}"
        );
    }

    let _ = std::fs::remove_dir_all(&session);
}
