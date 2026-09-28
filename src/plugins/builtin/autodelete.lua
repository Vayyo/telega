-- Автоудаление своих сообщений в выбранных чатах через заданное время.
-- Отложенные удаления хранятся в базе и выполняются и после перезапуска;
-- пока клиент закрыт, ничего не удаляется.

telega.plugin {
  name = "Автоудаление своих сообщений",
  description = "Удаляет ваши сообщения в выбранных чатах через заданное время.",
  permissions = { "read", "delete_own" },
  settings = {
    { key = "minutes", type = "number", label = "Через сколько минут удалять", default = 5 },
    { key = "chats", type = "chats", label = "Чаты" },
    { key = "revoke", type = "bool", label = "Удалять у всех (иначе только у себя)", default = true },
  },
}

local function selected(chat_id)
  for _, id in ipairs(telega.setting("chats")) do
    if id == chat_id then
      return true
    end
  end
  return false
end

telega.on("message_new", function(msg)
  if msg.outgoing and selected(msg.chat_id) then
    telega.after(telega.setting("minutes") * 60, "delete", { chat_id = msg.chat_id, id = msg.id })
  end
end)

telega.on_task("delete", function(task)
  telega.delete(task.chat_id, { task.id }, { revoke = telega.setting("revoke") })
end)
