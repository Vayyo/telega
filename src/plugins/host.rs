//! Plugin host: loads plugins into sandboxed Luau VMs, delivers events, runs
//! persistent delayed tasks and paces the resulting actions. Pure with respect
//! to time (`now` is passed in) so it is testable without threads or clocks.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value, VmState};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Action, Event, HostCmd, HostEvent, Permission, PluginInfo, SettingKind, SettingSpec};
use crate::settings::PluginConfig;

/// Time budget of one callback (loading, event, task).
const CALL_BUDGET: Duration = Duration::from_millis(200);
const MEMORY_LIMIT: usize = 32 * 1024 * 1024;
/// Minimal pause between two actions of all plugins together.
const ACTION_INTERVAL: f64 = 0.5;
const MAX_IDS_PER_DELETE: usize = 100;
/// Journal lines kept from one call and their length; the rest is counted.
const MAX_LOGS_PER_CALL: usize = 20;
const MAX_LOG_LINE: usize = 500;
/// Per plugin: scheduled tasks and size of one stored value / task payload.
const MAX_TASKS: i64 = 10_000;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_STORE_KEYS: i64 = 1_000;
/// Nesting of tables passed to `after` / `store_set`: deep structures would
/// overflow the thread stack during conversion and abort the process.
const MAX_DEPTH: usize = 32;
/// Handlers one plugin may register for one event.
const MAX_HANDLERS: usize = 16;
/// Actions one call may request and one plugin may have queued.
const MAX_ACTIONS_PER_CALL: usize = 20;
const MAX_QUEUED_PER_PLUGIN: usize = 1_000;
/// Tasks run by one tick; the rest waits for the next one.
const MAX_TASKS_PER_TICK: usize = 100;
/// Shortest delay of `telega.after`, so a task cannot reschedule itself into
/// the same tick forever.
const MIN_DELAY: f64 = 1.0;
/// Telegram's message length limit.
const MAX_SEND_CHARS: usize = 4096;

/// Data shared by all plugin VMs: the per-account database.
struct Shared {
    db: RefCell<Option<Connection>>,
    /// Time of the current call, for `telega.after`.
    now: Cell<f64>,
}

/// State one VM's `telega.*` functions work on.
struct Ctx {
    id: String,
    manifest: Option<PluginInfo>,
    config: PluginConfig,
    handlers: HashMap<String, Vec<Function>>,
    tasks: HashMap<String, Function>,
    /// Filled during a call, drained by the host afterwards.
    actions: Vec<Action>,
    logs: Vec<String>,
    /// Lines over `MAX_LOGS_PER_CALL` in the current call.
    dropped_logs: usize,
}

impl Ctx {
    /// Journal line with the per-call caps applied.
    fn log(&mut self, line: String) {
        if self.logs.len() >= MAX_LOGS_PER_CALL {
            self.dropped_logs += 1;
            return;
        }
        self.logs.push(line.chars().take(MAX_LOG_LINE).collect());
    }

    fn act(&mut self, action: Action) -> mlua::Result<()> {
        if self.actions.len() >= MAX_ACTIONS_PER_CALL {
            return Err(mlua::Error::runtime(format!(
                "не больше {MAX_ACTIONS_PER_CALL} действий за один вызов"
            )));
        }
        self.actions.push(action);
        Ok(())
    }

    fn allows(&self, permission: Permission) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|m| m.permissions.contains(&permission))
    }

    fn require(&self, permission: Permission, what: &str) -> mlua::Result<()> {
        if self.allows(permission) {
            Ok(())
        } else {
            Err(mlua::Error::runtime(format!(
                "{what}: плагин не объявил право \"{}\"",
                permission.name()
            )))
        }
    }
}

struct Plugin {
    lua: Lua,
    ctx: Rc<RefCell<Ctx>>,
    deadline: Rc<Cell<Instant>>,
    info: PluginInfo,
}

pub struct Host {
    plugins: Vec<Plugin>,
    shared: Rc<Shared>,
    queue: VecDeque<(String, Action)>,
    next_action_at: f64,
    paused_until: f64,
    out: Vec<HostEvent>,
    /// Every plugin id seen at startup, loaded or not: tasks of a plugin
    /// that merely failed to load are kept, unlike ones truly gone.
    known_ids: Vec<String>,
    /// Where per-account plugin databases live.
    db_dir: std::path::PathBuf,
}

