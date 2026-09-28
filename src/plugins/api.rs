//! Single description of the plugin API: rendered as the in-app reference and
//! written out as `telega.d.lua` type stubs for Lua language servers.

pub struct ApiDoc {
    /// Luau-style signature as shown to the user.
    pub signature: &'static str,
    pub doc: &'static str,
    /// Stub body for `telega.d.lua` (LuaLS annotations + declaration).
    pub stub: &'static str,
}

pub const API: &[ApiDoc] = &[
    ApiDoc {
        signature: "telega.plugin { name, description, permissions, settings }",
        doc: "Описание плагина. Вызывается один раз в начале файла. permissions: \
              \"read\" — получать события о сообщениях, \"delete_own\" — удалять свои \
              сообщения, \"send\" — отправлять сообщения. settings — список настроек \
              { key, type = \"number\" | \"text\" | \"bool\" | \"chats\", label, default }.",
        stub: "---@param manifest {name: string, description: string?, permissions: string[]?, settings: table[]?}\nfunction telega.plugin(manifest) end",
    },
    ApiDoc {
        signature: "telega.on(event, function(data) … end)",
        doc: "Подписка на событие; нужно право \"read\". События: \"message_new\" \
              { chat_id, id, text, outgoing, sender_id }, \"message_edited\" \
              { chat_id, id, text }, \"message_deleted\" { chat_id, ids }. \
              Не больше 16 обработчиков на событие.",
        stub: "---@param event \"message_new\"|\"message_edited\"|\"message_deleted\"\n---@param handler fun(data: table)\nfunction telega.on(event, handler) end",
    },
    ApiDoc {
        signature: "telega.after(seconds, task, payload)",
        doc: "Запланировать задачу через seconds секунд. Задачи хранятся в базе и \
              переживают перезапуск; пока клиент закрыт, они ждут. При выключении \
              плагина его задачи отменяются. Задержка не меньше 1 с; payload — \
              до 64 КБ и не глубже 32 уровней; не больше 10 000 задач.",
        stub: "---@param seconds number\n---@param task string\n---@param payload table?\nfunction telega.after(seconds, task, payload) end",
    },
    ApiDoc {
        signature: "telega.on_task(task, function(payload) … end)",
        doc: "Обработчик задачи, запланированной через telega.after.",
        stub: "---@param task string\n---@param handler fun(payload: table)\nfunction telega.on_task(task, handler) end",
    },
    ApiDoc {
        signature: "telega.delete(chat_id, ids, { revoke })",
        doc: "Удалить свои сообщения (чужие клиент пропустит); нужно право \
              \"delete_own\". revoke = true — у всех, иначе только у себя. В режиме \
              пробного прогона только записывает в журнал.",
        stub: "---@param chat_id number\n---@param ids number[]\n---@param options {revoke: boolean?}?\nfunction telega.delete(chat_id, ids, options) end",
    },
    ApiDoc {
        signature: "telega.send(chat_id, text)",
        doc: "Отправить текст как есть (без разметки), до 4096 символов; нужно право \
              \"send\". Не больше 20 действий за один вызов обработчика. В режиме \
              пробного прогона только записывает в журнал.",
        stub: "---@param chat_id number\n---@param text string\nfunction telega.send(chat_id, text) end",
    },
    ApiDoc {
        signature: "telega.setting(key)",
        doc: "Текущее значение настройки плагина (или значение по умолчанию). \
              Для типа \"chats\" — список id чатов.",
        stub: "---@param key string\n---@return any\nfunction telega.setting(key) end",
    },
    ApiDoc {
        signature: "telega.store_get(key) / telega.store_set(key, value)",
        doc: "Собственное хранилище плагина: строка → значение (таблицы, числа, \
              строки). Своё для каждого аккаунта, сохраняется между запусками. \
              Не больше 1000 ключей и 64 КБ на ключ и значение вместе. Без входа \
              в аккаунт store_get возвращает nil, а store_set — ошибку. \
              value = nil удаляет ключ.",
        stub: "---@param key string\n---@return any\nfunction telega.store_get(key) end\n\n---@param key string\n---@param value any\nfunction telega.store_set(key, value) end",
    },
    ApiDoc {
        signature: "telega.log(...)",
        doc: "Запись в журнал плагина (виден в настройках).",
        stub: "---@param ... any\nfunction telega.log(...) end",
    },
];

/// Contents of `telega.d.lua`.
pub fn stubs() -> String {
    let mut out = String::from(
        "---@meta\n-- Описание API плагинов Telega для Lua Language Server.\n-- Файл создаётся клиентом; правки перезапишутся.\n\ntelega = {}\n",
    );
    for api in API {
        out.push('\n');
        for line in api.doc.split(". ") {
            out.push_str("--- ");
            out.push_str(line.trim_end_matches('.'));
            out.push_str(".\n");
        }
        out.push_str(api.stub);
        out.push('\n');
    }
    out
}
