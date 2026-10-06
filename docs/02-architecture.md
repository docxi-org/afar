# 02. Архитектура

## Стек

| Задача | Выбор | Комментарий |
|---|---|---|
| Язык | Rust (stable, edition 2024) | Надёжность файловых операций, производительность, кроссплатформенность |
| TUI | `ratatui` + `crossterm` | Immediate-mode рендеринг с диффом буфера; на Windows — консольный API / VT |
| Асинхронность | `tokio` | Фоновые задачи, каналы, HTTP для MCP |
| Псевдотерминал | `portable-pty` | ConPTY на Windows, openpty на Unix |
| Эмуляция терминала | `vt100` + `tui-term` | Разбор VT-вывода в сетку ячеек и виджет для ratatui. Запасной вариант — `alacritty_terminal` (полнее, тяжелее) |
| MCP-сервер | `rmcp` (официальный Rust SDK) | Транспорт Streamable HTTP на `127.0.0.1` |
| Слежение за ФС | `notify` | Обновление панелей, атрибуция внешних изменений |
| Сериализация | `serde`, `serde_json`, `toml` | Журнал (JSONL), конфигурация (TOML) |
| Хранилище истории | `rusqlite` | История каталогов/команд/файлов, как в Far |
| Логирование | `tracing` + `tracing-appender` | Только в файл — stdout занят TUI |
| Корзина | `trash` | Удаление в корзину на всех платформах |
| Ширина символов | `unicode-width`, `unicode-segmentation` | Выравнивание колонок с CJK/эмодзи |

## Модули (cargo workspace)

```
afar/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ afar-core/              # модель без UI: состояние, команды, события, журнал, политика
│  ├─ afar-vfs/               # трейт виртуальной ФС + локальная ФС (позже: архивы, SFTP)
│  ├─ afar-ops/               # движок файловых операций (copy/move/delete/mkdir/attrs)
│  ├─ afar-term/              # PTY-сессии, VT-эмуляция, кодирование клавиш в VT
│  ├─ afar-mcp/               # MCP-сервер: инструменты ↔ команды, ресурсы ↔ состояние
│  ├─ afar-ui/                # виджеты: панели, командная строка, keybar, меню, диалоги, просмотрщик, терминал
│  └─ afar/                   # бинарник: сборка всего вместе, конфиг, подкоманды CLI (`afar hook ...`)
└─ docs/
```

Правило зависимостей: `afar-core` ни от чего UI-шного не зависит; `afar-ui` не делает ввод-вывод сам — только отображает состояние и превращает ввод в команды.

На этапе прототипа допустимо держать всё в одном крейте с модулями той же структуры и разделить позже.

## Модель потоков: один владелец состояния

Всё состояние приложения (`AppState`) принадлежит **одной задаче — главному циклу**. Остальные компоненты общаются с ним только сообщениями. Это прямой ответ на проблему Far, где управление извне требует `ACTL_SYNCHRO`, а любой долгий вызов блокирует интерфейс.

```
          ┌──────────────── ввод терминала (crossterm EventStream)
          │
          │   ┌──────────── PTY reader (поток на сессию) ── вывод уже разобран в vt100::Parser
          │   │
          ▼   ▼
   ┌─────────────────────┐   AppMsg (mpsc)    ┌──────────────────────────┐
   │   Главный цикл      │◄───────────────────│ Движок операций (tokio)  │ прогресс/итог
   │   (владеет AppState)│◄───────────────────│ Слежение за ФС (notify)  │ изменения
   │                     │◄───────────────────│ MCP-сервер (rmcp/HTTP)   │ запрос + oneshot для ответа
   │ dispatch → reduce   │◄───────────────────│ Хук-клиенты (`afar hook`)│ через локальный IPC
   │ → journal → render  │                    └──────────────────────────┘
   └─────────────────────┘
```

Цикл:

