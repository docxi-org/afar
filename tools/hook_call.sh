#!/usr/bin/env bash
# hook_call.sh <session_dir> <event> [json-input]
# Calls a hook endpoint of a running afar as Claude Code's hook would
# (`afar hook <event>`), without a model: stop, notification, user-prompt, …
# Example: tools/hook_call.sh "$(ls -td "$LOCALAPPDATA"/afar/sessions/* | head -1)" notification '{"message":"Claude needs your permission to use Bash"}'
# curl here is the Windows binary: keep temporary files under %TEMP%.

d="$1"; event="$2"; input="${3:-{\}}"
cfg=$(python -I -c "import json,sys;c=json.load(open(sys.argv[1]))['mcpServers']['afar'];sys.stdout.write(c['url']+' '+c['headers']['Authorization'])" "$d/mcp.json")
URL=${cfg%% *}
TOK=${cfg#* }
curl -s -X POST "${URL%/mcp}/hook/$event" -H "Authorization: $TOK" -d "$input"