impl Host {
    /// `sources`: (id, Lua source, built-in). Plugins with duplicate ids or
    /// load errors are listed with an error and stay inactive.
    pub fn new(sources: Vec<(String, String, bool)>, now: f64) -> Self {
        let shared = Rc::new(Shared {
            db: RefCell::new(None),
            now: Cell::new(now),
        });
        let mut plugins: Vec<Plugin> = Vec::new();
        let mut infos = Vec::new();
        let mut known_ids = Vec::new();
        for (id, source, builtin) in sources {
            known_ids.push(id.clone());
            let result = if plugins.iter().any(|p| p.info.id == id) {
                Err(format!("плагин с id \"{id}\" уже загружен"))
            } else {
                load(&id, &source, builtin, &shared)
            };
            match result {
                Ok(plugin) => {
                    infos.push(plugin.info.clone());
                    plugins.push(plugin);
                }
                Err(e) => infos.push(PluginInfo {
                    id,
                    name: String::new(),
                    description: String::new(),
                    permissions: Vec::new(),
                    settings: Vec::new(),
                    builtin,
                    error: Some(e),
                }),
            }
        }
        Self {
            plugins,
            shared,
            queue: VecDeque::new(),
            next_action_at: 0.0,
            paused_until: 0.0,
            out: vec![HostEvent::Plugins(infos)],
            known_ids,
            db_dir: crate::paths::base().to_owned(),
        }
    }

    #[cfg(test)]
    pub fn with_db_dir(mut self, dir: &std::path::Path) -> Self {
        self.db_dir = dir.to_owned();
        self
    }

