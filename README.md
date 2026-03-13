# log-wrangler

<p align="center">
  <img src="https://github.com/timothyb89/log-wrangler/blob/master/docs/screenshot.png?raw=true" alt="log-wrangler screenshot"/>
</p>


This is a CLI utility to quickly browse, filter, and search large volumes of
logs directly on your local machine.

It can ingest and stream logs from a variety of sources and in many formats -
including custom formats - and displays them for easy viewing and rapid
filtering.

It's both performant and efficient: it can ingest nearly 1 million log lines in
a few seconds over `ssh` with under 500 MiB of memory. Filtering this many logs
is nearly instantaneous, and scrolling through huge logs is just as quick as
browsing a small batch.

## Installation

Grab a binary for your platform from the
[Releases](https://github.com/timothyb89/log-wrangler/releases) page or run:

```
$ cargo install --git https://github.com/timothyb89/log-wrangler.git
```

## Usage

### Input sources

Sources are defined with the `--source` flag. You can add as many concurrent
sources as you like.

- `stdin://`: reads from standard input
- `grafana+loki+http://127.0.0.1:1234/api/datasources/proxy/uid/loki`: reads
  from the `loki` datasource of an unauthenticated Grafana instance at
  `http://127.0.0.1:1234`
- `grafana+loki+teleport://grafana/api/datasources/proxy/uid/loki`: uses your
  local [Teleport](https://github.com/gravitational/teleport) app credentials
  for the `grafana` app to stream without a local proxy

Sources can be named by specifying `name=uri://`

#### `stdin` input

The `stdin` source is the most flexible and can ingest logs from a wide variety
of sources. This input does not require an explicit source unless you want to
rename it.

Examples:
- `journalctl -u my-service -f | log-wrangler`
- `ssh -n user@host journalctl -u my-service -f | log-wrangler`
- `logcli query '{namespace="kube-system"}' --output=jsonl --since=1h --tail | log-wrangler`

One note: any apps that receive stdin that are then piped to `log-wrangler` will
compete to receive TTY input. For example, `ssh` will steal half the keystrokes
sent to the TUI! This isn't a bug in `long-wrangler` and is a function of how
the shell works. So: make sure to disable stdin in anything you pipe. Examples:
- `ssh -n host ... | log-wrangler`: ssh's `-n` flag disables stdin
- `ssh host ... < /dev/null | log-wrangler`: give the app an explicit /dev/null
  input

### Input formats

Logs can be emitted in a variety of formats, but the experience is best with
JSON logs. Where possible, configure your application to emit them directly.

`log-wrangler` supports several common JSON log schemas and will at least try to
treat everything as k/v pairs.

#### Regex format

For logs that don't match a known format, you can specify a custom regex
pattern using named capture groups. Use `?format=regex` on your source URI
and provide the pattern with `--format-regex`.

**Required capture groups:**
- `timestamp` — parsed as RFC 3339 or a Unix timestamp
- `level` — the log level (e.g. `info`, `error`, `debug`)

**Optional capture groups:**
- `message` — if present, used as the displayed message; otherwise the full
  line is shown
- Any other named groups become structured key/value fields

**Example:**

```
log-wrangler \
  --source 'stdin://?format=regex' \
  --format-regex '(?P<timestamp>\S+) (?P<level>\w+) (?P<message>.*)'
```

This would parse lines like:

```
2024-01-15T10:30:00Z info server started on port 8080
```

You can also capture additional structured fields:

```
--format-regex '(?P<timestamp>\S+) (?P<level>\w+) \[(?P<request_id>\S+)\] (?P<message>.*)'
```

If the pattern is invalid, `log-wrangler` will warn at startup and fall back
to automatic format detection.

#### Encapsulating formats

Some log formats encapsulate others. For example, Loki has a custom JSON
structure, and journald (both JSON and plaintext) wrap the actual content of the
app whose log they captured.

Supported encapsulated formats include:
- Loki
- journald
- journald-json

When an encapsulating format is detected, relevant labels are extracted and the
inner message is displayed. If you want to view the original log, including its
outer format, use the `v` key to toggle the TUI to the plaintext view.

### The `log-wrangler` TUI

The TUI is fundamentally a view of a node in the _filter tree_. The root of
the tree is the unfiltered view of all logs from all sources. As you add
filters, you build out a branch of the tree, but at any time you can view the
tree itself (with the `Tab` key), browse up to view a less-filtered version,
and if desired, create a new branch from that point.

Aside from simply viewing the latest logs, the TUI can perform many actions:
- Browsing (scrolling, inspecting a single message)
- Searching
- Filtering
- Managing sources
- Browsing and managing the filter tree

#### The filter tree

You can view the current filter tree with the `Tab` key. Each time a filter is
added, a new entry will be added to this view. You can change your view to any
node by selecting it and pressing `Enter`.

To create a new branch, move to a parent node (or the root) and create any new
filter.

To delete a node, press `p` to pop it. This returns your view to the next filter
up the tree.

#### Filters

There are several types of filters:
- Text: Press `/` and enter some text
- Regex: Press `/` to begin entering
- Inverted text or regex: Begin entering either a text or regex filter and press
  `Ctrl+N` (for "not")

  Inverted filters are especially useful for discarding noisy logs you don't
  want to see.
- Source: Added from the Sources dialog (`s`). Highlight a source and press
  `Enter`
- Time:
  - Press `>` to filter for only logs after the current time or the selected
    message
  - Press `<` to filter for only logs before the current time or selected
    message
  - These are especially useful if you're triggering an event in the log you're
    watching and want to quickly discard early spam, and then stop new logs from
    arriving in the current view.

#### Browsing and searching

There are several ways to browse efficiently

1. Scrolling: use the arrow keys or mouse wheel to scroll through individual
   messages. Trackpads work great!
2. Quick scrolling: press shift and scroll. This scrolls a set percentage
   through the entire log history rather than having to traverse each message
   individually.

   Note that some terminal emulators (e.g. iTerm) will capture shift+scroll.
   This can sometimes be disabled in your terminal settings, or you can use
   `Shift+Up` / `Shift+Down` to accomplish the same thing.
3. Searching: press `?` and enter a search. Toggle between regex and plaintext
   mode with `Ctrl+T`. This jumps to a message matching the search term.
4. Jumping: the `Home` key jumps to the first message, `End` removes the
   selection (inherently jumping to the most recent message)

#### Managing sources

You can add certain sources dynamically at runtime. Use the Sources dialog (`s`)
to add or remove sources. Currently, only Loki-via-Grafana sources can be added
dynamically.

## AI Use Disclaimer

This application was significantly written using Claude Code with human review
and testing.

## Credits

This was inspired by [woodchipper](https://github.com/HewlettPackard/woodchipper),
which I also wrote several years ago.
