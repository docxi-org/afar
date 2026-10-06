# afar's own strings (Far Manager's are in far/*.ftl).

objects = { $count ->
    [one] { $count } object
   *[other] { $count } objects
}

## Agent pane
agent-title = Agent · claude · { $status }
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
