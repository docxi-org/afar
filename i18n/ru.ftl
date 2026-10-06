# Строки самого afar (строки Far Manager — в far/*.ftl).

objects = { $count ->
    [one] { $count } объект
    [few] { $count } объекта
   *[many] { $count } объектов
}

## Панель агента
agent-title = Агент · claude · { $status }
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
