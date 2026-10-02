// Copyright 2020 - developers of the `grammers` project.
// Copyright 2021 - developers of the `tdlib-rs` project.
// Copyright 2024 - developers of the `tgt` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
use futures_channel::oneshot;
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::task::{Context, Poll};

pub(super) struct Observer {
    requests: RwLock<HashMap<u32, (u64, oneshot::Sender<Value>)>>,
    next_id: AtomicU64,
}

pub(super) struct Pending<'a> {
    observer: &'a Observer,
    extra: u32,
    id: u64,
    receiver: oneshot::Receiver<Value>,
}

impl Future for Pending<'_> {
    type Output = Result<Value, oneshot::Canceled>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(cx)
    }
}

impl Pending<'_> {
    #[cfg(test)]
    pub fn try_recv(&mut self) -> Result<Option<Value>, oneshot::Canceled> {
        self.receiver.try_recv()
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let mut requests = self.observer.requests.write().unwrap_or_else(|e| e.into_inner());
        if requests.get(&self.extra).is_some_and(|(id, _)| *id == self.id) {
            let removed = requests.remove(&self.extra);
            drop(requests);
            drop(removed);
        }
    }
}

impl Observer {
    pub fn new() -> Self {
        Observer {
            requests: RwLock::default(),
            next_id: AtomicU64::new(0),
        }
    }

    pub fn subscribe(&self, extra: u32) -> Pending<'_> {
        let (sender, receiver) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let replaced = self
            .requests
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(extra, (id, sender));
        drop(replaced);
        Pending {
            observer: self,
            extra,
            id,
            receiver,
        }
    }

    pub fn notify(&self, response: Value) {
        let Some(extra) = response
            .get("@extra")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
        else {
            return;
        };
        let entry = self.requests.write().unwrap_or_else(|e| e.into_inner()).remove(&extra);
        if let Some((_, sender)) = entry {
            let _ = sender.send(response);
        }
    }
}

#[cfg(test)]
mod security_diagnostics {
    use super::Observer;
    use serde_json::json;

    #[test]
    #[ignore = "explicit local security audit probe"]
    fn security_probe_dropped_observer_receivers_release_all_pending_requests() {
        let observer = Observer::new();

        for extra in 0..10_000 {
            drop(observer.subscribe(extra));
        }
        assert_eq!(
            observer.requests.read().unwrap().len(),
            0,
            "canceled requests must not remain in the observer"
        );

        // A late response for a canceled request must not affect a live one.
        let mut live = observer.subscribe(10_000);
        let mut other = observer.subscribe(10_001);
        observer.notify(json!({"@extra": 0, "marker": "late"}));
        assert!(live.try_recv().unwrap().is_none());
        assert!(other.try_recv().unwrap().is_none());

        observer.notify(json!({"@extra": 10_000, "marker": 731}));
        let response = live.try_recv().unwrap().expect("matching response");
        assert_eq!(response["@extra"], 10_000);
        assert_eq!(response["marker"], 731);
        assert!(
            other.try_recv().unwrap().is_none(),
            "unrelated receiver must remain pending"
        );
        observer.notify(json!({"@extra": 10_001, "marker": 732}));
        assert_eq!(
            other.try_recv().unwrap().expect("other matching response")["marker"],
            732
        );
        assert_eq!(observer.requests.read().unwrap().len(), 0);
    }

    #[test]
    #[ignore = "explicit local security audit probe"]
    fn security_probe_unknown_and_malformed_extras_preserve_valid_correlations() {
        let observer = Observer::new();
        let mut first = observer.subscribe(17);
        let mut second = observer.subscribe(19);

        observer.notify(json!({"@extra": 20, "marker": "unknown"}));
        assert_eq!(observer.requests.read().unwrap().len(), 2);

        for malformed in [
            json!({"marker": "missing"}),
            json!({"@extra": null}),
            json!({"@extra": "17"}),
            json!({"@extra": -1}),
            json!({"@extra": 17.5}),
            json!({"@extra": u64::from(u32::MAX) + 18, "marker": "alias"}),
        ] {
            let result = std::panic::catch_unwind(|| observer.notify(malformed.clone()));
            assert!(
                result.is_ok(),
                "malformed native @extra must not panic: {malformed}"
            );
            assert_eq!(
                observer.requests.read().unwrap().len(),
                2,
                "malformed @extra must not remove another request: {malformed}"
            );
            assert!(first.try_recv().unwrap().is_none());
            assert!(second.try_recv().unwrap().is_none());
        }

        observer.notify(json!({"@extra": 19, "marker": 921}));
        assert_eq!(
            second.try_recv().unwrap().expect("second response")["marker"],
            921
        );
        assert!(first.try_recv().unwrap().is_none());
        observer.notify(json!({"@extra": 17, "marker": 731}));
        let response = first.try_recv().unwrap().expect("first response");
        assert_eq!(response["@extra"], 17);
        assert_eq!(response["marker"], 731);
        assert_eq!(observer.requests.read().unwrap().len(), 0);
    }
}
