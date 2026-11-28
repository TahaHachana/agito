# Driving a browser from an AI agent

`agito mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server. Any MCP
client, such as Claude Code, Claude Desktop or another agent, can launch it and drive a real
browser through agito's engine: navigate, read a page, act on its elements and capture it.

Agents read pages through the accessibility tree, not pixels. A snapshot is a compact,
labelled list of what is on the page, and each element worth acting on carries a ref that the
interaction tools take.

## Registering the server

Install the CLI and a browser:

```console
cargo install agito-cli
agito install
```

Then register `agito mcp` with your client. With Claude Code:

```console
claude mcp add agito -- agito mcp
```

Everything after `--` is the command the client runs, so server options go there too:

```console
claude mcp add agito -- agito mcp --headless false --viewport 1440x900
```

Clients configured by a JSON file, such as Claude Desktop, take the same command:

```json
{
  "mcpServers": {
    "agito": { "command": "agito", "args": ["mcp"] }
  }
}
```

If the client cannot find `agito`, give the absolute path to the binary as `command`
(`~/.cargo/bin/agito` by default).

The server starts its browser on the first call that needs one, and closes it when the client
disconnects. If the browser crashes, the call that raced the crash fails and the next call
starts a new browser, with nothing open.

## Checking the server by hand

The server speaks newline-delimited JSON-RPC on stdin and stdout, so a handshake piped into it
shows the tools it offers:

```console
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1.0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | agito mcp
```

## Tools

| Area | Tools |
|---|---|
| Navigation | `navigate`, `go_back`, `go_forward`, `reload` |
| Perception | `snapshot`, `screenshot`, `audit_accessibility`, `get_performance` |
| Interaction | `click`, `type`, `press`, `press_key`, `hover`, `focus`, `clear`, `select_option`, `set_checked`, `scroll_into_view`, `wait_for_element`, `wait_for` |
| Tabs | `tab_list`, `tab_open`, `tab_select`, `tab_close` |
| Frames | `frame_list`, `frame_select` |
| Scripting | `evaluate` |
| Dialogs | `handle_dialog` |
| Logs | `console_messages`, `network_requests`, `network_request` |
| Files | `upload_files` |
| Browser | `browser_status` |

A tool that fails returns an error result whose message says what to do next, such as taking a
fresh snapshot, rather than a protocol error.

### Added on request

Every tool a server lists is described to the client at the start of every conversation,
whether it is called or not. So the groups a session either leans on throughout or never
touches are left out until `--caps` names them:

| Group | `--caps` | Tools |
|---|---|---|
| Coordinate input | `coordinates` | `click_xy`, `hover_xy`, `move_xy`, `scroll_xy`, `drag`, `resize` |
| Isolated contexts | `contexts` | `context_list`, `context_create`, `context_select`, `context_close` |
| Request mocking | `routing` | `route_fulfill`, `route_abort`, `route_continue`, `unroute`, `route_list` |
| Attaching to a running browser | `--allow-attach` or `--connect` | `browser_attach` |

```console
claude mcp add agito -- agito mcp --caps coordinates,contexts,routing
```

Separate groups with commas or repeat the flag; `AGITO_MCP_CAPS` takes the same list, and the
flag outranks it. Naming a group only ever adds tools. A recording group (traces, HAR and
video) will follow once agito records traces and video.

### Snapshots and refs

`snapshot` prints one line per node, indented to show nesting: the role, the accessible name in
quotes, then attributes in brackets.

```text
- RootWebArea "Shop" [ref=e1]
  - main
    - heading "Checkout" [ref=e2] [level=1]
    - LabelText
      - text: Email
      - textbox "Email" [ref=e3]
    - checkbox "Accept terms" [ref=e4]
    - combobox [ref=e5] [value="Small"]
      - MenuListPopup
        - option "Small" [ref=e6] [selected]
        - option "Large" [ref=e7]
    - button "Pay" [ref=e8]
    - link "terms" [ref=e9] [url=https://shop.test/terms]
```

- **Refs** (`e1`, `e2`, ...) go to every node an agent might act on: interactive roles
  (buttons, links, fields, options, rows, cells, ...), named nodes, focusable nodes and
  iframes. Wrappers, labels and text carry none.
- **Attributes:** `[level=N]` on headings, `[url=...]` on links, `[value="..."]` on form
  controls, and the states `[disabled]`, `[readonly]`, `[required]`, `[checked]`,
  `[selected]`, `[expanded]` and `[pressed]` when set.
- **Folding:** text is folded into the element it names, so `button "Pay"` is one line, and
  a field's typed text is shown once, as its value.
  Text that adds to the name follows a colon (`listitem: Item one`), and text between elements
  gets a `text:` line of its own. An unnamed wrapper with a single child steps aside for it.

`root_ref` roots a snapshot at one element, and `max_depth` limits how deep it goes.

Refs belong to the latest snapshot of the page. A navigation retires them, so take a new
snapshot after one, or pass `snapshot: true` to `navigate`, `click`, `type` and the other
actions to get one appended to the result.

### Frames

A page snapshot prints the frames the page hosts, each inside the `iframe` element holding
it, so a payment form in a frame reads like the rest of the page:

```text
- main
  - heading "Checkout" [ref=e2] [level=1]
  - Iframe "Payment" [ref=e3] [frame=1]
    - textbox "Card number" [ref=f1e1]
    - button "Pay" [ref=f1e2]
```

`[frame=N]` is the frame's index, and the prefix on every ref inside it: `f1e2` is the second
addressable element of frame 1. Pass it to `click` or `type` like any other ref; no frame needs
selecting first. A frame inside a frame prints inside its own parent, with its own index.

Ten frames are printed by default; `max_frames` on `snapshot` changes that. A frame past the
limit, or whose element is hidden, is left out, and the snapshot says how many were.

- **`frame_list`** lists the frames with their depth: index 0 is the page, and each frame
  follows the frame holding it.
- **`frame_select`** scopes `snapshot`, `evaluate`, selectors and the `wait_for` text waits to
  one frame; `frame_select 0` returns to the page. A scoped snapshot describes that frame on
  its own, with plain `e1`, `e2` refs. The scope resets when the page navigates or another tab
  is selected.

### Refs or selectors

Every tool that takes a `ref` takes a selector in its place: CSS by default, or `xpath=`,
`text=`, `role=` or `data-testid=` (see [Locators](writing/locators.md)). A selector needs no
snapshot. Anything shaped like a ref (`e5`, or `f1e5`) is read as one, so a ref the latest
snapshot no longer holds is reported as stale, not run as a selector that matches nothing.

### Notes on individual tools

- **`click`** takes `button` (`left`, `right` or `middle`), `modifiers` (any of `Alt`,
  `Control`, `Meta`, `Shift`) and `double`. Every click goes through the same actionability
  checks as in a test: the element is scrolled into view and must be visible, enabled, still
  and the one receiving the click.
- **`type`** sets the value in one step. `slowly: true` types key by key, for fields that
  react to each keystroke, and `submit: true` presses Enter afterwards.
- **`evaluate`** returns the value under a `result` key, whatever its shape; values with no
  JSON form, such as `undefined` or a DOM node, come back as `null`. Without a `ref` it also
  takes statements (`const n = 2; n * 3`), returning the last one's value. With a `ref`, the
  expression must be a function, and receives the element as its argument and as `this`:
  `el => el.scrollHeight`.
- **`wait_for`** takes one of `time` (milliseconds), `text` or `text_gone`.
- **`screenshot`** captures the viewport, or the whole page with `full_page: true`.

### What an action reports

An action returns one line saying what it did, and under it a row for anything it changed:

```text
Clicked e3
Page: https://shop.test/checkout | Checkout
New tab opened: [1] https://shop.test/terms
Console: 1 error, 1 page error. Read them with console_messages since=14.
Refs from the last snapshot no longer address this page: it navigated. Take a new snapshot.
```

The rows say where the page went or what it renamed itself to, tabs the action opened (with
the index `tab_select` takes), errors and uncaught page errors it logged (with the cursor that
reads exactly those), a dialog left open, and refs retired by the page or one of its frames
navigating. An action that changes nothing surprising stays one line.

The browser accepts a click before the page has acted on it, so the result waits briefly for
the page to show what the click did: 500 ms by default, ending early when a tab appears.
`--settle` changes the wait; `--settle 0` reports the instant the action returns.

### Dialogs

A JavaScript dialog stops the page answering anything until it is handled. An action that
raises one comes back at once and says so:

```text
The action opened a confirm dialog: "Delete this?". Call handle_dialog to accept or dismiss it.
```

Until it is answered, every result starts with a line saying the dialog is open, and actions
and page reads (`snapshot`, `evaluate`, `screenshot`) are refused rather than sent to a page
that cannot answer. `handle_dialog` takes `accept`, and for a prompt, the `text` to enter. The
action that raised the dialog finishes once it is answered.

`--dialogs accept` or `--dialogs dismiss` answers every dialog at once instead.

### Console and network logs

Each tab keeps the last 250 console messages and uncaught errors (`console_messages`), and the
last 250 finished or failed requests (`network_requests`), from the moment it opened. Reading
leaves them in place. Each read ends with `next=N`; pass it as `since` to read only what came
after:

```text
console_messages()
  -> [error] Cannot read properties of null
     [pageerror] Error: uncaught boom
     next=3
```

`network_requests` numbers each line, `[5] POST 201 https://shop.test/api/cart (Fetch)`, and
`network_request` takes that number for the request's headers, request body, and response
body while the browser still has it. Bodies that are not text, or longer than 4000
characters, are described rather than shown.

### Accessibility audits

`audit_accessibility` runs the [accessibility rules](writing/accessibility.md) on the page and
its frames. The text lists each violation; the structured result gives each one's `ruleId`,
`severity`, `message`, `nodeRole`, `nodeName`, `nodeText`, `selector`, the `frameUrl` of a
frame, and a `ref` when the element took one, so an agent can act on it directly. It takes a
fresh snapshot to do so, which replaces the refs in hand. `min_severity` (`error`, `warning` or
`info`) leaves out the less severe.

### Coordinate input

Some applications draw their interface on a `<canvas>`, so the accessibility tree has nothing
to address. A snapshot that finds nothing says so, and with `--caps coordinates` the way
forward is perception by screenshot:

1. `screenshot` the page and find the control.
2. Act on its position with `click_xy`, `hover_xy`, `scroll_xy` or `drag`. Coordinates are CSS
   pixels in the viewport, the space screenshots and `getBoundingClientRect()` use.
3. If a target sits beyond the viewport's edge, `resize` it first (1280x800 by default).

The input is trusted browser input, as for the ref-based tools, and the browser's own hit
test picks the target. `drag` takes either elements (`start_ref` and `end_ref`, each scrolled
into view first) or points (`start_x`, `start_y`, `end_x`, `end_y`), and always moves through
intermediate points (`steps`, 10 by default), with `hold_ms` for libraries that wait before
starting a drag.

### Isolated contexts

A context has its own cookies and storage, so two contexts are two users. A session that
never names one uses `default`, created on first use. `context_create` makes a context and
switches to it, refusing a name already open; `context_select` switches; `context_close` closes
a context and its tabs, and `default` becomes active if it was the one closed.

`tab_list` numbers every tab across contexts, naming each tab's context once there is more
than one, and `tab_select` switches to a tab's context. `tab_open` opens in the active one.

### Request mocking

A mock answers a request before it reaches the network, which reaches states the server will
not produce on demand: an error, an empty list, a slow dependency.

- `route_fulfill` answers with `status` (200), `body`, `content_type` and `headers`.
- `route_abort` fails the request, with an `error_code` such as `connectionrefused` or
  `timedout`.
- `route_continue` sends it on with the `url`, `method`, `headers` or `post_data` replaced.
- `unroute` removes a pattern's rule, and `route_list` lists them as `pattern -> action`.

A pattern matches a URL it equals; with a `*`, as a glob over the whole URL, each `*` any run of
characters; otherwise, as a substring. So `*/api/status.json` and `/api/` both match
`https://shop.test/api/status.json`. Rules belong to the active context: they cover all its
tabs, and survive navigation. When several match, the one registered most recently answers,
and re-registering a pattern replaces its rule without moving it.

```text
route_fulfill(url_pattern: "*/api/status.json", body: "{\"overall\": \"degraded\"}")
  -> Mocking '*/api/status.json' with status 200.
```

A redirect to a `file:` URL, and a `route_continue` to one, are refused unless the server
allows file access.

### Uploads

`upload_files` sets a file input's files from local paths. Each file is read on the machine
running the server, and must lie inside one of the roots the MCP client reported (the project
folders it works in), or the server's working directory when the client reports none. A path
that climbs out with `..`, or a link pointing out, is refused like any other.

### Performance

`get_performance` reads the page's [Core Web Vitals](writing/performance.md), its JS heap size
and DOM node count, and each layout shift with the elements that moved. The text summarises
them; the structured result has each value. LCP and CLS keep changing while a page loads, and
INP needs an interaction, so read again after acting.

### One call at a time

The server runs tool calls one at a time, so two calls never act on the page at once. A client
that sends several calls without waiting for their results cannot rely on them running in the
order sent; to chain actions, wait for each result before sending the next, as agents do.

## Driving a browser that is already running

By default the server starts a browser and closes it when the session ends. Pointed at one
somebody else started, it drives what is open there and leaves it running, which makes a
signed-in profile, or a browser kept warm between runs, drivable:

```console
google-chrome --remote-debugging-port=9222 --user-data-dir="$HOME/agent-profile"
claude mcp add agito -- agito mcp --connect http://127.0.0.1:9222
```

The session's `default` context is then the browser's own, with its open tabs; the first one
is active. Nothing is emulated on it, so `--viewport`, `--user-agent`, `--locale` and
`--timezone` do not apply there, nor do the options that describe starting a browser; the
server says so on startup. If the connection drops, the next call connects again to the same
endpoint. `tab_close`, and `context_close` with `--caps contexts`, act on somebody's working
state here, though closing the default context only lets go of it.

`browser_attach` attaches mid-session, for when the endpoint is not known when the client is
configured. It is listed only with `--allow-attach` or `--connect`: a running browser may hold
somebody's signed-in sessions, so which browser a session drives is the operator's decision.
Attaching closes the browser the server started, and refs, mocks, logs and named contexts do
not carry over.

A debugging port gives complete control of the browser to anything that can reach it; keep it
on loopback.

## HTTP transport

For remote clients, the same tools are served over Streamable HTTP:

```console
# Loopback only, no token needed
agito mcp --http

# Reachable from other machines: binding anything but loopback needs a token
agito mcp --http --host 0.0.0.0 --port 8931 --token "$AGITO_MCP_TOKEN"
```

The endpoint is `http://host:port/mcp`. Each client session gets a browser of its own, closed
when the session ends or after 30 minutes without a request. With a token, every request must
carry `Authorization: Bearer <token>`, compared in constant time; without one, the server only
answers requests addressed to a loopback host, which keeps web pages from reaching it through
DNS rebinding.

MCP protocol version 2026-07-28 has no sessions over HTTP, so a browser could not outlast a
single request; the HTTP server offers versions up to 2025-11-25, which clients negotiate to.

stdio inherits the trust of the user who started it. HTTP does not: treat the token as a
credential, and prefer loopback or a trusted network.

## Options

| Option | Default | Meaning |
|---|---|---|
| `--headless [BOOL]` | `true` | Run the browser without a window; `--headless false` shows it |
| `--channel` | | `chromium`, `chrome` or `edge` |
| `--executable-path` | | Start this browser binary |
| `--browser-arg=ARG` | | An extra browser argument; repeat for more. In a container running as root, `--browser-arg=--no-sandbox` |
| `--user-data-dir` | | A browser profile directory, so cookies and sign-ins persist between runs |
| `--viewport WxH` | `1280x800` | The viewport of every page |
| `--user-agent`, `--locale`, `--timezone` | browser defaults | What every page reports |
| `--timeout MS` | the context's action timeout | How long an element action waits for its element |
| `--navigation-timeout MS` | the context's navigation timeout | How long a navigation waits |
| `--settle MS` | `500` | How long an action waits for the page to show what it did |
| `--dialogs` | `ask` | `ask` leaves a dialog for `handle_dialog`; `accept` or `dismiss` answers it at once |
| `--caps GROUPS` | none | Optional tool groups: `coordinates`, `contexts`, `routing`. Also `AGITO_MCP_CAPS` |
| `--allow-unrestricted-file-access` | off | Let tools open `file://` URLs, and upload files from anywhere |
| `--connect ENDPOINT` | | Drive the browser already running at this debugging endpoint instead of starting one |
| `--allow-attach` | off | List `browser_attach`; implied by `--connect` |
| `--http` | off | Serve Streamable HTTP instead of stdio |
| `--host`, `--port` | `127.0.0.1`, `8931` | Where `--http` listens |
| `--token` | | The bearer token for `--http`; also `AGITO_MCP_TOKEN`. Required off loopback |

Options left out fall back to [`agito.config.json`](reference/configuration.md) and `AGITO_*`
variables, read from the directory the server starts in, as tests do.

**`file://` is blocked** by default, and uploads read only from the client's roots: a page
could otherwise talk an agent into reading the machine's files back.

Stdout carries the protocol, so the server writes its own messages to stderr.
