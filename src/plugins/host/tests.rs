//! Plugin host behavior: real Luau VMs and SQLite files in a temp dir, time
//! passed explicitly.

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::plugins::BUILTIN;

fn origin() -> crate::plugins::Origin {
    crate::plugins::Origin {
        client_id: 1,
        account_id: 1,
    }
}

fn replacement_origin() -> crate::plugins::Origin {
    crate::plugins::Origin {
        client_id: 2,
        account_id: 1,
    }
}
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("telega-plugins-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn autodelete() -> (String, String, bool) {
    let (id, src) = BUILTIN[0];
    (id.into(), src.into(), true)
}

fn plugin(id: &str, src: &str) -> (String, String, bool) {
    (id.into(), src.into(), false)
}

fn config(enabled: bool, dry_run: bool, values: serde_json::Value) -> PluginConfig {
    PluginConfig {
        enabled,
        dry_run,
        values: serde_json::from_value(values).unwrap(),
        // Simulates a user who granted everything the plugin could ask for;
        // tests of the granted-permissions check build their own config.
        granted: vec!["read".into(), "delete_own".into(), "send".into()],
    }
}

/// Host with one account and the given plugin configs, output drained.
fn host(
    dir: &TempDir,
    sources: Vec<(String, String, bool)>,
    configs: &[(&str, PluginConfig)],
) -> Host {
    let mut host = Host::new(sources, 0.0).with_db_dir(&dir.0);
    host.handle(
        HostCmd::Configure(
            configs
                .iter()
                .map(|(id, c)| ((*id).into(), c.clone()))
                .collect(),
        ),
        0.0,
    );
    host.handle(HostCmd::Account(Some(origin())), 0.0);
    host.drain();
    host
}

fn own_message(chat_id: i64, id: i64) -> HostCmd {
    HostCmd::Event(
        origin(),
        Event::New {
            chat_id,
            id,
            text: "hi".into(),
            outgoing: true,
            sender_id: 1,
        },
    )
}

fn actions(host: &mut Host) -> Vec<Action> {
    host.drain()
        .into_iter()
        .filter_map(|e| match e {
            HostEvent::Action(_, _, a) => Some(a),
            _ => None,
        })
        .collect()
}

fn logs(host: &mut Host) -> Vec<String> {
    host.drain()
        .into_iter()
        .filter_map(|e| match e {
            HostEvent::Log(_, l) => Some(l),
            _ => None,
        })
        .collect()
}

fn autodelete_on(dry_run: bool) -> PluginConfig {
    config(true, dry_run, json!({ "chats": [5], "minutes": 1 }))
}

#[test]
fn autodelete_deletes_own_messages_in_selected_chats_after_delay() {
    let dir = TempDir::new();
    let mut host = host(
        &dir,
        vec![autodelete()],
        &[("autodelete", autodelete_on(false))],
    );

    host.handle(own_message(5, 10), 1000.0);
    // Not selected chat and not own message: ignored.
    host.handle(own_message(6, 11), 1000.0);
    host.handle(
        HostCmd::Event(
            origin(),
            Event::New {
                chat_id: 5,
                id: 12,
                text: "x".into(),
                outgoing: false,
                sender_id: 2,
            },
        ),
        1000.0,
    );

    host.tick(1059.0);
    assert!(actions(&mut host).is_empty());
    assert_eq!(host.next_wakeup(1059.0), Some(1060.0));
    host.tick(1060.0);
    assert_eq!(
        actions(&mut host),
        [Action::Delete {
            chat_id: 5,
            ids: vec![10],
            revoke: true
        }]
    );
    host.tick(5000.0);
    assert!(actions(&mut host).is_empty(), "each task runs once");
}

#[test]
fn scheduled_tasks_survive_restart_and_run_when_overdue() {
    let dir = TempDir::new();
    let configs = [("autodelete", autodelete_on(false))];
    let mut first = host(&dir, vec![autodelete()], &configs);
    first.handle(own_message(5, 10), 1000.0);
    drop(first);

    // Client was closed past the due time; the task runs on the next start.
    let mut second = host(&dir, vec![autodelete()], &configs);
    second.tick(2000.0);
    assert_eq!(actions(&mut second).len(), 1);
}