    pub fn drain(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut self.out)
    }

    pub fn handle(&mut self, cmd: HostCmd, now: f64) {
        match cmd {
            HostCmd::Configure(configs) => self.configure(configs),
            HostCmd::Account(account) => self.set_account(account),
            HostCmd::Event(event) => self.deliver(&event, now),
            HostCmd::FloodWait(secs) => {
                // A short wait that arrives after a longer one must not cut
                // the longer pause short.
                self.paused_until = self.paused_until.max(now + secs as f64);
                self.out.push(HostEvent::Log(
                    String::new(),
                    format!("Telegram просит подождать {secs} с; действия плагинов приостановлены"),
                ));
            }
            HostCmd::Requeue(id, action) => self.queue.push_front((id, action)),
        }
    }

    fn configure(&mut self, mut configs: BTreeMap<String, PluginConfig>) {
        for plugin in &self.plugins {
            let mut config = configs.remove(&plugin.info.id).unwrap_or_default();
            // The client may not have revoked an over-broad grant yet (e.g.
            // right after login, before `Account` is processed): the host
            // never runs a plugin missing a permission it declares, no
            // matter what `enabled` it was sent.
            if config.enabled
                && plugin
                    .info
                    .permissions
                    .iter()
                    .any(|p| !config.granted.iter().any(|g| g == p.name()))
            {
                config.enabled = false;
            }
            let was_enabled = plugin.ctx.borrow().config.enabled;
            if was_enabled && !config.enabled {
                // Turning a plugin off cancels what it planned: no surprise
                // burst of old deletions when it is turned on again later.
                if let Some(db) = self.shared.db.borrow().as_ref() {
                    let _ = db.execute("DELETE FROM tasks WHERE plugin = ?1", [&plugin.info.id]);
                }
            }
            plugin.ctx.borrow_mut().config = config;
        }
        // Queued actions of plugins now off or in dry run are dropped: the
        // switch must take effect for what is already waiting too.
        let before = self.queue.len();
        self.queue.retain(|(id, _)| {
            self.plugins.iter().any(|p| {
                let ctx = p.ctx.borrow();
                &p.info.id == id && ctx.config.enabled && !ctx.config.dry_run
            })
        });
        let dropped = before - self.queue.len();
        if dropped > 0 {
            self.out.push(HostEvent::Log(
                String::new(),
                format!("отменено {dropped} ожидавших действий плагинов"),
            ));
        }
    }

    /// Tasks of plugins that are off are removed whenever an account
    /// database is opened, so an account used later gets no burst either.
    fn purge_disabled_tasks(&self) {
        let db = self.shared.db.borrow();
        let Some(db) = db.as_ref() else { return };
        for plugin in &self.plugins {
            if !plugin.enabled() {
                let _ = db.execute("DELETE FROM tasks WHERE plugin = ?1", [&plugin.info.id]);
            }
        }
        // Tasks of plugins that no longer exist at all (unlike ones that
        // are merely broken right now: `known_ids` still lists those, so
        // fixing the file later finds their tasks still there).
        if let Ok(mut stmt) = db.prepare("SELECT DISTINCT plugin FROM tasks") {
            let stale: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map(|rows| {
                    rows.flatten()
                        .filter(|p| !self.known_ids.contains(p))
                        .collect()
                })
                .unwrap_or_default();
            for id in stale {
                let _ = db.execute("DELETE FROM tasks WHERE plugin = ?1", [&id]);
            }
        }
    }

    fn set_account(&mut self, account: Option<i64>) {
        let db = account.and_then(|id| {
            let conn = Connection::open(self.db_dir.join(format!("plugins-{id}.sqlite")));
            match conn.and_then(|c| init_db(&c).map(|()| c)) {
                Ok(c) => Some(c),
                Err(e) => {
                    self.out
                        .push(HostEvent::Log(String::new(), format!("база плагинов: {e}")));
                    None
                }
            }
        });
        *self.shared.db.borrow_mut() = db;
        // Actions belong to the previous account.
        self.queue.clear();
        self.purge_disabled_tasks();
    }

    fn deliver(&mut self, event: &Event, now: f64) {
        self.shared.now.set(now);
        for i in 0..self.plugins.len() {
            if !self.plugins[i].enabled() {
                continue;
            }
            let handlers = self.plugins[i]
                .ctx
                .borrow()
                .handlers
                .get(event.name())
                .cloned()
                .unwrap_or_default();
            for handler in handlers {
                self.call(i, |lua| handler.call::<()>(lua.to_value(event)?));
            }
        }
    }

    /// Runs due tasks and releases paced actions.
    pub fn tick(&mut self, now: f64) {
        self.shared.now.set(now);
        // Only tasks that existed when the tick started, and a bounded number
        // of them: commands (like "stop all") are handled between ticks.
        let last = self
            .shared
            .db
            .borrow()
            .as_ref()
            .and_then(|db| {
                db.query_row("SELECT MAX(rowid) FROM tasks", [], |r| {
                    r.get::<_, Option<i64>>(0)
                })
                .ok()
                .flatten()
            })
            .unwrap_or(0);
        for _ in 0..MAX_TASKS_PER_TICK {
            let due = {
                let db = self.shared.db.borrow();
                let Some(db) = db.as_ref() else { break };
                db.query_row(
                    "SELECT rowid, plugin, task, payload FROM tasks
                     WHERE due <= ?1 AND rowid <= ?2 ORDER BY due LIMIT 1",
                    params![now, last],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()
                .unwrap_or(None)
            };
            let Some((rowid, plugin_id, task, payload)) = due else {
                break;
            };
            if let Some(db) = self.shared.db.borrow().as_ref() {
                let _ = db.execute("DELETE FROM tasks WHERE rowid = ?1", [rowid]);
            }
            let Some(i) = self
                .plugins
                .iter()
                .position(|p| p.info.id == plugin_id && p.enabled())
            else {
                continue;
            };
            let handler = self.plugins[i].ctx.borrow().tasks.get(&task).cloned();
            let Some(handler) = handler else {
                self.log(i, format!("нет обработчика задачи \"{task}\""));
                continue;
            };
            let payload: serde_json::Value =
                serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
            let lua_payload = self.plugins[i].lua.to_value(&payload);
            self.call(i, |_| handler.call::<()>(lua_payload?));
        }

        while now >= self.next_action_at.max(self.paused_until) {
            let Some((id, action)) = self.queue.pop_front() else {
                break;
            };
            let allowed = self.plugins.iter().any(|p| {
                let ctx = p.ctx.borrow();
                p.info.id == id && ctx.config.enabled && !ctx.config.dry_run
            });
            if allowed {
                self.out.push(HostEvent::Action(id, action));
                self.next_action_at = now + ACTION_INTERVAL;
            }
        }
    }

    /// When `tick` should run next, if anything is waiting.
    pub fn next_wakeup(&self, now: f64) -> Option<f64> {
        let task = self.shared.db.borrow().as_ref().and_then(|db| {
            db.query_row("SELECT MIN(due) FROM tasks", [], |r| {
                r.get::<_, Option<f64>>(0)
            })
            .ok()
            .flatten()
        });
        let action = (!self.queue.is_empty()).then(|| self.next_action_at.max(self.paused_until));
        match (task, action) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
        .map(|t| t.max(now))
    }

    fn log(&mut self, i: usize, line: String) {
        self.out
            .push(HostEvent::Log(self.plugins[i].info.id.clone(), line));
    }

    /// Runs Lua code of plugin `i` under its time budget, then collects the
    /// actions and log lines it produced.
    fn call(&mut self, i: usize, f: impl FnOnce(&Lua) -> mlua::Result<()>) {
        let plugin = &self.plugins[i];
        plugin.deadline.set(Instant::now() + CALL_BUDGET);
        // mlua resumes panics raised inside callbacks; one broken plugin must
        // not take down the plugin thread (and every other plugin with it).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&plugin.lua)));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => self.log(i, format!("ошибка: {e}")),
            Err(_) => self.log(i, "ошибка: внутренний сбой плагина".to_owned()),
        }
        let plugin = &self.plugins[i];
        let (actions, logs, dropped) = {
            let mut ctx = plugin.ctx.borrow_mut();
            (
                std::mem::take(&mut ctx.actions),
                std::mem::take(&mut ctx.logs),
                std::mem::take(&mut ctx.dropped_logs),
            )
        };
        let id = plugin.info.id.clone();
        for line in logs {
            self.out.push(HostEvent::Log(id.clone(), line));
        }
        if dropped > 0 {
            self.out.push(HostEvent::Log(
                id.clone(),
                format!("… ещё {dropped} строк журнала пропущено"),
            ));
        }
        let queued = self.queue.iter().filter(|(q, _)| *q == id).count();
        let room = MAX_QUEUED_PER_PLUGIN.saturating_sub(queued);
        if actions.len() > room {
            self.out.push(HostEvent::Log(
                id.clone(),
                format!(
                    "… {} действий отброшено: слишком много в очереди",
                    actions.len() - room
                ),
            ));
        }
        for action in actions.into_iter().take(room) {
            self.queue.push_back((id.clone(), action));
        }
    }
}

