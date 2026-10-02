# Локальный аудит безопасности — 2026-10-01

## Результат и границы

Исторический отчёт о состоянии на 2026-10-01: обнаружены четыре риска High; подтверждённого удалённого чтения ключей, выхода Lua из песочницы или RCE не получено. Примеры ниже объясняют прежнюю механику, а не текущий API. Все 14 пунктов и аудиопауза исправлены в v0.3.0; применённые изменения, проверки и ограничения — в [описании исправлений 2026-10-02](security-fixes-2026-10-02.md). Аудит и регрессии не являются сертификатом безопасности.

Проверены Rust-границы TDLib, обработчики аккаунта/пароля/плагинов, пути/URL/разметка, изображения и медиапотоки. Реальный аккаунт, сеть, отправка сообщений, запуск внешних URL/файлов и микрофон не использовались. Пробы используют синтетические данные и in-memory SQLite. Нативные TDLib/FFmpeg/rlottie не прошли полноценный fuzzing; устойчивость к каждому возможному повреждённому файлу не установлена.

### Исторические пробы на дату аудита

- Девять диагностических проб приложения: `target/debug/deps/telega-068fd3d2e3404dfa security_probe_ --ignored --nocapture --test-threads=1`; 9 passed, 4.69 с. Это успешное **воспроизведение наблюдений**, а не отсутствие уязвимостей.
- Две пробы реального vendored Observer: standalone `rustc --test` с импортом исходного `observer.rs` и уже собранными зависимостями; 2 passed, 0.01 с. Cargo не запускает тесты зависимости из корневого `cargo test`; воспроизводимый runner описан ниже.
- ОС сообщила пик дочернего процесса диагностик 140880 KiB (около 138 MiB); это максимум всего тестового процесса, не изолированная стоимость конкретного сообщения. В измерение не входила сборка.
- 10000 входящих обновлений / 10 чатов: 1371 мс, 1000 сообщений сохранено в открытой панели, построено 47 видимых элементов. Это виртуализация отображения, **не ограничение общего числа сохранённых сообщений**.
- Один headless render текста длиной 4095 символов: обычная строка 122 мс; 2047 переводов строк — 233 мс; те же переводы строк в code entity — 513 мс. Один замер каждого большого варианта, debug-сборка: не performance SLA и не строгий benchmark.
- RTL/zero-width и некорректные границы UTF-16 дошли до настоящего App/iced; текст не потерялся, маскирование и изображения кадра наблюдались. Это не доказательство защиты от визуального обмана Unicode.
- Lua: один timeout обработчика; затем 64 события по 4096 байт обработаны за 201 мс, после отключения выходов нет. Реальный HostHandle принял все 512 событий по 32768 байт за 1 мс — 16 MiB backlog без потребителя. Последняя проба измеряет admission, **не throughput живого worker**.

Повторить пробы приложения после сборки:

```sh
cargo test --locked --bin telega security_probe_ -- --ignored --nocapture --test-threads=1
```

Диагностические пробы явно игнорируются обычным suite. Пробы, фиксировавшие уязвимый инвариант, в v0.3.0 заменены поведенческими `security_fix_` регрессиями; остальные input/Lua-диагностики сохранены. Исторические количества и времена выше не являются результатом текущего runner.

Observer проверяется отдельным tracked runner, импортирующим настоящий vendored исходник:

```sh
cargo test --locked --no-run
bash scripts/security-observer-probe.sh
```

Runner использует уже собранные `.rlib` и fingerprint из `target/debug`; не устанавливает зависимости и не создаёт TDLib-клиент. В v0.3.0 он проверяет настоящие Observer и response: 4 passed, malformed `@extra` не вызывает panic, некорректный `@type` не теряет корреляцию, предупреждения не содержат payload. На дату первоначального аудита две диагностические пробы воспроизводили старые ошибки, включая пойманный panic.

## High

### H1. Ошибка обслуживания после committed rekey теряет согласованность ключей