#[test]
fn dry_run_only_logs() {
    let dir = TempDir::new();
    let mut host = host(
        &dir,
        vec![autodelete()],
        &[("autodelete", autodelete_on(true))],
    );
    host.handle(own_message(5, 10), 0.0);
    host.tick(60.0);
    let events = host.drain();
    assert!(!events.iter().any(|e| matches!(e, HostEvent::Action(..))));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HostEvent::Log(_, l) if l.contains("пробный прогон")))
    );
}

#[test]
fn disabling_cancels_planned_tasks() {
    let dir = TempDir::new();
    let mut host = host(
        &dir,
        vec![autodelete()],
        &[("autodelete", autodelete_on(false))],
    );
    host.handle(own_message(5, 10), 0.0);

    let off = config(false, false, json!({ "chats": [5], "minutes": 1 }));
    host.handle(HostCmd::Configure([("autodelete".into(), off)].into()), 1.0);
    host.handle(
        HostCmd::Configure([("autodelete".into(), autodelete_on(false))].into()),
        2.0,
    );
    host.tick(1000.0);
    assert!(actions(&mut host).is_empty());
}

#[test]
fn undeclared_permission_is_refused() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "sneaky", permissions = { "read" } }
        telega.on("message_new", function(m) telega.delete(m.chat_id, { m.id }) end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("sneaky", src)],
        &[("sneaky", config(true, false, json!({})))],
    );
    host.handle(own_message(5, 10), 0.0);
    host.tick(100.0);
    let events = host.drain();
    assert!(!events.iter().any(|e| matches!(e, HostEvent::Action(..))));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HostEvent::Log(_, l) if l.contains("delete_own")))
    );
}

#[test]
fn sandbox_blocks_system_access_and_api_tampering() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "probe", permissions = { "read" } }
        telega.on("message_new", function()
            telega.log("require", tostring(require), "loadstring", tostring(loadstring))
            telega.log("load", tostring(load), "dofile", tostring(dofile))
            telega.log("loadfile", tostring(loadfile), "getfenv", tostring(getfenv))
            telega.log("setfenv", tostring(setfenv))
            telega.log("tamper", pcall(function() telega.delete = nil end))
        end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("probe", src)],
        &[("probe", config(true, false, json!({})))],
    );
    host.handle(own_message(5, 10), 0.0);
    let logs = logs(&mut host);
    // Every name the host clears from globals (host.rs), not just the two
    // that happen to have no Luau stdlib equivalent to fall back on.
    assert_eq!(logs[0], "require nil loadstring nil");
    assert_eq!(logs[1], "load nil dofile nil");
    assert_eq!(logs[2], "loadfile nil getfenv nil");
    assert_eq!(logs[3], "setfenv nil");
    assert!(logs[4].starts_with("tamper false"), "{logs:?}");
}

#[test]
fn runaway_handler_is_stopped_and_host_keeps_working() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "loop", permissions = { "read" } }
        telega.on("message_new", function(m)
            if m.id == 1 then while true do end end
            telega.log("ok", m.id)
        end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("loop", src)],
        &[("loop", config(true, false, json!({})))],
    );
    host.handle(own_message(5, 1), 0.0);
    host.handle(own_message(5, 2), 0.0);
    let logs = logs(&mut host);
    assert!(logs[0].contains("превышено время"), "{logs:?}");
    assert_eq!(logs[1], "ok 2");
}

#[test]
fn actions_are_paced_and_flood_wait_pauses_them() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "burst", permissions = { "read", "delete_own" } }
        telega.on("message_new", function(m) telega.delete(m.chat_id, { m.id }) end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("burst", src)],
        &[("burst", config(true, false, json!({})))],
    );
    for id in 1..=3 {
        host.handle(own_message(5, id), 10.0);
    }
    host.tick(10.0);
    assert_eq!(actions(&mut host).len(), 1);
    host.tick(10.2);
    assert!(actions(&mut host).is_empty());
    host.tick(10.5);
    assert_eq!(actions(&mut host).len(), 1);

    host.handle(HostCmd::FloodWait(origin(), 30), 10.6);
    host.tick(20.0);
    assert!(actions(&mut host).is_empty());
    assert_eq!(host.next_wakeup(20.0), Some(40.6));
    host.tick(40.6);
    assert_eq!(actions(&mut host).len(), 1);
}

