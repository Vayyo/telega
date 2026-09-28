//! Plugin host behavior: real Luau VMs and SQLite files in a temp dir, time
//! passed explicitly.

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::plugins::BUILTIN;

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
    host.handle(HostCmd::Account(Some(1)), 0.0);
    host.drain();
    host
}

fn own_message(chat_id: i64, id: i64) -> HostCmd {
    HostCmd::Event(Event::New {
        chat_id,
        id,
        text: "hi".into(),
        outgoing: true,
        sender_id: 1,
    })
}

fn actions(host: &mut Host) -> Vec<Action> {
    host.drain()
        .into_iter()
        .filter_map(|e| match e {
            HostEvent::Action(_, a) => Some(a),
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
        HostCmd::Event(Event::New {
            chat_id: 5,
            id: 12,
            text: "x".into(),
            outgoing: false,
            sender_id: 2,
        }),
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

    host.handle(HostCmd::FloodWait(30), 10.6);
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
        HostCmd::Event(Event::Deleted {
            chat_id: 1,
            ids: vec![1],
        }),
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