**Файл/контекст:** `src/archive.rs`, `Archive::rekey`; `src/app/password.rs`, `ArchiveRekeyed` и `finish_password_change`.

**Механика:** `tx.commit()` и смена `self.key` происходят перед fallible checkpoint/VACUUM. Caller трактует любую ошибку как отсутствие rekey и откатывает TDLib. При ошибке записи настроек результат обратного rekey архива вообще игнорируется. TDLib, архив и сохранённая соль могут остаться с несовместимыми ключами; прежний пароль не открывает уже изменённый архив. Это риск доступности/целостности, не доказанное раскрытие ключа атакующему.

**Доказательство:** изолированный SQLite authorizer запретил ATTACH, используемый VACUUM. `rekey` вернул `AuthorizationForStatementDenied`, но новая ключевая пара читала committed текст; старый ключ — нет. Реальное заполнение диска/отказ TDLib не моделировались.

**Исправление:** явно разделить committed состояние и ошибку обслуживания; не откатывать TDLib до успешного восстановления архива. До первой смены долговечного ключа нужен восстанавливаемый журнал операции, а при неудаче компенсации нельзя терять альтернативные ключевые материалы. Пример контракта транзакционного шага:

```rust
struct RekeyCommit {
    cleanup_error: Option<rusqlite::Error>,
}
// Внутри rekey после подготовки UPDATEs:
tx.commit()?;
self.key = new;
let cleanup_error = self.conn
    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
    .err();
Ok(RekeyCommit { cleanup_error })
```

Успешный commit должен пройти сохранение метаданных независимо от warning обслуживания. Один этот фрагмент не решает crash recovery нескольких независимых хранилищ.

### H2. Смена аккаунта пересекается с незавершённой сменой ключа

**Файл/контекст:** `src/app/password.rs`, `Derived`/`Applied`/`ArchiveRekeyed`; `src/app/accounts.rs`, `switch_account`; `src/app.rs`, смена сессии после закрытия.

**Механика:** ключ может уже измениться в TDLib, но ответ старого slot/client отбрасывается после переключения. Salt/check предыдущего аккаунта не сохраняются. Проверка stale response защищает новую сессию, но не завершает долговечную операцию старой.

**Доказательство:** настоящий `Apply` сделал `busy=true`; `SwitchAccount(3)` всё равно установил `leave`, `after_close()` выбрал слот 3. Задачи TDLib не исполнялись: фактическая невозможность открыть базу проверена только по source flow, а не живой базой.

**Исправление:** блокировать добровольные switch/logout/quit до завершения операции и компенсации; для аварийного завершения нужен recovery H1. Пример раннего guard:

```rust
if self.session.password_form.busy || self.session.rekeying_archive.is_some() {
    return iced::Task::none();
}
```

### H3. Уже выданное действие плагина обходит включённый dry-run

**Файл/контекст:** `src/app/plugin_runtime.rs`, `run_plugin_action`; `src/app.rs`, `PluginDeleteOwn`; `src/plugins/host.rs`, `Configure`/выдача action.

**Механика:** Host очищает свою очередь, но Action уже может находиться в канале/UI. Последний App gate проверяет enabled/granted, но не dry_run; асинхронная проверка владельца удаляемого сообщения тоже не перепроверяет актуальную политику. После включения «только журнал» возможна реальная отправка/удаление ранее подготовленного действия.

**Доказательство:** реальный Lua Host выдал действие до Configure; после dry-run новых действий нет, но ранее выданное событие остаётся у получателя. Отсутствие App gate подтверждено исходником; сетевое действие не исполнялось.

**Исправление:** проверять действующую политику на каждой границе непосредственно перед эффектом, включая continuation удаления:

```rust
let allowed = config.enabled && !config.dry_run
    && required.iter().all(|p| config.granted.iter().any(|g| g == p.name()));
if !allowed { return iced::Task::none(); }
```

### H4. Действие плагина не привязано к исходной сессии аккаунта