#[test]
fn broken_and_duplicate_plugins_are_listed_with_errors() {
    let host = Host::new(
        vec![
            autodelete(),
            plugin("autodelete", "telega.plugin { name = \"copy\" }"),
            plugin("syntax", "telega.plugin {"),
            plugin("nomanifest", "local x = 1"),
        ],
        0.0,
    );
    let mut host = host;
    let Some(HostEvent::Plugins(infos)) = host.drain().into_iter().next() else {
        panic!("no plugin list");
    };
    let errors: Vec<(&str, bool)> = infos
        .iter()
        .map(|i| (i.id.as_str(), i.error.is_some()))
        .collect();
    assert_eq!(
        errors,
        [
            ("autodelete", false),
            ("autodelete", true),
            ("syntax", true),
            ("nomanifest", true)
        ]
    );
}

#[test]
fn store_persists_per_plugin() {
    let dir = TempDir::new();
    let src = |name: &str| {
        format!(
            r#"
            telega.plugin {{ name = "{name}", permissions = {{ "read" }} }}
            telega.on("message_new", function(m)
                local n = (telega.store_get("count") or 0) + 1
                telega.store_set("count", n)
                telega.log(n)
            end)
        "#
        )
    };
    let sources = || vec![plugin("a", &src("a")), plugin("b", &src("b"))];
    let on = config(true, false, json!({}));
    let configs = [("a", on.clone()), ("b", on)];
    let mut first = host(&dir, sources(), &configs);
    first.handle(own_message(5, 1), 0.0);
    first.handle(own_message(5, 2), 0.0);
    assert_eq!(logs(&mut first), ["1", "1", "2", "2"]);
    drop(first);
    let mut second = host(&dir, sources(), &configs);
    second.handle(own_message(5, 3), 0.0);
    assert_eq!(logs(&mut second), ["3", "3"]);
}

#[test]
fn precompiled_bytecode_is_refused() {
    let mut host = Host::new(vec![plugin("bc", "\x05\x03anything")], 0.0);
    let Some(HostEvent::Plugins(infos)) = host.drain().into_iter().next() else {
        panic!("no plugin list");
    };
    assert!(
        infos[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("байткод"))
    );
}

#[test]
fn deeply_nested_or_reentrant_values_fail_without_crashing() {
    let dir = TempDir::new();
    let hostile = r#"
        -- Reading the manifest calls back into the API.
        telega.plugin(setmetatable({ permissions = { "read" } }, {
            __index = function(_, k) telega.log("probe " .. tostring(k)) end,
        }))
        telega.on("message_new", function(m)
            local t = {}
            for i = 1, 100000 do t = { t } end
            telega.after(1, "x", t)
        end)
    "#;
    let on = config(true, false, json!({}));
    let mut host = host(
        &dir,
        vec![plugin("hostile", hostile), autodelete()],
        &[("hostile", on), ("autodelete", autodelete_on(true))],
    );
    host.handle(own_message(5, 1), 0.0);
    let lines = logs(&mut host);
    assert!(lines.iter().any(|l| l.contains("вложенность")), "{lines:?}");
    // The thread survived and the other plugin still works.
    assert!(
        lines.iter().any(|l| l.contains("пробный прогон")) || {
            host.tick(120.0);
            logs(&mut host).iter().any(|l| l.contains("пробный прогон"))
        }
    );
}

#[test]
fn self_rescheduling_task_runs_once_per_tick() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "loop", permissions = { "read" } }
        telega.on_task("again", function()
            telega.log("run")
            telega.after(0, "again")
        end)
        telega.on("message_new", function() telega.after(0, "again") end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("loop", src)],
        &[("loop", config(true, false, json!({})))],
    );
    host.handle(own_message(5, 1), 0.0);
    host.tick(1.0);
    assert_eq!(logs(&mut host), ["run"]);
    // A zero delay is raised to the minimum, so the next run is not due yet.
    host.tick(1.5);
    assert!(logs(&mut host).is_empty());
    host.tick(2.0);
    assert_eq!(logs(&mut host), ["run"]);
}

