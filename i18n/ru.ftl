# Строки самого afar (строки Far Manager — в far/*.ftl).

objects = { $count ->
    [one] { $count } объект
    [few] { $count } объекта
   *[many] { $count } объектов
}

## Панель агента
agent-title = Агент · { $name } · { $status }
agent-running = работает
agent-not-started = не запущен — Enter: запуск
agent-exited = завершён (код { $code }) — Enter: перезапуск
observe-live = ● live
observe-on-demand = ○ по запросу
agent-footer = { $scroll }{ $mode } · +{ $unseen } соб. · Ctrl+Space
agent-config-failed = Не удалось подготовить конфигурацию агента: { $error }
agent-start-failed = Не удалось запустить claude: { $error }
observe-switched-live = Агент видит ваши действия сразу (live). afar:live — переключить
observe-switched-on-demand = Агент читает журнал по необходимости. afar:live — переключить
agent-asks = Агент просит { $what }
requested-by-agent = — запрошено агентом —

## Подсказки фокуса в строке клавиш
hint-agent = Ввод идёт агенту · Ctrl+Space — к панелям
hint-command = Ввод идёт команде · Ctrl+Space — к агенту · Ctrl+O — экран команды

## Сообщения
command-busy = Команда уже выполняется — дождитесь завершения
command-start-failed = Ошибка запуска: { $error }
quit-confirm = Агент или команда ещё работают. F10 ещё раз — выход
not-implemented = F{ $n } — ещё не реализовано
ide-connected = Агент подключился к afar как к IDE
ide-disconnected = Агент отключился от afar как от IDE
ide-diff-title = Правка агента
ide-diff-lines = Строк: было { $old }, станет { $new }
ide-diff-accept = &Принять
ide-diff-reject = &Отклонить
ide-start-failed = Протокол IDE не запущен: { $error }
viewer-not-yet = В просмотрщике ещё не реализовано
not-a-folder = { $path } — не папка
op-waits-answer = Файловая операция ждёт ответа — Ctrl+Space

## Режим разработки
dev-building = afar: сборка…
dev-built = afar: собрано за { $secs } с — перезапуск
dev-build-failed = afar: ошибка сборки — Ctrl+O
dev-restart-waits = afar: перезапуск ждёт — { $reason }
dev-only = Перезапуск работает в режиме разработки: afar --dev
dev-blocker-dialog = открыт диалог
dev-blocker-operation = идёт файловая операция
dev-blocker-command = выполняется команда
dev-blocker-agent-busy = агент работает
dev-blocker-agent-starting = агент запускается

## Файловые операции (формулировки afar там, где у Far своих нет)
copy-nothing = Нечего копировать
copy-root = { $path }: нельзя копировать или переносить корень диска
copy-file-where-folder = на месте папки уже есть файл с тем же именем
copy-folder-where-file = { $path }: на этом месте папка
link-create-failed = не удалось создать ссылку: { $error }

config-problem = Настройки не прочитаны: { $problem }