**Файл/контекст:** `src/plugins/mod.rs`, `HostEvent::Action`; `src/plugins/host.rs`, очередь; `src/app/plugin_runtime.rs`, исполнение.

**Механика:** событие содержит ID плагина/action, но не origin. Host живёт дольше аккаунта; старое событие исполняется с **текущим** `session.client_id`. При совпадающем chat ID действие одного аккаунта может отправить/удалить данные другого. Требуется включённый плагин с соответствующим разрешением; это не обнаруженный выход из Lua sandbox.

**Доказательство:** удаление account в настоящем Host очищает будущие действия, но ранее выданный event остаётся. Отсутствие origin и выбор текущего client ID — source evidence; end-to-end отправка не проверялась.

**Исправление:** передавать origin клиента/эпохи в Event, Action, Requeue, completion и async-delete continuation; отбрасывать до исполнения несовпадение и неготовую авторизацию:

```rust
if origin.client_id != self.session.client_id || self.session.auth != Auth::Ready {
    return iced::Task::none();
}
```

Один user ID недостаточен: тот же пользователь может войти повторно с новым TDLib client.

## Medium

### M1. Пароль включается без успешно открытого существующего архива

**Файл/контекст:** `src/app/password.rs`, `Applied`; `src/archive.rs`/`src/lock.rs`, чтение plaintext legacy rows.

**Механика:** при ошибке открытия архив остаётся `None`; смена пароля пропускает его rekey и сообщает успех. Позже plaintext rows читаются и с установленным ключом. Исторические сообщения на диске не получили обещанное шифрование.

**Доказательство:** source flow, отдельный I/O fault не исполнялся.

**Исправление:** отличать отсутствие нового архива от ошибки открытия существующего; проверять перед сменой ключа:

```rust
if self.session.my_id.is_some() && self.session.archive.is_none() {
    self.password_done(Err("архив недоступен; пароль не изменён".into()));
    return iced::Task::none();
}
```

### M2. Rust-очереди плагинов не ограничены

**Файл/контекст:** `src/plugins/mod.rs`, `HostHandle` и `run`; `src/app/plugin_runtime.rs`, `plugin_event`.

**Механика:** удалённые сообщения передаются в unbounded каналы; обработчик Lua может расходовать до 200 мс на событие. VM memory/action quotas не ограничивают Rust backlog. Нагрузка способна увеличивать память и задерживать Configure/Account.

**Доказательство:** реальный transport принял 512 событий / 16 MiB без backpressure; timeout реального Lua подтверждён. Длительный рост RSS до OOM не запускался намеренно.

**Исправление:** отдельный приоритетный control канал, ограниченная event очередь и документированный drop policy; bounded выход worker. Пример admission удалённых событий:

```rust
let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<Event>(256);
match event_tx.try_send(event) {
    Ok(()) => {}
    Err(std::sync::mpsc::TrySendError::Full(_)) => { /* учесть пропуск события */ }
    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => { /* worker закрыт */ }
}
```

Configure/Account не должны теряться или блокировать GUI за переполненной event очередью.

### M3. Отмена запроса оставляет подписку в Observer

**Файл/контекст:** `vendor/tdlib-rs/src/observer.rs`, `subscribe`/`notify`; `lib.rs`, `send_request`.

**Механика:** отмена future уничтожает oneshot receiver, но глобальная карта сохраняет sender до ответа. Ответ может не прийти после закрытия клиента; накопление живёт до завершения процесса.

**Доказательство:** 10000 уничтоженных receivers оставили 10000 pending entries; 64 synthetic ответы убрали только 64; live correlation осталась корректной.

**Исправление:** RAII подписка удаляет свой номер на любом пути Drop, включая отмену. `send_request` держит её до await; таймауты задавать по семантике операции, а не одним коротким timeout для всех запросов:

```rust
struct Pending<'a> {
    extra: u32,
    receiver: futures_channel::oneshot::Receiver<serde_json::Value>,
    observer: &'a Observer,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Ok(mut requests) = self.observer.requests.write() {
            requests.remove(&self.extra);
        }
    }
}
```