#[test]
fn switching_to_dry_run_drops_queued_actions() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "burst", permissions = { "read", "delete_own" } }
        telega.on("message_new", function(m)
            for i = 1, 5 do telega.delete(m.chat_id, { m.id + i }) end
        end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("burst", src)],
        &[("burst", config(true, false, json!({})))],
    );
    host.handle(own_message(5, 1), 0.0);
    host.tick(0.0);
    assert_eq!(
        actions(&mut host).len(),
        1,
        "the first goes out, the rest waits"
    );
    host.handle(
        HostCmd::Configure([("burst".into(), config(true, true, json!({})))].into()),
        0.0,
    );
    host.tick(100.0);
    assert!(actions(&mut host).is_empty());
}

#[test]
fn per_call_limits_are_enforced() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "greedy", permissions = { "read", "send", "delete_own" } }
        for i = 1, 16 do telega.on("message_new", function() end) end
        local ok = pcall(telega.on, "message_new", function() end)
        telega.on("message_edited", function() end)
        telega.on("message_deleted", function(m)
            telega.log("handlers capped: " .. tostring(not ok))
            telega.log("long send refused: " .. tostring(not pcall(telega.send, 1, string.rep("я", 4097))))
            local sent = 0
            for i = 1, 30 do if pcall(telega.send, 1, "x") then sent = sent + 1 end end
            telega.log("sent " .. sent)
            for i = 1, 1000 do telega.log("flood") end
        end)
    "#;
    let mut host = host(
        &dir,
        vec![plugin("greedy", src)],
        &[("greedy", config(true, false, json!({})))],
    );
    host.handle(
        HostCmd::Event(
            origin(),
            Event::Deleted {
                chat_id: 1,
                ids: vec![1],
            },
        ),
        0.0,
    );
    let lines = logs(&mut host);
    assert_eq!(
        lines[..3],
        [
            "handlers capped: true",
            "long send refused: true",
            "sent 20"
        ]
    );
    assert!(lines.len() <= MAX_LOGS_PER_CALL + 2, "{}", lines.len());
    assert!(lines.last().unwrap().contains("пропущено"));
}

#[test]
fn plugin_stays_off_without_all_granted_permissions() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "greedy", permissions = { "read", "delete_own" } }
        telega.on("message_new", function(m) telega.delete(m.chat_id, { m.id }) end)
    "#;
    // enabled=true, but the user only ever agreed to "read": the host must
    // not run the plugin at all, regardless of the `enabled` flag it got.
    let mut only_read = config(true, false, json!({}));
    only_read.granted = vec!["read".into()];
    let mut host = host(&dir, vec![plugin("greedy", src)], &[("greedy", only_read)]);
    host.handle(own_message(5, 10), 0.0);
    host.tick(100.0);
    assert!(
        host.drain().is_empty(),
        "a plugin missing a granted permission must not even run its \"read\" handler"
    );
}

#[test]
fn tasks_of_a_plugin_that_failed_to_load_are_kept() {
    let dir = TempDir::new();
    let src = r#"
        telega.plugin { name = "flaky", permissions = { "read", "delete_own" } }
        telega.on("message_new", function(m)
            telega.after(1, "gone", { chat_id = m.chat_id, id = m.id })
        end)
        telega.on_task("gone", function(p) telega.delete(p.chat_id, { p.id }) end)
    "#;
    let configs = [("flaky", config(true, false, json!({})))];
    let mut first = host(&dir, vec![plugin("flaky", src)], &configs);
    first.handle(own_message(5, 10), 0.0);
    drop(first);

    // The file now has a syntax error, as if caught mid-edit; its already
    // scheduled task must not be wiped just because it fails to load once.
    host(&dir, vec![plugin("flaky", "telega.plugin {")], &configs);

    // Fixed again: the task scheduled before the breakage still runs.
    let mut fixed = host(&dir, vec![plugin("flaky", src)], &configs);
    fixed.tick(1000.0);
    assert_eq!(
        actions(&mut fixed).len(),
        1,
        "a task must survive its plugin briefly failing to load"
    );
}

