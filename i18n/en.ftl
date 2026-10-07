# afar's own strings (Far Manager's are in far/*.ftl).

objects = { $count ->
    [one] { $count } object
   *[other] { $count } objects
}

## Agent pane
agent-title = Agent · { $name } · { $status }
agent-running = running
agent-not-started = not started — Enter: start
agent-exited = exited (code { $code }) — Enter: restart
observe-live = ● live
observe-on-demand = ○ on request
agent-footer = { $scroll }{ $mode } · +{ $unseen } ev. · Ctrl+Space
agent-config-failed = Could not prepare the agent's configuration: { $error }
agent-start-failed = Could not start claude: { $error }
observe-switched-live = The agent sees your actions at once (live). afar:live — switch
observe-switched-on-demand = The agent reads the journal when needed. afar:live — switch
agent-asks = The agent asks to { $what }
requested-by-agent = — requested by the agent —

## Focus hints in the key bar
hint-agent = Input goes to the agent · Ctrl+Space — to the panels
hint-command = Input goes to the command · Ctrl+Space — to the agent · Ctrl+O — command screen

## Messages
command-busy = A command is already running — wait for it to finish
command-start-failed = Could not start: { $error }
quit-confirm = The agent or a command is still running. F10 again — exit
not-implemented = F{ $n } — not implemented yet
ide-connected = The agent connected to afar as its IDE
ide-disconnected = The agent disconnected from afar as its IDE
ide-diff-title = The agent's edit
ide-diff-lines = Lines: { $old } now, { $new } after
ide-diff-accept = &Accept
ide-diff-reject = &Reject
ide-start-failed = The IDE protocol did not start: { $error }
viewer-not-yet = Not implemented in the viewer yet
not-a-folder = { $path } is not a folder
op-waits-answer = A file operation waits for your answer — Ctrl+Space

## Development mode
dev-building = afar: building…
dev-built = afar: built in { $secs } s — restarting
dev-build-failed = afar: build failed — Ctrl+O
dev-restart-waits = afar: restart waits — { $reason }
dev-only = Restart works in development mode: afar --dev
dev-blocker-dialog = a dialog is open
dev-blocker-operation = a file operation is running
dev-blocker-command = a command is running
dev-blocker-agent-busy = the agent is working
dev-blocker-agent-starting = the agent is starting

## File operations (afar's wording where Far has none)
copy-nothing = Nothing to copy
copy-root = { $path }: cannot copy or move a drive root
copy-file-where-folder = a file with the same name is where the folder should go
copy-folder-where-file = { $path }: there is a folder here
link-create-failed = could not create the link: { $error }

config-problem = Settings not read: { $problem }

## F9 → Options: the agent's settings
menu-agent-settings = Agent and permissions
agent-settings-title = Agent and permissions
agent-settings-command = Agent command:
agent-settings-args = Arguments:
agent-settings-live = Start in &live mode (actions go with each prompt)
agent-settings-position = Agent pane:
agent-position-bottom = at the bottom, below the panels
agent-position-top = at the top, above the panels
agent-settings-ide = afar as the agent's &IDE: it sees the file in the viewer, Ctrl+Enter
agent-settings-channels = afar's &events wake the agent (Channels; the agent asks to confirm on start)
agent-settings-permissions = What the agent may do through afar
perm-navigate = Show and select in panels:
perm-mkdir = Create folders:
perm-copy = Copy:
perm-move = Move and rename:
perm-delete = Delete to the recycle bin:
perm-delete-permanent = Delete permanently:
perm-run-command = Run commands:
perm-allow = Allow
perm-confirm = Ask me
perm-deny = Deny
agent-settings-note = The agent's own Bash, Edit and Write are asked about by Claude Code.
agent-settings-restart = Command, arguments, IDE and events apply when the agent starts again.
settings-saved = Settings saved: { $path }
settings-save-failed = Settings not saved: { $error }
agent-menu-session = Session
agent-menu-mode = Mode
agent-menu-links = Links
agent-menu-view = View
agent-menu-new = &New session in the panel's folder
agent-menu-resume = &Continue a session of the folder…
agent-menu-move = Move the session to the panel's &folder
agent-menu-restart = R&estart the agent
agent-menu-rename = Re&name…
agent-menu-compact = Co&mpact context (/compact)…
agent-menu-clear = C&lear (/clear)
agent-menu-interrupt = &Interrupt the turn
agent-mode-default = Permissions: &default
agent-mode-accept-edits = Permissions: &accept edits
agent-mode-plan = Permissions: &plan
agent-mode-auto = Permissions: a&uto
agent-mode-dont-ask = Permissions: don't as&k
agent-mode-bypass = Permissions: &bypass
agent-menu-model = Model { $model }
agent-menu-model-other = Other &model…
agent-effort-low = Effort: low
agent-effort-medium = Effort: medium
agent-effort-high = Effort: high
agent-effort-xhigh = Effort: extra high
agent-effort-max = Effort: max
agent-menu-on-demand = Observing: &on demand
agent-menu-live = Observing: &live
agent-menu-ide = afar as the agent's &IDE
agent-menu-channels = afar's &events wake the agent (Channels)
agent-menu-ide-log = IDE protocol (ide.&log)
agent-menu-journal = Session &journal
agent-menu-settings = Agent and &permissions…
agent-menu-top = Agent pane at the &top
agent-menu-bottom = Agent pane at the &bottom
agent-menu-taller = &Taller
agent-menu-shorter = &Shorter
agent-menu-hide = &Hide
agent-rename-title = Session name
agent-rename-prompt = The agent session's name (/rename):
agent-model-title = Model
agent-model-prompt = The agent's model (/model), e.g. claude-opus-5-5:
agent-sessions-title = Sessions: { $dir }
agent-no-sessions = No saved sessions in { $dir }
confirm-agent = A&gent session actions (afar)
agent-confirm-title = Agent
agent-compact-title = Compact the agent's context (/compact)
agent-compact-prompt = What to keep when compacting (optional):
agent-compact-button = &Compact
agent-confirm-compact-note = The conversation is replaced with a summary: the agent loses the details of earlier messages.
agent-confirm-clear = Clear the agent's context (/clear)?
agent-confirm-clear-note = The agent starts a new session with an empty context; the current one stays on disk and can be continued.
agent-confirm-end = End the running agent?
agent-confirm-end-note = The current turn is interrupted; the conversation is kept and can be continued.
agent-confirm-bypass = Bypass permissions: the agent will run any command and make any edit without asking.
history-open-failed = The history did not open (kept until exit): { $error }
ac-command-line = AutoComplete in the &command line
ac-sources = Where matches come from
ac-source-history = History:
ac-source-files = Files and folders:
ac-source-variables = Environment variables:
ac-source-programs = Programs on PATH:
ac-use-always = always
ac-use-ctrl-space = only on Ctrl+Space
ac-use-never = never
history-filter = filter: { $filter }
history-passive-panel = Passive panel
history-folders = Folders
ac-suggest = Suggest as you type:
ac-suggest-ghost = the rest in grey
ac-suggest-list = a list (as in Far)
ac-suggest-off = nothing
ac-fuzzy = &Fuzzy matches (letters in order)
completion-passive-panel = Passive panel