## F9 → Параметры: настройки агента
menu-agent-settings = Агент и ра&зрешения
agent-settings-title = Агент и разрешения
agent-settings-command = Команда агента:
agent-settings-args = Аргументы:
agent-settings-live = Режим &live при запуске (действия уходят с каждым запросом)
agent-settings-position = Панель агента:
agent-position-bottom = внизу, под панелями
agent-position-top = вверху, над панелями
agent-settings-ide = afar как &IDE агента: он видит открытый в просмотре файл, Ctrl+Enter
agent-settings-channels = &События afar будят агента (Channels; при запуске агент спросит подтверждение)
agent-settings-confirm-channels = afar сам &подтверждает запуск своего канала
agent-channels-confirmed = afar подтвердил запуск своего канала разработки (server:afar-channel)
agent-settings-permissions = Что агенту можно делать через afar
perm-navigate = Показывать и выделять в панелях:
perm-mkdir = Создавать папки:
perm-copy = Копировать:
perm-move = Перемещать и переименовывать:
perm-delete = Удалять в Корзину:
perm-delete-permanent = Удалять минуя Корзину:
perm-run-command = Выполнять команды:
perm-allow = Разрешать
perm-confirm = Спрашивать
perm-deny = Запрещать
agent-settings-note = Свои Bash, Edit и Write агента подтверждает Claude Code.
agent-settings-restart = Команда, аргументы, IDE и события применятся при следующем запуске агента.
settings-saved = Настройки сохранены: { $path }
settings-save-failed = Настройки не сохранены: { $error }
agent-menu-session = Сессия
agent-menu-mode = Режим
agent-menu-links = Связь
agent-menu-view = Вид
agent-menu-new = &Новая сессия в каталоге панели
agent-menu-resume = &Продолжить сессию каталога…
agent-menu-move = Перенести сессию в &каталог панели
agent-menu-restart = П&ерезапустить агента
agent-menu-rename = Пере&именовать…
agent-menu-compact = &Сжать контекст (/compact)…
agent-menu-clear = &Очистить (/clear)
agent-menu-interrupt = П&рервать ход
agent-mode-default = Разрешения: &обычный режим
agent-mode-accept-edits = Разрешения: &принимать правки
agent-mode-plan = Разрешения: п&лан
agent-mode-auto = Разрешения: &авто
agent-mode-dont-ask = Разрешения: &не спрашивать
agent-mode-bypass = Разрешения: о&бходить
agent-menu-model = Модель { $model }
agent-menu-model-other = Другая &модель…
agent-effort-low = Усилие: низкое
agent-effort-medium = Усилие: среднее
agent-effort-high = Усилие: высокое
agent-effort-xhigh = Усилие: очень высокое
agent-effort-max = Усилие: максимум
agent-menu-on-demand = Наблюдение: по &запросу
agent-menu-live = Наблюдение: &live
agent-menu-ide = afar как &IDE агента
agent-menu-channels = &События afar будят агента (Channels)
agent-menu-ide-log = Протокол IDE (ide.&log)
agent-menu-journal = &Журнал сессии
agent-menu-settings = Агент и &разрешения…
agent-menu-top = Панель агента &вверху
agent-menu-bottom = Панель агента в&низу
agent-menu-taller = &Выше
agent-menu-shorter = Ни&же
agent-menu-hide = &Погасить
agent-rename-title = Имя сессии
agent-rename-prompt = Имя сессии агента (/rename):
agent-model-title = Модель
agent-model-prompt = Модель агента (/model), например claude-opus-5-5:
agent-sessions-title = Сессии: { $dir }
agent-no-sessions = Нет сохранённых сессий в { $dir }
confirm-agent = Действия с сессией а&гента (afar)
agent-confirm-title = Агент
agent-compact-title = Сжать контекст агента (/compact)
agent-compact-prompt = Что сохранить при сжатии (необязательно):
agent-compact-button = &Сжать
agent-confirm-compact-note = Разговор заменится кратким изложением — подробности ранних сообщений агент потеряет.
agent-confirm-clear = Очистить контекст агента (/clear)?
agent-confirm-clear-note = Агент начнёт новую сессию с пустым контекстом; текущая останется на диске, её можно продолжить.
agent-confirm-end = Завершить работающего агента?
agent-confirm-end-note = Текущий ход прервётся; разговор сохранится, его можно продолжить.
agent-confirm-bypass = Режим «обходить разрешения»: агент будет выполнять любые команды и правки без вопросов.
history-open-failed = История не открылась (работает до выхода): { $error }
ac-command-line = Автозавершение в &командной строке
ac-sources = Откуда брать варианты
ac-source-history = История:
ac-source-files = Файлы и папки:
ac-source-variables = Переменные окружения:
ac-source-programs = Программы из PATH:
ac-use-always = всегда
ac-use-ctrl-space = только по Ctrl+Space
ac-use-never = никогда
history-filter = фильтр: { $filter }
history-passive-panel = Пассивная панель
history-folders = Папки
ac-suggest = Подсказка при наборе:
ac-suggest-ghost = продолжение серым
ac-suggest-list = список (как в Far)
ac-suggest-off = нет
ac-fuzzy = &Нечёткие совпадения (буквы по порядку)
completion-passive-panel = Пассивная панель
menu-import-far-history = Импорт истории &Far…
far-import-title = История Far
far-import-found = Найдена история Far: { $path }
far-import-counts = команд: { $commands }, папок: { $folders }, просмотра и правки: { $views }, полей диалогов: { $dialogs }
far-import-question = Перенести её в afar? Повторно — F9 → Команды → Импорт истории Far.
far-import-yes = &Перенести
far-import-no = &Не сейчас
far-import-done = Перенесено из Far — { $counts }
far-import-nothing-new = Из Far нечего переносить: всё уже перенесено
far-import-not-found = История Far не найдена (Far Manager\Profile\history.db)
far-import-empty = История Far пуста
far-import-failed = Не удалось прочитать историю Far: { $error }