mod security_probes {
    use super::*;
    use std::time::Instant;

    const ACTION_LUA: &str = r#"
        telega.plugin { name = "audit", permissions = { "read", "delete_own" } }
        telega.on("message_new", function(m) telega.delete(m.chat_id, { m.id }) end)
    "#;

    fn emitted_action(host: &mut Host) -> Vec<HostEvent> {
        host.handle(own_message(5, 123), 0.0);
        host.tick(0.0);
        host.drain()
    }

    #[test]
    #[ignore = "explicit local security audit probe"]
    fn security_probe_drained_action_survives_dry_run_and_account_change() {
        for account_change in [false, true] {
            let dir = TempDir::new();
            let mut host = host(
                &dir,
                vec![plugin("audit", ACTION_LUA)],
                &[("audit", config(true, false, json!({})))],
            );
            let already_drained = emitted_action(&mut host);
            let emitted = already_drained
                .iter()
                .filter(|e| matches!(e, HostEvent::Action(..)))
                .count();
            if account_change {
                host.handle(HostCmd::Account(None), 0.1);
            } else {
                host.handle(
                    HostCmd::Configure([("audit".into(), config(true, true, json!({})))].into()),
                    0.1,
                );
            }
            host.tick(1.0);
            let after = host.drain();
            let later_actions = after
                .iter()
                .filter(|e| matches!(e, HostEvent::Action(..)))
                .count();
            let previously_emitted_still_owned = already_drained
                .iter()
                .any(|e| matches!(e, HostEvent::Action(..)));
            println!(
                "security_probe_drained_action: account_removed={} emitted_before_policy={} emitted_after_policy={} prior_event_still_owned={}",
                account_change, emitted, later_actions, previously_emitted_still_owned
            );
            assert_eq!(emitted, 1);
            assert_eq!(later_actions, 0);
            assert!(previously_emitted_still_owned);
        }
    }

    #[test]
    #[ignore = "explicit local security audit probe"]
    fn security_probe_lua_timeout_and_bounded_message_storm() {
        let dir = TempDir::new();
        let source = r#"
            telega.plugin { name = "audit", permissions = { "read" } }
            telega.on("message_new", function(m)
                if m.id == 1 then while true do end end
                telega.log("processed")
            end)
        "#;
        let mut host = host(
            &dir,
            vec![plugin("audit", source)],
            &[("audit", config(true, false, json!({})))],
        );
        let started = Instant::now();
        host.handle(own_message(5, 1), 0.0);
        let timed_out = host
            .drain()
            .iter()
            .filter(|e| matches!(e, HostEvent::Log(_, line) if line.contains("превышено время")))
            .count();
        const EVENTS: i64 = 64;
        const BYTES_PER_EVENT: usize = 4 * 1024;
        let mut processed = 0;
        for id in 2..EVENTS + 2 {
            host.handle(
                HostCmd::Event(
                    origin(),
                    Event::New {
                        chat_id: 5,
                        id,
                        text: "x".repeat(BYTES_PER_EVENT),
                        outgoing: true,
                        sender_id: 1,
                    },
                ),
                1.0,
            );
            processed += host
                .drain()
                .iter()
                .filter(|e| matches!(e, HostEvent::Log(_, line) if line == "processed"))
                .count();
        }
        let elapsed = started.elapsed();
        host.handle(
            HostCmd::Configure([("audit".into(), config(false, false, json!({})))].into()),
            2.0,
        );
        host.drain();
        host.handle(own_message(5, EVENTS + 2), 2.0);
        let disabled_output = host.drain().len();
        println!(
            "security_probe_lua_load: timeout_count={} events={} bytes_per_event={} processed={} elapsed_ms={} disabled_output={}",
            timed_out,
            EVENTS,
            BYTES_PER_EVENT,
            processed,
            elapsed.as_millis(),
            disabled_output
        );
        assert_eq!(timed_out, 1);
        assert_eq!(processed, EVENTS as usize);
        assert_eq!(disabled_output, 0);
        assert!(elapsed.as_secs() < 15, "local probe time bound exceeded");
    }