impl Plugin {
    fn enabled(&self) -> bool {
        self.ctx.borrow().config.enabled
    }
}

fn init_db(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS tasks (
             plugin  TEXT NOT NULL,
             due     REAL NOT NULL,
             task    TEXT NOT NULL,
             payload TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS tasks_due ON tasks (due);
         CREATE INDEX IF NOT EXISTS tasks_plugin ON tasks (plugin);
         CREATE TABLE IF NOT EXISTS store (
             plugin TEXT NOT NULL,
             key    TEXT NOT NULL,
             value  TEXT NOT NULL,
             PRIMARY KEY (plugin, key)
         ) WITHOUT ROWID;",
    )
}

fn load(id: &str, source: &str, builtin: bool, shared: &Rc<Shared>) -> Result<Plugin, String> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).map_err(|e| e.to_string())?;
    lua.set_memory_limit(MEMORY_LIMIT)
        .map_err(|e| e.to_string())?;
    let deadline = Rc::new(Cell::new(Instant::now() + CALL_BUDGET));
    let interrupt_deadline = deadline.clone();
    lua.set_interrupt(move |_| {
        if Instant::now() > interrupt_deadline.get() {
            Err(mlua::Error::runtime("превышено время выполнения"))
        } else {
            Ok(VmState::Continue)
        }
    });
    let ctx = Rc::new(RefCell::new(Ctx {
        id: id.to_owned(),
        manifest: None,
        config: PluginConfig::default(),
        handlers: HashMap::new(),
        tasks: HashMap::new(),
        actions: Vec::new(),
        logs: Vec::new(),
        dropped_logs: 0,
    }));
    let telega = api_table(&lua, &ctx, shared, builtin).map_err(|e| e.to_string())?;
    let globals = lua.globals();
    globals.set("telega", telega).map_err(|e| e.to_string())?;
    // No code or file loading from plugins (mlua's Luau `require` reads files)
    // and no environment tricks around the sandbox.
    for name in [
        "require",
        "loadstring",
        "load",
        "dofile",
        "loadfile",
        "getfenv",
        "setfenv",
    ] {
        globals.set(name, Value::Nil).map_err(|e| e.to_string())?;
    }
    // From here on globals and `telega` are read-only for the script.
    lua.sandbox(true).map_err(|e| e.to_string())?;
    // Only source text: precompiled Luau bytecode is not verified by the VM
    // and could escape the sandbox.
    if source.as_bytes().first().is_some_and(|&b| b < b'\t') {
        return Err("файл похож на байткод, а не на текст Lua".to_owned());
    }
    lua.load(source)
        .set_name(format!("={id}.lua"))
        .set_mode(mlua::chunk::ChunkMode::Text)
        .exec()
        .map_err(|e| e.to_string())?;
    let info = ctx
        .borrow()
        .manifest
        .clone()
        .ok_or("нет вызова telega.plugin { … }")?;
    Ok(Plugin {
        lua,
        ctx,
        deadline,
        info,
    })
}