1. `select!` по: событиям ввода, каналу `AppMsg`, таймеру кадра.
2. Ввод → раскладка клавиш → `Invocation` (команда + источник).
3. `Policy::check` → разрешить / запросить подтверждение (диалог) / отказать.
4. Исполнение: быстрые команды меняют `AppState` сразу; долгие — запускают фоновую задачу и сразу возвращают управление.
5. Каждое значимое изменение порождает `JournalEntry`.
6. Состояние помечается «грязным»; отрисовка — не чаще ~60 раз в секунду (кадры объединяются).

Фоновые задачи **никогда** не трогают `AppState` напрямую.

## Слой команд

```rust
pub struct Invocation {
    pub id: InvocationId,
    pub origin: Origin,          // откуда пришла команда
    pub command: Command,
}

pub enum Origin {
    User { key: Option<KeyChord> },
    Agent { tool_call: String },  // вызов MCP-инструмента
    Macro { name: String },       // зарезервировано
    System,
}

pub enum Command {
    Panel(PanelCmd),        // ChangeDir, MoveCursor, Select, SetSort, SetViewMode, Refresh, Swap...
    FileOp(FileOpCmd),      // Copy, Move, Rename, Delete, MkDir, SetAttrs, Link
    View(ViewCmd),          // OpenViewer{path, line}, OpenEditor{path, line}, QuickView, Compare
    Cmdline(CmdlineCmd),    // Execute{text}, Insert{text}, Clear
    Layout(LayoutCmd),      // FocusPane, TogglePanels, ResizeAgentPane, ToggleTerminalView
    Agent(AgentCmd),        // StartSession, Restart, ToggleLiveObserve, SendText
    App(AppCmd),            // Quit, OpenMenu, OpenConfig
}
```

Требования:

- у каждой команды есть **стабильное имя** (`panel.change_dir`, `fileop.copy`), сериализуемые параметры (serde) и описание — из них генерируются раскладка клавиш, меню и MCP-инструменты;
- команды, требующие диалога (F5 копирование), разделяются на «открыть диалог» (для пользователя) и «выполнить с параметрами» (для агента и макросов); диалог в итоге вызывает второе;
- результат команды — `Outcome` (успех / ошибка / задача запущена с `TaskId`).

### Политика разрешений

`Policy::check(&Invocation) -> Decision { Allow, Confirm(Prompt), Deny(Reason) }`

- для `Origin::User` подтверждения задаются как в Far (подтверждать удаление, перезапись и т. п.);
- для `Origin::Agent` по умолчанию: чтение и навигация — разрешены; создание — разрешено; копирование/перемещение — подтверждение; удаление — подтверждение, только в корзину; выполнение команд в командной строке — подтверждение;
- уровни настраиваются в `config.toml` (`[agent.permissions]`).

## Журнал событий

```rust
pub struct JournalEntry {
    pub seq: u64,                 // монотонный номер в сессии
    pub ts: DateTime<Utc>,
    pub actor: Actor,             // User | Agent | System | External
    pub invocation: Option<InvocationId>,
    pub event: Event,
}

pub enum Event {
    AppStarted { cwd_left: PathBuf, cwd_right: PathBuf },
    DirChanged { panel: Side, from: PathBuf, to: PathBuf },
    FocusChanged { pane: Pane },
    SelectionChanged { panel: Side, count: usize, sample: Vec<String> },
    FileOpStarted { op: OpId, kind: OpKind, sources: Vec<PathBuf>, dest: Option<PathBuf> },
    FileOpFinished { op: OpId, result: OpResult, done: usize, failed: Vec<(PathBuf, String)> },
    CommandStarted { cmd_id: CmdId, text: String, cwd: PathBuf },
    CommandFinished { cmd_id: CmdId, exit_code: Option<i32>, duration_ms: u64, output: OutputRef },
    FileOpened { path: PathBuf, mode: OpenMode },
    FileSaved { path: PathBuf },
    FsChanged { path: PathBuf, change: FsChangeKind },      // замечено watcher'ом, не нашими операциями
    AgentToolUsed { tool: String, summary: String, paths: Vec<PathBuf> }, // из хука PostToolUse
    AgentPrompt { chars: usize },                           // пользователь отправил запрос агенту (без текста)
}
```

