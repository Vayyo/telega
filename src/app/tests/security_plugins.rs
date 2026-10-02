//! Plugin actions crossing App policy and account boundaries; all TDLib tasks
//! are dropped without polling, so these tests send no messages or deletes.

use super::*;
use iced_runtime::task::into_stream;
use std::path::PathBuf;

struct PluginDbDir(PathBuf);

impl PluginDbDir {
    fn new(app: &App) -> Self {
        let path = app.settings_path.with_extension("plugins-test");
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for PluginDbDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Keep the actual event drained from a Lua Host, not a newly invented App
/// action. The Host's account switch/Configure cannot recall this value.
fn emitted_send(app: &mut App) -> HostEvent {
    let dir = PluginDbDir::new(app);
    let source = r#"
        telega.plugin { name = "Sender", permissions = { "read", "send" } }
        telega.on("message_new", function(m) telega.send(m.chat_id, "queued message") end)
    "#;
    let mut host =
        plugins::Host::new(vec![("sender".into(), source.into(), false)], 0.0).with_db_dir(&dir.0);
    for event in host.drain() {
        let _ = app.update(Msg::Plugin(event));
    }
    let _ = app.update(Msg::PluginToggle("sender".into(), true));
    let _ = app.update(Msg::PluginDryRun("sender".into(), false));
    let origin = plugin_origin(app);
    let config = app.settings.plugins["sender"].clone();
    host.handle(HostCmd::Configure([("sender".into(), config)].into()), 0.0);
    host.handle(HostCmd::Account(Some(origin)), 0.0);
    host.drain();
    host.handle(
        HostCmd::Event(
            origin,
            plugins::Event::New {
                chat_id: 17,
                id: 21,
                text: "sample".into(),
                outgoing: false,
                sender_id: 7,
            },
        ),
        0.0,
    );
    host.tick(0.0);
    host.drain()
        .into_iter()
        .find(|event| matches!(event, HostEvent::Action(..)))
        .expect("enabled Lua sender must emit its action before policy changes")
}

#[test]
fn security_fix_dry_run_rejects_an_action_already_emitted_by_lua() {
    let mut app = app();
    app.session.my_id = Some(5);
    let rx = with_plugin_host(&mut app);
    sent(&rx);
    let emitted = emitted_send(&mut app);
    sent(&rx);
    assert!(
        into_stream(app.update(Msg::Plugin(emitted.clone()))).is_some(),
        "the same action is eligible before dry run is enabled"
    );

    let _ = app.update(Msg::PluginDryRun("sender".into(), true));
    assert!(
        sent(&rx).iter().any(|cmd| matches!(cmd,
            HostCmd::Configure(configs) if configs["sender"].dry_run
        )),
        "the policy change must reach the plugin Host"
    );
    assert!(
        into_stream(app.update(Msg::Plugin(emitted))).is_none(),
        "a previously emitted send must not enqueue a TDLib request in dry run"
    );
}

#[test]
fn security_fix_dry_run_rejects_delayed_delete_own_without_marking_messages_deleted() {
    let mut app = app();
    app.session.my_id = Some(5);
    let rx = with_plugin_host(&mut app);
    sent(&rx);
    let info = PluginInfo {
        id: "cleanup".into(),
        name: "Cleanup".into(),
        description: String::new(),
        permissions: vec![plugins::Permission::DeleteOwn],
        settings: Vec::new(),
        builtin: false,
        error: None,
    };
    let _ = app.update(Msg::Plugin(HostEvent::Plugins(vec![info])));
    let _ = app.update(Msg::PluginToggle("cleanup".into(), true));
    let _ = app.update(Msg::PluginDryRun("cleanup".into(), false));
    let action = plugins::Action::Delete {
        chat_id: 17,
        ids: vec![21],
        revoke: true,
    };
    // Ownership lookup completed while the plugin was permitted to delete;
    // the user's policy changes before that asynchronous reply reaches App.
    let _ = app.update(Msg::PluginDryRun("cleanup".into(), true));
    sent(&rx);
    let origin = plugin_origin(&app);
    let task = app.update(Msg::PluginDeleteOwn(
        origin,
        "cleanup".into(),
        17,
        vec![21],
        true,
        action,
    ));
    assert!(
        into_stream(task).is_none(),
        "the delayed result must not queue an actual TDLib deletion"
    );
    assert!(
        !app.session.self_deleted.contains(&(17, 21)),
        "the message must still be treated as ordinary, not self-deleted"
    );
    assert!(
        !sent(&rx)
            .iter()
            .any(|cmd| matches!(cmd, HostCmd::Requeue(..))),
        "dry run must not requeue the disallowed deletion"
    );
}

#[test]
fn security_fix_same_user_new_client_rejects_old_host_action() {
    let mut app = app();
    app.session.client_id = 100;
    app.session.my_id = Some(5);
    let _rx = with_plugin_host(&mut app);
    let old_client_event = emitted_send(&mut app);
    assert!(
        into_stream(app.update(Msg::Plugin(old_client_event.clone()))).is_some(),
        "the event was valid for the original authorized client"
    );
    // A fresh TDLib client can log into the *same* user. User-id comparison
    // alone cannot make the old Host event safe to execute in this session.
    app.session.client_id = 101;
    assert_eq!(app.session.my_id, Some(5));
    assert!(matches!(app.session.auth, Auth::Ready));
    assert!(
        into_stream(app.update(Msg::Plugin(old_client_event))).is_none(),
        "an event emitted by the old client must not enqueue work on its replacement"
    );
}

#[test]
fn security_fix_not_ready_session_rejects_emitted_host_action() {
    let mut app = app();
    app.session.my_id = Some(5);
    let emitted = emitted_send(&mut app);
    app.session.auth = Auth::Phone;
    assert!(
        into_stream(app.update(Msg::Plugin(emitted))).is_none(),
        "a logged-out or not-yet-authorized client must not enqueue plugin work"
    );
}

#[test]
fn security_fix_old_client_delete_continuation_cannot_delete_same_users_new_message() {
    let mut app = app();
    app.session.my_id = Some(5);
    allow_plugin_delete(&mut app);
    let old_origin = plugin_origin(&app);
    app.session.client_id += 1;
    let action = plugins::Action::Delete {
        chat_id: 17,
        ids: vec![21],
        revoke: true,
    };
    let task = app.update(Msg::PluginDeleteOwn(
        old_origin,
        "autodelete".into(),
        17,
        vec![21],
        true,
        action,
    ));
    assert!(
        into_stream(task).is_none(),
        "an old client's ownership answer must not request deletion on a new client"
    );
    assert!(
        !app.session.self_deleted.contains(&(17, 21)),
        "an old client's continuation must not mark the new message self-deleted"
    );
}

#[test]
fn security_fix_old_client_delete_failure_cannot_requeue_or_roll_back_current_state() {
    let mut app = app();
    app.session.my_id = Some(5);
    let rx = with_plugin_host(&mut app);
    allow_plugin_delete(&mut app);
    sent(&rx);
    let old_origin = plugin_origin(&app);
    app.session.client_id += 1;
    app.session.self_deleted.insert((17, 21));
    let action = plugins::Action::Delete {
        chat_id: 17,
        ids: vec![21],
        revoke: true,
    };
    let _ = app.update(Msg::PluginDeleted(
        old_origin,
        "autodelete".into(),
        17,
        vec![21],
        action,
        Err("Too Many Requests: retry after 30 (429)".into()),
    ));
    assert!(
        app.session.self_deleted.contains(&(17, 21)),
        "the old client's failed deletion must not roll back the new client's marker"
    );
    assert!(
        !sent(&rx)
            .iter()
            .any(|cmd| matches!(cmd, HostCmd::FloodWait(..) | HostCmd::Requeue(..))),
        "an old client's failed action must not pause or requeue work for the new client"
    );
    assert!(
        app.plugin_logs
            .get("autodelete")
            .is_none_or(|log| log.is_empty()),
        "an old client's completion must not add an error to the current account's journal"
    );
}
