#!/usr/bin/env bash
# mcp_call.sh <session_dir> <tool> <json-arguments>
# Calls a tool of a running afar's MCP server and prints the result text;
# a picture in the result is saved to $MCP_IMAGE (or %TEMP%\mcp_image_N.png).
# Example: tools/mcp_call.sh "$(ls -td "$LOCALAPPDATA"/afar/sessions/* | head -1)" afar_state '{}'
# curl here is the Windows binary: keep temporary files under %TEMP%.

d="$1"; tool="$2"; args="$3"
cfg=$(python -I -c "import json,sys;c=json.load(open(sys.argv[1]))['mcpServers']['afar'];sys.stdout.write(c['url']+' '+c['headers']['Authorization'])" "$d/mcp.json")
URL=${cfg%% *}
TOK=${cfg#* }
H=(-H "Authorization: $TOK" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream")
hdr="$TEMP/mcp_hdr_$$.txt"
curl -s -D "$hdr" -o "$TEMP/mcp_null.txt" -X POST "$URL" "${H[@]}" -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
SID=$(grep -i "mcp-session-id" "$hdr" | cut -d' ' -f2 | tr -d '\r')
curl -s -o "$TEMP/mcp_null.txt" -X POST "$URL" "${H[@]}" -H "Mcp-Session-Id: $SID" -d '{"jsonrpc":"2.0","method":"notifications/initialized"}'
curl -s -X POST "$URL" "${H[@]}" -H "Mcp-Session-Id: $SID" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args}}" \
  | grep '^data: {' | sed 's/^data: //' \
  | python -I -c "
import json, sys, base64, os
sys.stdout.reconfigure(encoding='utf-8')
r = json.loads(sys.stdin.read())['result']
if r.get('isError'):
    print('ERROR: ', end='')
for n, c in enumerate(r['content']):
    if c['type'] == 'image':
        # A picture goes to a file (MCP_IMAGE, or %TEMP%), its path is printed.
        path = os.environ.get('MCP_IMAGE') or os.path.join(os.environ['TEMP'], 'mcp_image_%d.png' % n)
        open(path, 'wb').write(base64.b64decode(c['data']))
        print('[image: ' + path + ']')
    else:
        print(c.get('text', ''))
"
rm -f "$hdr"
