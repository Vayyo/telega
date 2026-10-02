//! Validates and routes the untrusted TDLib wire envelope.

use crate::enums::Update;
use crate::observer::Observer;
use serde_json::Value;

/// Routes a TDLib wire message without retaining or logging its payload.
pub(crate) fn parse_response(raw: &str, observer: &Observer) -> Option<(Update, i32)> {
    let Ok(response) = serde_json::from_str::<Value>(raw) else {
        log::warn!("Ignoring malformed TDLib JSON");
        return None;
    };
    let Some(object) = response.as_object() else {
        log::warn!("Ignoring non-object TDLib response");
        return None;
    };
    if object.contains_key("@extra") {
        observer.notify(response);
        return None;
    }
    if object.get("@type").and_then(Value::as_str).is_none() {
        log::warn!("Ignoring TDLib response without a valid type");
        return None;
    }
    let Some(client_id) = object
        .get("@client_id")
        .and_then(Value::as_i64)
        .and_then(|id| i32::try_from(id).ok())
    else {
        log::warn!("Ignoring TDLib update without a valid client ID");
        return None;
    };
    match serde_json::from_value(response) {
        Ok(update) => Some((update, client_id)),
        Err(_) => {
            log::warn!("Ignoring unrecognized TDLib update");
            None
        }
    }
}

#[cfg(test)]
mod security_regressions {
    use super::{Observer, Update, parse_response};
    use std::sync::{OnceLock, mpsc};

    struct CapturingLogger;
    static LOGGER: CapturingLogger = CapturingLogger;
    static LOG_SENDER: OnceLock<mpsc::Sender<String>> = OnceLock::new();

    impl log::Log for CapturingLogger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            if record.level() <= log::Level::Warn
                && let Some(sender) = LOG_SENDER.get()
            {
                let _ = sender.send(record.args().to_string());
            }
        }

        fn flush(&self) {}
    }

    #[test]
    #[ignore = "explicit local security audit probe"]
    fn security_probe_invalid_native_responses_do_not_leak_payload_or_interrupt_updates() {
        let (sender, receiver) = mpsc::channel();
        LOG_SENDER.set(sender).expect("single parser log capture test");
        log::set_logger(&LOGGER).expect("no other logger in vendored library tests");
        log::set_max_level(log::LevelFilter::Warn);
        let observer = Observer::new();

        const SECRET: &str = "private-native-payload-245810";
        for response in [
            format!(r#"{{"@type":"updateAuthorizationState","@client_id":7,"private":"{SECRET}""#),
            format!(
                r#"{{"@type":"updateAuthorizationState","authorization_state":{{"@type":"authorizationStateReady"}},"private":"{SECRET}"}}"#
            ),
            format!(
                r#"{{"@type":"updateAuthorizationState","@client_id":2147483648,"authorization_state":{{"@type":"authorizationStateReady"}},"private":"{SECRET}"}}"#
            ),
            format!(
                r#"{{"@type":"unknownNativeType","@client_id":7,"private":"{SECRET}"}}"#
            ),
        ] {
            assert!(
                parse_response(&response, &observer).is_none(),
                "invalid native envelope must not yield an update"
            );
        }

        let (update, client_id) = parse_response(
            r#"{"@type":"updateAuthorizationState","@client_id":7,"authorization_state":{"@type":"authorizationStateReady"}}"#,
            &observer,
        )
        .expect("valid update after malformed envelopes");
        assert_eq!(client_id, 7);
        assert!(matches!(update, Update::AuthorizationState(_)));

        let warnings: Vec<_> = receiver.try_iter().collect();
        assert!(
            warnings.iter().all(|warning| !warning.contains(SECRET)),
            "warnings must not contain native response payload: {warnings:?}"
        );
    }

    #[test]
    #[ignore = "explicit local security audit probe"]
    fn security_probe_correlated_replies_with_malformed_types_reach_their_subscribers() {
        let observer = Observer::new();
        for (extra, response, marker) in [
            (41, r#"{"@extra":41,"message":"missing type"}"#, "missing type"),
            (42, r#"{"@extra":42,"@type":null,"message":"null type"}"#, "null type"),
            (43, r#"{"@extra":43,"@type":123,"message":"numeric type"}"#, "numeric type"),
        ] {
            let mut pending = observer.subscribe(extra);
            assert!(
                parse_response(response, &observer).is_none(),
                "correlated response is not an update"
            );
            let delivered = pending
                .try_recv()
                .expect("observer receiver remains valid")
                .expect("malformed correlated response must complete the request");
            assert_eq!(delivered["@extra"], extra);
            assert_eq!(delivered["message"], marker);
        }
    }
}