- Хранение: кольцевой буфер в памяти (последние N тыс. записей) + файл `sessions/<session-id>/journal.jsonl` в каталоге данных.
- **Не журналируется:** движение курсора, прокрутка, ввод символов в командной строке до Enter.
- **Объединяется:** изменения выделения — с задержкой 500 мс (одна запись на серию), быстрые серии смены каталогов — без объединения, но с компактным форматом при выдаче агенту.
- `OutputRef` — ссылка на сохранённый вывод команды (`sessions/<id>/output/<cmd_id>.log`): текст берётся из эмулятора терминала (строки, уходящие в историю прокрутки), а не из сырого потока байтов, с ограничением размера; агент получает вывод отдельным MCP-инструментом. Подробно — [04-agent.md](04-agent.md#консольный-вывод).
- Атрибуция `FsChanged`: если путь затронут нашей операцией в последние ~2 с — событие подавляется; если совпадает с путём из недавнего `AgentToolUsed` — `actor = Agent`; иначе `External`.

Выдача журнала агенту — [04-agent.md](04-agent.md#журнал-для-агента).

## Состояние приложения

```rust
pub struct AppState {
    pub panels: [PanelState; 2],
    pub active_side: Side,
    pub focus: Pane,                 // Left | Right | CommandLine | Agent | Terminal | Viewer(id) | Dialog
    pub layout: LayoutState,         // высота панели агента, скрытие панелей, режим «терминал»
    pub cmdline: CmdlineState,
    pub terminals: TerminalSet,      // сессия агента + сессия пользовательских команд
    pub tasks: TaskTable,            // фоновые операции и их прогресс
    pub viewers: Vec<ViewerState>,
    pub dialogs: DialogStack,
    pub journal: Journal,
    pub config: Config,
}

pub struct PanelState {
    pub vfs: VfsLocation,            // где находится панель (локальная ФС, позже — архив и т.п.)
    pub entries: Vec<Entry>,         // отсортированный список
    pub cursor: usize,
    pub top: usize,
    pub selection: SelectionSet,
    pub sort: SortSpec,
    pub view_mode: ViewMode,         // Brief / Medium / Full / Wide / Detailed / Descriptions...
    pub filter: Option<Filter>,
    pub kind: PanelKind,             // Files | Tree | Info | QuickView
}
```

Чтение каталогов — в фоновой задаче (большие и сетевые каталоги не блокируют интерфейс); пока чтение идёт, панель показывает индикатор.

## Рендеринг

- Каждый кадр строится из `AppState` (immediate mode); `ratatui` сам отправляет в терминал только изменившиеся ячейки.
- Цвета — палитра в стиле Far (классические 16 цветов), задаётся темой (`themes/far-classic.toml`); поддержка 256/truecolor для тем и для вывода внутри встроенного терминала.
- Рамки — символы псевдографики (`═ ║ ╔ ╗ ╟ ─`), как в Far.
- Ширина символов — `unicode-width`; имена файлов с широкими символами обрезаются по графемам.

## Встроенный терминал

`afar-term` предоставляет `PtySession`:

- `spawn(cmd, args, cwd, env, size)` через `portable-pty`;
- поток-читатель: читает байты из PTY **всегда** (иначе дочерний процесс встанет на заполненном канале), скармливает их в `vt100::Parser` под `Mutex`, шлёт `AppMsg::PtyDirty(session)` (повторные уведомления объединяются);
- `write_input(bytes)`, `resize(rows, cols)`;
- отрисовка: виджет `tui-term::PseudoTerminal` по снимку `vt100::Screen`;
- **кодировщик клавиш** (пишем свой, небольшой): `KeyEvent` → xterm-последовательности (стрелки и F-клавиши с модификаторами, Alt как префикс ESC, Ctrl+буква → C0); вставка из буфера — с обёрткой `ESC[200~ … ESC[201~`, если программа включила bracketed paste; мышь — SGR 1006, если программа её запросила;
- прокрутка истории (scrollback) — по Shift+PgUp/PgDn, не передаётся программе.

Сессии: **агент** (одна долгоживущая PTY с `claude`) и **команды** — у каждой своя PTY и свой эмулятор, поэтому её границы, код возврата и вывод известны точно. «Экран пользователя» составляется из текста завершённых команд и живого экрана текущей (см. [03-ui-and-keys.md](03-ui-and-keys.md#командная-строка-и-терминал), [04-agent.md](04-agent.md#консольный-вывод)). Один общий эмулятор на все команды не годится: ConPTY перерисовывает экран по своей копии и затирает чужой вывод (проверено в прототипе).

Особенности, выясненные в прототипе:

- `vt100` лежит в `vendor/vt100` (MIT) с небольшой доработкой: колбэк строк, уходящих за верх экрана (`Screen::set_line_capture` / `take_scrolled_lines`), — через него текст вывода дописывается в лог команды. Доработка помечена `AFAR-PATCH`.
- ConPTY не закрывает канал вывода после завершения процесса — отдельный поток ждёт процесс и закрывает псевдоконсоль; ConPTY при старте запрашивает позицию курсора (`CSI 6n`), эмулятор обязан ответить.
- `portable-pty` экранирует аргументы по правилам MSVCRT, которые `cmd.exe` не понимает: текст команды передаётся через переменную окружения — `cmd /c %AFAR_CMD%`.
- С кириллической раскладкой `Ctrl+O` приходит как `Ctrl+Щ`: для сочетаний с Ctrl/Alt буквы ЙЦУКЕН сопоставляются латинским по положению клавиши (`keys::normalize`).
- Вывод кадра: весь кадр собирается в памяти и уходит одной записью в обёртке синхронного вывода (`?2026h` … `?2026l`, `src/tui.rs`) — иначе Windows Terminal показывает перерисовку по частям (Rust пишет в консоль Windows кусками по 8 КБ).
- Синхронный вывод программ в PTY: Claude Code рисует кадры между `?2026h` и `?2026l`; на время кадра эмулятор показывает снимок последнего целого экрана (`termview::Snapshot`, `term::view`) и не будит главный цикл; кадр дольше 250 мс показывается как есть.
- `AFAR_DEBUG_FRAMES=1` пишет размер и время каждого кадра в `frames.log` каталога сессии.
- Агенту не передаются служебные переменные окружения внешней сессии Claude Code (`CLAUDECODE`, `CLAUDE_CODE_*`), иначе он считает себя дочерней сессией.

## Конфигурация и данные

| Что | Где (Windows / Unix) |
|---|---|
| Конфигурация | `%APPDATA%\afar\config.toml` / `~/.config/afar/config.toml` |
| Темы, раскладки | рядом с конфигурацией: `themes/`, `keymaps/` |
| История (SQLite) | `%LOCALAPPDATA%\afar\history.db` / `~/.local/share/afar/history.db` |
| Сессии (журнал, вывод команд) | `…\afar\sessions\<session-id>\` |
| Логи приложения | `…\afar\logs\` |

Пути — через крейт `directories`.

## Обработка ошибок и логирование

- `thiserror` для типизированных ошибок в библиотечных крейтах, `anyhow` — в бинарнике;
- паника в фоновой задаче не роняет приложение: задача завершается с ошибкой, пользователь видит сообщение;
- при панике главного цикла — восстановление терминала (выход из raw mode / alternate screen) в panic hook.

## Тестирование

- `afar-core`: модульные тесты на команды, политику, журнал, сортировку, выделение по маске;
- `afar-ops`: интеграционные тесты на временных каталогах (`tempfile`): конфликты, ошибки доступа, длинные пути, символические ссылки, отмена посередине;
- `afar-ui`: снимки экрана через `ratatui::backend::TestBackend` (`insta`);
- `afar-mcp`: тесты инструментов через клиент `rmcp`;
- `afar-term`: тесты кодировщика клавиш; дымовой тест запуска `cmd /c echo` / `sh -c echo` в PTY.