### M4. FileStream скрывает терминальную ошибку загрузки

**Файл/контекст:** `src/td.rs`, `FileStream::read`/`downloaded_from`; `src/av/player.rs`, streamed decoder.

**Механика:** ошибки prefix/download превращаются в отсутствие данных или игнорируются; offset помечается requested, затем reader опрашивает каждые 40 мс без окончания. Некорректный/недоступный файл оставляет поток декодера и UI в ожидании. Stop проверяется только между потенциально зависшими TDLib запросами.

**Доказательство:** source-only; безопасный offline TDLib response harness не реализован.

**Исправление:** распространять ошибки и ограничивать отсутствие прогресса/ожидание запроса, сохраняя отмену:

```rust
let downloaded = functions::download_file(file_id, 32, offset, 0, false, client_id)
    .await
    .map_err(|e| std::io::Error::other(e.message))?;
```

Аргументы здесь иллюстрируют проверку результата; реальный диапазон download должен остаться текущим. Для bounded deadline также нужна отменяемость ожидающей подписки M3.

### M5. Лимит анимаций не ограничивает время жизни decoder workers

**Файл/контекст:** `src/app/playback.rs`, `frames`/`animation_hidden`; `src/av/video.rs`, open/decode.

**Механика:** скрытая анимация удаляется из карты и освобождает UI slot, но detached OS thread замечает отмену лишь при следующем send. Медленный open/decode + быстрое пролистывание может оставить больше workers, чем MAX_ANIMATIONS.

**Доказательство:** source-only; число зависших настоящих нативных workers не измерялось.

**Исправление:** permit должен принадлежать worker до его фактического завершения; cancellation flag и interruptible I/O — на границах native decode:

```rust
let permit = workers.clone().try_acquire_owned().map_err(|e| e.to_string())?;
std::thread::spawn(move || {
    let _permit = permit;
    decode_frames();
});
```

`decode_frames` обозначает существующий decoder body, не новый фиктивный fallback. При отказе admission UI не должен считаться playing.

### M6. Отмена PhotoView преждевременно освобождает decoder permit

**Файл/контекст:** `src/app/viewer.rs`, `decode_for_viewer`; `src/app/media.rs`, decode semaphore.

**Механика:** permit хранится в abortable async task, а `spawn_blocking` продолжает decode после drop JoinHandle. Следующая фотография получает освобождённый slot до завершения старого декодера; двухслотовый предел CPU/памяти не действует на worker lifetime.

**Доказательство:** source/lifetime proof; RSS под быстрым пролистыванием не измерялся.

**Исправление:** переместить permit в существующую blocking closure:

```rust
let permit = super::media::DECODES.acquire().await.map_err(|e| e.to_string())?;
tokio::task::spawn_blocking(move || {
    let _permit = permit;
    decode_full(&path)
}).await.map_err(|e| e.to_string()).and_then(|r| r)
```

### M7. Spoiler URL ошибочно считается раскрытой целью preview

**Файл/контекст:** `src/app/extra.rs`, классификация preview; `src/app/pane_update.rs`, `PaneMsg::Link`.

**Механика:** URL ищется в исходной строке, хотя spoiler скрывает его на экране. Карточка передаёт `hidden=false`, а handler пропускает подтверждение скрытой цели. Разрешённый https URL может раскрыть посетителя сайту, который пользователь не видел в сообщении.

**Доказательство:** обычный и Unicode-boundary spoiler реально маскировали URL, но returned preview_hidden=false. Видимый URL тоже false — контрольный сценарий. Browser не открывался.

**Исправление:** классифицировать по отображаемому/маскированному тексту:

```rust
let visible = rich::masked(&rich::pieces(&m.text));
let hidden = !visible.contains(&preview.url);
```

## Low

### L1. Многострочная разметка даёт сверхлинейную работу и копирование