/// Mutable access to a plugin's state from its API functions. Lua code
/// (metamethods) can run while the state is in use; that re-entry becomes a
/// Lua error instead of a panic.
fn ctx_mut(c: &Rc<RefCell<Ctx>>) -> mlua::Result<std::cell::RefMut<'_, Ctx>> {
    c.try_borrow_mut()
        .map_err(|_| mlua::Error::runtime("повторный вызов API во время обработки"))
}

/// Tables handed to Rust stay shallow and small (checked iteratively, cycles
/// included), so converting them cannot overflow the thread stack.
fn check_shape(value: &Value) -> mlua::Result<()> {
    let mut stack = vec![(value.clone(), 0usize)];
    let mut entries = 0usize;
    while let Some((v, depth)) = stack.pop() {
        let Value::Table(table) = v else { continue };
        if depth >= MAX_DEPTH {
            return Err(mlua::Error::runtime(format!(
                "вложенность таблиц больше {MAX_DEPTH}"
            )));
        }
        for pair in table.pairs::<Value, Value>() {
            let (k, v) = pair?;
            entries += 1;
            if entries > 10_000 {
                return Err(mlua::Error::runtime("слишком большая таблица"));
            }
            stack.push((k, depth + 1));
            stack.push((v, depth + 1));
        }
    }
    Ok(())
}

