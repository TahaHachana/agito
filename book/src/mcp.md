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
| Perception | `snapshot`, `screenshot` |
| Interaction | `click`, `type`, `press`, `press_key`, `hover`, `focus`, `clear`, `select_option`, `set_checked`, `scroll_into_view`, `wait_for_element`, `wait_for` |
| Tabs | `tab_list`, `tab_open`, `tab_select`, `tab_close` |
| Scripting | `evaluate` |
| Browser | `browser_status` |

A tool that fails returns an error result whose message says what to do next, such as taking a
fresh snapshot, rather than a protocol error.

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
- **Folding:** text is folded into the element it names, so `button "Pay"` is one line.
  Text that adds to the name follows a colon (`listitem: Item one`), and text between elements
  gets a `text:` line of its own. An unnamed wrapper with a single child steps aside for it.

`root_ref` roots a snapshot at one element, and `max_depth` limits how deep it goes.

Refs belong to the latest snapshot of the page. A navigation retires them, so take a new
snapshot after one, or pass `snapshot: true` to `navigate`, `click`, `type` and the other
actions to get one appended to the result.

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
  JSON form, such as `undefined` or a DOM node, come back as `null`. With a `ref`, the
  expression must be a function, and receives the element as its argument and as `this`:
  `el => el.scrollHeight`.
- **`wait_for`** takes one of `time` (milliseconds), `text` or `text_gone`.
- **`screenshot`** captures the viewport, or the whole page with `full_page: true`.

### One call at a time

The server runs tool calls one at a time, so two calls never act on the page at once. A client
that sends several calls without waiting for their results cannot rely on them running in the
order sent; to chain actions, wait for each result before sending the next, as agents do.

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
| `--allow-unrestricted-file-access` | off | Let tools open `file://` URLs |

Options left out fall back to [`agito.config.json`](reference/configuration.md) and `AGITO_*`
variables, read from the directory the server starts in, as tests do.

**`file://` is blocked** by default: a page could otherwise talk an agent into reading the
machine's files back through a snapshot.

Stdout carries the protocol, so the server writes its own messages to stderr.