    #[test]
    fn security_fix_stale_host_backlog_does_not_act_on_same_users_new_client() {
        let dir = TempDir::new();
        let src = r#"
            telega.plugin { name = "sender", permissions = { "read", "send" } }
            telega.on("message_new", function(m) telega.send(m.chat_id, "reply") end)
        "#;
        let mut host = host(
            &dir,
            vec![plugin("sender", src)],
            &[("sender", config(true, false, json!({})))],
        );
        host.handle(HostCmd::Account(Some(replacement_origin())), 0.1);
        host.handle(own_message(5, 21), 0.2);
        host.tick(0.2);
        assert!(
            !host
                .drain()
                .iter()
                .any(|e| matches!(e, HostEvent::Action(..))),
            "an event queued before account replacement must not act for its new client"
        );

        host.handle(
            HostCmd::Event(
                replacement_origin(),
                Event::New {
                    chat_id: 5,
                    id: 22,
                    text: "new session".into(),
                    outgoing: false,
                    sender_id: 7,
                },
            ),
            0.3,
        );
        host.tick(0.3);
        assert!(
            host.drain().into_iter().any(|e| matches!(
                e, HostEvent::Action(origin, id, Action::Send { chat_id: 5, .. })
                    if origin == replacement_origin() && id == "sender"
            )),
            "the authorized new client's event should still work"
        );
    }

    #[test]
    fn security_fix_old_client_requeue_is_discarded_after_same_user_replacement() {
        let dir = TempDir::new();
        let mut host = host(
            &dir,
            vec![plugin(
                "sender",
                r#"telega.plugin { name = "sender", permissions = { "send" } }"#,
            )],
            &[("sender", config(true, false, json!({})))],
        );
        host.handle(HostCmd::Account(Some(replacement_origin())), 0.1);
        let action = Action::Send {
            chat_id: 5,
            text: "reply".into(),
        };
        host.handle(
            HostCmd::Requeue(origin(), "sender".into(), action.clone()),
            0.2,
        );
        host.tick(0.2);
        assert!(
            !host
                .drain()
                .iter()
                .any(|e| matches!(e, HostEvent::Action(..))),
            "an old client's failed action must not be retried for the new client"
        );
        host.handle(
            HostCmd::Requeue(replacement_origin(), "sender".into(), action.clone()),
            0.3,
        );
        host.tick(0.3);
        assert!(
            host.drain().into_iter().any(|e| matches!(
                e, HostEvent::Action(current, id, issued)
                    if current == replacement_origin() && id == "sender" && issued == action
            )),
            "a current client's retry must remain eligible"
        );
    }

    #[test]
    fn security_fix_rapid_disable_reenable_cancels_scheduled_delete_through_transport() {
        let dir = TempDir::new();
        let mut host = host(
            &dir,
            vec![autodelete()],
            &[("autodelete", autodelete_on(false))],
        );
        let (handle, receiver) = crate::plugins::HostHandle::for_test();
        // The real transport first observes the enabled state, just as it
        // does at startup; otherwise it cannot know an off transition began.
        handle.send(HostCmd::Configure(
            [("autodelete".into(), autodelete_on(false))].into(),
        ));
        for cmd in receiver.try_iter() {
            host.handle(cmd, 0.0);
        }
        host.handle(own_message(5, 10), 0.0);
        // Both commands arrive while Lua has not read the transport. The
        // disable must not vanish when the final enabled config arrives.
        handle.send(HostCmd::Configure(
            [(
                "autodelete".into(),
                config(false, false, json!({ "chats": [5], "minutes": 1 })),
            )]
            .into(),
        ));
        handle.send(HostCmd::Configure(
            [("autodelete".into(), autodelete_on(false))].into(),
        ));
        for cmd in receiver.try_iter() {
            host.handle(cmd, 0.1);
        }
        host.tick(61.0);
        assert!(
            actions(&mut host).is_empty(),
            "a deletion scheduled before the brief disable must not fire after re-enabling"
        );
        host.handle(own_message(5, 11), 61.0);
        host.tick(121.0);
        assert_eq!(
            actions(&mut host),
            [Action::Delete {
                chat_id: 5,
                ids: vec![11],
                revoke: true,
            }],
            "newly scheduled work should still run after re-enabling"
        );
    }