**Файл/контекст:** `src/app/view.rs`, `view_rich`; `src/app/selectable.rs`, line ranges.

**Механика:** каждый segment ищет line через scan с начала; code span дополнительно копирует целый piece. Один видимый длинный message обходит защиту виртуализации по сообщениям. Для 2048 строк — примерно два миллиона сравнений при rebuild.

**Доказательство:** 4095 символов/2047 separators в реальном iced кадре: 233 мс ordinary, 513 мс code против 122 мс single line. Эти замеры согласуются с source complexity, но не доказывают отдельно стоимость каждой операции.

**Исправление:** монотонный line cursor и общий payload whole-code-copy (`Arc<str>`) вместо клонирования всего piece в каждом span; сохранить consumer behavior:

```rust
while line_index < line_ranges.len() && at >= line_ranges[line_index].end {
    line_index += 1;
}
```

## Best Practice

### B1. Пароли и ключи не гарантированно стираются из памяти

**Файл/контекст:** `src/app/password.rs`, Form/PasswordMsg; `src/lock.rs`, Copy key/base64 String.

**Механика:** `clear`, `mem::take` и drop обычной String/Copy массива не гарантируют zeroization; локальный дамп/своп может содержать остатки. Это не доказанная удалённая утечка.

**Доказательство:** source-only, memory dump не читался.

**Исправление:** некопируемые secret wrappers, deliberate borrowing вместо Clone сообщений с ключом, гарантированный Drop/clear. Пример с дополнительной зависимостью, **не установленной этим аудитом**:

```rust
let password = zeroize::Zeroizing::new(password);
let key = zeroize::Zeroizing::new(lock::derive(&password, &salt)?);
```

### B2. Malformed native response вызывает panic; warning может включать payload

**Файл/контекст:** vendored `Observer::notify` и `lib.rs::receive`.

**Механика:** unchecked native JSON fields вызывают unwrap; warning неизвестного ответа включает сырой response. При ошибке совместимости/нативного producer возможен crash, а при подключённом logger — содержание сообщения в журнале. Ни возможность remote генерации malformed TDLib envelope, ни текущая утечка в активный logger не установлены.

**Доказательство:** synthetic malformed @extra вызвал пойманный panic; после него настоящий valid response всё ещё доставился. Пустой/неверный payload не отправлялся TDLib; log output с пользовательскими данными не снимался.

**Исправление:** checked parsing на native boundary и structural-only warning:

```rust
let Some(extra) = response.get("@extra").and_then(serde_json::Value::as_u64)
    .and_then(|value| u32::try_from(value).ok()) else { return; };
log::warn!("unknown TDLib response type; payload omitted");
```

## Отдельный риск корректности, не уязвимость чтения данных

**Medium; `src/av/player.rs`, `fill_sound`:** tuple `(playing, sound.samples.pop_front())` потребляет samples и при pause. Проба наблюдала silence, уменьшение очереди при `played=0` и неверный первый sample после resume. Исправление — вызывать `pop_front` только при playing:

```rust
*sample = if playing {
    sound.samples.pop_front().map_or(0.0, |value| value * volume)
} else { 0.0 };
```

Существующее обновление `filled`/`played` при фактическом воспроизведении необходимо сохранить.

## Что не является установленной уязвимостью

- TDLib receive pointer null-check/copy и единственный receive consumer в текущем приложении проверены исходником; подтверждённого use-after-free или неверного Send/Sync не найдено.
- URL проходит `safe_link`; открытие документа имеет narrow extension allowlist. Attacker filename в path join и shell-инъекция не обнаружены.
- Lua quotas/capabilities и escape notification markup имеются; найденные плагинные риски — policy/session/queue boundaries, не sandbox escape.
- Plaintext downloaded media, avatars, plugin DB и область шифрования уже явно описаны README/ADR. Само наличие такого кэша не объявлено remote exfiltration.
- Fuzzing нативных библиотек, реальные сетевые отказы, power-failure recovery, реальный account rekey и атака до OOM не выполнялись.