fn api_table(
    lua: &Lua,
    ctx: &Rc<RefCell<Ctx>>,
    shared: &Rc<Shared>,
    builtin: bool,
) -> mlua::Result<Table> {
    let t = lua.create_table()?;

    let c = ctx.clone();
    t.set(
        "plugin",
        lua.create_function(move |_, manifest: Table| {
            // Parsed before borrowing: reading the table may run metamethods.
            let id = c
                .try_borrow()
                .map_err(|_| mlua::Error::runtime("повторный вызов"))?
                .id
                .clone();
            let parsed = parse_manifest(&id, &manifest, builtin)?;
            let mut ctx = ctx_mut(&c)?;
            if ctx.manifest.is_some() {
                return Err(mlua::Error::runtime("telega.plugin вызван повторно"));
            }
            ctx.manifest = Some(parsed);
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "on",
        lua.create_function(move |_, (event, handler): (String, Function)| {
            let mut ctx = ctx_mut(&c)?;
            ctx.require(Permission::Read, "telega.on")?;
            if !matches!(
                event.as_str(),
                "message_new" | "message_edited" | "message_deleted"
            ) {
                return Err(mlua::Error::runtime(format!(
                    "неизвестное событие \"{event}\""
                )));
            }
            let handlers = ctx.handlers.entry(event).or_default();
            if handlers.len() >= MAX_HANDLERS {
                return Err(mlua::Error::runtime(format!(
                    "telega.on: не больше {MAX_HANDLERS} обработчиков на событие"
                )));
            }
            handlers.push(handler);
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "on_task",
        lua.create_function(move |_, (task, handler): (String, Function)| {
            ctx_mut(&c)?.tasks.insert(task, handler);
            Ok(())
        })?,
    )?;

    let (c, s) = (ctx.clone(), shared.clone());
    t.set(
        "after",
        lua.create_function(move |lua, (seconds, task, payload): (f64, String, Value)| {
            if !seconds.is_finite() || seconds < 0.0 {
                return Err(mlua::Error::runtime(
                    "telega.after: seconds должно быть ≥ 0",
                ));
            }
            let seconds = seconds.max(MIN_DELAY);
            if s.db.borrow().is_none() {
                return Err(mlua::Error::runtime("telega.after: нет входа в аккаунт"));
            }
            check_shape(&payload)?;
            let payload: serde_json::Value = lua.from_value(payload)?;
            let payload = payload.to_string();
            if payload.len() > MAX_VALUE_BYTES {
                return Err(mlua::Error::runtime(
                    "telega.after: слишком большие данные задачи",
                ));
            }
            let db = s.db.borrow();
            let db = db
                .as_ref()
                .ok_or_else(|| mlua::Error::runtime("telega.after: нет входа в аккаунт"))?;
            let planned: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE plugin = ?1",
                    [&c.borrow().id],
                    |r| r.get(0),
                )
                .map_err(mlua::Error::external)?;
            if planned >= MAX_TASKS {
                return Err(mlua::Error::runtime(format!(
                    "telega.after: не больше {MAX_TASKS} запланированных задач"
                )));
            }
            db.execute(
                "INSERT INTO tasks (plugin, due, task, payload) VALUES (?1, ?2, ?3, ?4)",
                params![c.borrow().id, s.now.get() + seconds, task, payload],
            )
            .map_err(mlua::Error::external)?;
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "delete",
        lua.create_function(
            move |_, (chat_id, ids, options): (i64, Vec<i64>, Option<Table>)| {
                // Raw read, before borrowing: no metamethods run.
                let revoke = options
                    .map(|o| o.raw_get::<Option<bool>>("revoke"))
                    .transpose()?
                    .flatten()
                    .unwrap_or(false);
                let mut ctx = ctx_mut(&c)?;
                ctx.require(Permission::DeleteOwn, "telega.delete")?;
                if ids.is_empty() || ids.len() > MAX_IDS_PER_DELETE {
                    return Err(mlua::Error::runtime(format!(
                        "telega.delete: от 1 до {MAX_IDS_PER_DELETE} сообщений за вызов"
                    )));
                }
                if ctx.config.dry_run {
                    let line = format!(
                        "пробный прогон: удалил бы {} сообщ. в чате {chat_id} ({})",
                        ids.len(),
                        if revoke { "у всех" } else { "у себя" }
                    );
                    ctx.log(line);
                } else {
                    ctx.act(Action::Delete {
                        chat_id,
                        ids,
                        revoke,
                    })?;
                }
                Ok(())
            },
        )?,
    )?;

    let c = ctx.clone();
    t.set(
        "send",
        lua.create_function(move |_, (chat_id, text): (i64, String)| {
            let mut ctx = ctx_mut(&c)?;
            ctx.require(Permission::Send, "telega.send")?;
            if text.is_empty() || text.chars().count() > MAX_SEND_CHARS {
                return Err(mlua::Error::runtime(format!(
                    "telega.send: от 1 до {MAX_SEND_CHARS} символов"
                )));
            }
            if ctx.config.dry_run {
                ctx.log(format!(
                    "пробный прогон: отправил бы в чат {chat_id}: {text}"
                ));
            } else {
                ctx.act(Action::Send { chat_id, text })?;
            }
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "setting",
        lua.create_function(move |lua, key: String| {
            let ctx = c.borrow();
            let manifest = ctx
                .manifest
                .as_ref()
                .ok_or_else(|| mlua::Error::runtime("telega.setting до telega.plugin"))?;
            if !manifest.settings.iter().any(|s| s.key == key) {
                return Err(mlua::Error::runtime(format!("нет настройки \"{key}\"")));
            }
            lua.to_value(manifest.value(&ctx.config, &key))
        })?,
    )?;

    let (c, s) = (ctx.clone(), shared.clone());
    t.set(
        "store_get",
        lua.create_function(move |lua, key: String| {
            let db = s.db.borrow();
            let Some(db) = db.as_ref() else {
                return Ok(Value::Nil);
            };
            let json: Option<String> = db
                .query_row(
                    "SELECT value FROM store WHERE plugin = ?1 AND key = ?2",
                    params![c.borrow().id, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(mlua::Error::external)?;
            match json {
                Some(json) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&json).map_err(mlua::Error::external)?;
                    lua.to_value(&value)
                }
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    let (c, s) = (ctx.clone(), shared.clone());
    t.set(
        "store_set",
        lua.create_function(move |lua, (key, value): (String, Value)| {
            let db = s.db.borrow();
            let db = db
                .as_ref()
                .ok_or_else(|| mlua::Error::runtime("telega.store_set: нет входа в аккаунт"))?;
            let id = c.borrow().id.clone();
            if value.is_nil() {
                db.execute(
                    "DELETE FROM store WHERE plugin = ?1 AND key = ?2",
                    params![id, key],
                )
            } else {
                check_shape(&value)?;
                let keys: i64 = db
                    .query_row("SELECT COUNT(*) FROM store WHERE plugin = ?1", [&id], |r| {
                        r.get(0)
                    })
                    .map_err(mlua::Error::external)?;
                if keys >= MAX_STORE_KEYS {
                    let exists: bool = db
                        .query_row(
                            "SELECT 1 FROM store WHERE plugin = ?1 AND key = ?2",
                            params![id, key],
                            |_| Ok(true),
                        )
                        .optional()
                        .map_err(mlua::Error::external)?
                        .unwrap_or(false);
                    if !exists {
                        return Err(mlua::Error::runtime(format!(
                            "telega.store_set: не больше {MAX_STORE_KEYS} ключей"
                        )));
                    }
                }
                let value: serde_json::Value = lua.from_value(value)?;
                let value = value.to_string();
                if value.len() + key.len() > MAX_VALUE_BYTES {
                    return Err(mlua::Error::runtime(format!(
                        "telega.store_set: значение больше {} КБ",
                        MAX_VALUE_BYTES / 1024
                    )));
                }
                db.execute(
                    "INSERT INTO store (plugin, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT (plugin, key) DO UPDATE SET value = excluded.value",
                    params![id, key, value],
                )
            }
            .map_err(mlua::Error::external)?;
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "log",
        lua.create_function(move |_, args: mlua::Variadic<Value>| {
            if ctx_mut(&c)?.logs.len() >= MAX_LOGS_PER_CALL {
                ctx_mut(&c)?.dropped_logs += 1;
                return Ok(());
            }
            // Converted without holding the state: __tostring may run Lua.
            let line: String = args
                .iter()
                .map(|v| {
                    v.to_string()
                        .unwrap_or_else(|_| format!("{v:?}"))
                        .chars()
                        .take(MAX_LOG_LINE)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ");
            ctx_mut(&c)?.log(line);
            Ok(())
        })?,
    )?;

    Ok(t)
}

fn parse_manifest(id: &str, t: &Table, builtin: bool) -> mlua::Result<PluginInfo> {
    let name: String = t
        .get::<Option<String>>("name")?
        .unwrap_or_else(|| id.to_owned());
    let description: String = t.get::<Option<String>>("description")?.unwrap_or_default();
    let mut permissions = Vec::new();
    for p in t
        .get::<Option<Vec<String>>>("permissions")?
        .unwrap_or_default()
    {
        let parsed = Permission::parse(&p)
            .ok_or_else(|| mlua::Error::runtime(format!("неизвестное право \"{p}\"")))?;
        if !permissions.contains(&parsed) {
            permissions.push(parsed);
        }
    }
    let mut settings = Vec::new();
    for s in t.get::<Option<Vec<Table>>>("settings")?.unwrap_or_default() {
        let key: String = s.get("key")?;
        let kind = match s.get::<String>("type")?.as_str() {
            "number" => SettingKind::Number,
            "text" => SettingKind::Text,
            "bool" => SettingKind::Bool,
            "chats" => SettingKind::Chats,
            other => {
                return Err(mlua::Error::runtime(format!(
                    "настройка \"{key}\": неизвестный тип \"{other}\""
                )));
            }
        };
        let default = match (kind, s.get::<Value>("default")?) {
            (SettingKind::Chats, _) => serde_json::json!([]),
            (SettingKind::Number, Value::Integer(n)) => serde_json::json!(n),
            (SettingKind::Number, Value::Number(n)) => serde_json::json!(n),
            (SettingKind::Number, _) => serde_json::json!(0),
            (SettingKind::Bool, Value::Boolean(b)) => serde_json::json!(b),
            (SettingKind::Bool, _) => serde_json::json!(false),
            (SettingKind::Text, Value::String(v)) => serde_json::json!(v.to_str()?.to_string()),
            (SettingKind::Text, _) => serde_json::json!(""),
        };
        let label: String = s
            .get::<Option<String>>("label")?
            .unwrap_or_else(|| key.clone());
        settings.push(SettingSpec {
            key,
            label,
            kind,
            default,
        });
    }
    Ok(PluginInfo {
        id: id.to_owned(),
        name,
        description,
        permissions,
        settings,
        builtin,
        error: None,
    })
}

#[cfg(test)]
mod tests;