    #[test]
    fn security_fix_flood_wait_wake_precedes_due_action_and_preserves_retry() {
        let dir = TempDir::new();
        let src = r#"
            telega.plugin { name = "burst", permissions = { "read", "delete_own" } }
            telega.on("message_new", function(m) telega.delete(m.chat_id, { m.id }, { revoke = true }) end)
        "#;
        let mut host = host(
            &dir,
            vec![plugin("burst", src)],
            &[("burst", config(true, false, json!({})))],
        );
        for id in 1..=2 {
            host.handle(own_message(5, id), 0.0);
        }
        host.tick(0.0);
        assert_eq!(
            actions(&mut host),
            [Action::Delete {
                chat_id: 5,
                ids: vec![1],
                revoke: true,
            }]
        );

        let (handle, receiver) = crate::plugins::HostHandle::for_test();
        let retry = Action::Delete {
            chat_id: 5,
            ids: vec![99],
            revoke: true,
        };
        handle.send(HostCmd::FloodWait(origin(), 30));
        handle.send(HostCmd::Requeue(origin(), "burst".into(), retry.clone()));
        // At the exact instant action 2 becomes due, dispatch the wake with
        // the actual worker step. A pre-control tick would emit action 2.
        while receiver.drive_host(&mut host, 0.5) {}
        assert!(
            actions(&mut host).is_empty(),
            "the due send must remain paused when FloodWait wakes the worker"
        );
        assert_eq!(host.next_wakeup(0.5), Some(30.5));
        host.tick(30.5);
        assert_eq!(
            actions(&mut host),
            [retry],
            "the retried action must survive the pause and run first afterward"
        );
    }

    #[test]
    fn security_fix_account_removal_wake_precedes_due_autodelete() {
        let dir = TempDir::new();
        let mut host = host(
            &dir,
            vec![autodelete()],
            &[("autodelete", autodelete_on(false))],
        );
        host.handle(own_message(5, 10), 0.0);
        assert_eq!(host.next_wakeup(0.0), Some(60.0));

        let (handle, receiver) = crate::plugins::HostHandle::for_test();
        handle.send(HostCmd::Account(None));
        // No real clock or thread: the pending delete is already overdue when
        // the account-removal wake is processed by the production worker step.
        while receiver.drive_host(&mut host, 61.0) {}
        assert!(
            actions(&mut host).is_empty(),
            "logging out must prevent an overdue action on the old account"
        );
        host.tick(61.0);
        assert!(
            actions(&mut host).is_empty(),
            "the old account's task cannot fire again without an active account"
        );
    }

    #[test]
    fn security_fix_transport_bounds_event_backlog_without_losing_controls() {
        // Exercise the real HostHandle transport with no worker draining it:
        // ordinary events must not retain an unbounded backlog in Rust memory.
        let (handle, receiver) = crate::plugins::HostHandle::for_test();
        const EVENTS: usize = 512;
        const MAX_PENDING_EVENTS: usize = 256;
        let text = "x".repeat(32 * 1024);
        for id in 0..EVENTS {
            handle.send(HostCmd::Event(
                origin(),
                Event::New {
                    chat_id: 5,
                    id: id as i64,
                    text: text.clone(),
                    outgoing: true,
                    sender_id: 1,
                },
            ));
        }
        // Neither policy changes nor account changes may be stuck behind or
        // silently dropped with the saturated event queue.
        handle.send(HostCmd::Configure(Default::default()));
        handle.send(HostCmd::Account(Some(origin())));
        let commands: Vec<_> = receiver.try_iter().collect();
        let accepted = commands
            .iter()
            .filter(|cmd| matches!(cmd, HostCmd::Event(_, Event::New { .. })))
            .count();
        assert!(
            accepted <= MAX_PENDING_EVENTS,
            "retained {accepted} of {EVENTS} 32-KiB events with no consumer"
        );
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd, HostCmd::Configure(_))),
            "a full event queue must still admit the configuration change"
        );
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd, HostCmd::Account(Some(account)) if *account == origin())),
            "a full event queue must still admit the account change"
        );
    }
}
