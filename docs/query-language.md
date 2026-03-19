# Query Language

log-wrangler includes a query language for composing complex filters in a
single expression. It supports boolean logic (`and`, `or`, `not`), multiple
comparison operators, and access to all entry fields including dynamic labels.

## Entering query mode

From normal mode, press `/` (filter) or `?` (search), then press `Ctrl+T`
to cycle to `[QRY]` mode. The toolbar indicator shows the active mode.

- **Filter** (`/`) creates a child view in the filter tree, narrowing visible entries.
- **Search** (`?`) highlights matching entries for `n`/`N` navigation without narrowing scope.

Press `Enter` to apply, `Esc` to cancel.

## Fields

Every log entry has the following queryable fields:

| Field | Description |
|---|---|
| `message` | The raw log line text |
| `level` | Extracted log level (e.g. `debug`, `info`, `warn`, `error`) |
| `source` | Name of the log source that produced the entry |
| `timestamp` | Entry timestamp |
| `label.KEY` | A key-value attribute on the entry (see below) |

### Labels

`label.KEY` searches both source-level metadata (Loki stream labels, journald
fields like `hostname` or `unit`) and structured fields extracted from the
message content by classifiers (e.g. JSON fields like `request_id`, `target`,
`span`). There is no need to distinguish between these in queries; both are
searched.

`field.KEY` is accepted as a synonym for `label.KEY`.

## Operators

| Operator | Meaning | Value type |
|---|---|---|
| `==` | Exact equality | Quoted string or timestamp |
| `!=` | Not equal | Quoted string or timestamp |
| `contains` | Substring match (SIMD-accelerated) | Quoted string |
| `=~` | Regex match | Regex literal |
| `!~` | Regex non-match | Regex literal |
| `>` | Greater than | Timestamp (only on `timestamp` field) |
| `<` | Less than | Timestamp |
| `>=` | Greater than or equal | Timestamp |
| `<=` | Less than or equal | Timestamp |

## Values

**Quoted strings** are delimited by double quotes. Backslash escapes are
supported: `\"`, `\\`, `\n`, `\t`.

```
message contains "connection refused"
level == "error"
label.namespace == "kube-system"
```

**Regex literals** are delimited by `/`. Use `\/` to include a literal slash.
The regex syntax is Rust's `regex` crate (similar to RE2/PCRE without
backreferences).

```
message =~ /timeout.*retry/
message !~ /health_?check/
```

**Timestamps** are first-class unquoted values. Any ISO 8601 format is
accepted. A bare date is interpreted as midnight UTC.

```
timestamp > 2024-01-15T10:00:00Z
timestamp >= 2024-01-15
timestamp < 2024-01-16T00:00:00+02:00
```

## Boolean composition

Expressions can be combined with `and`, `or`, and `not`. Operator precedence
from highest to lowest:

1. `not`, `(?i)` (unary prefixes)
2. `and`
3. `or`

Use parentheses to override precedence:

```
level == "error" and message contains "timeout"
level == "error" or level == "warn"
not level == "debug"
(level == "error" or level == "warn") and message =~ /connection/
```

## Case-insensitive matching

Prefix an expression with `(?i)` to make all string comparisons within it
case-insensitive. This affects `==`, `!=`, `contains`, and regex operators
(regex gets the `(?i)` flag automatically).

```
(?i) message contains "error"
(?i) (level == "warn" and message contains "timeout")
```

`(?i)` is a unary prefix like `not`, so it applies to the immediately following
expression. To apply it to a compound expression, wrap it in parentheses.

## Grammar

```
expr       = or_expr
or_expr    = and_expr ("or" and_expr)*
and_expr   = unary ("and" unary)*
unary      = "not" unary | "(?i)" unary | atom
atom       = comparison | "(" expr ")"
comparison = field_ref operator value
field_ref  = "message" | "level" | "source" | "timestamp"
           | "label." IDENT | "field." IDENT
operator   = "==" | "!=" | "=~" | "!~" | "contains"
           | ">" | "<" | ">=" | "<="
value      = QUOTED_STRING | REGEX_LITERAL | TIMESTAMP
```

## Editor features

### Error highlighting

As you type, the query is parsed on every keystroke. If the input is invalid:

- Text before the error position renders normally.
- Text from the error position onward is highlighted in red.
- A descriptive error message appears inline (e.g. "expected quoted string",
  "expected operator").

This also applies to regex mode (`[RGX]`), where invalid regex patterns are
highlighted the same way.

### Tab completion

In query mode, pressing `Tab` completes the current token. Completions include:

- Keywords: `message`, `level`, `source`, `timestamp`, `and`, `or`, `not`,
  `contains`, `(?i)`
- The `label.` prefix, followed by all known label keys from the current
  log data (both source labels and structured fields)

Use `Up`/`Down` arrows to navigate completions. The list shows up to 5 items
at a time and scrolls to follow the selection, with a `(n/total)` position
indicator.

### Word navigation

`Alt+Left` and `Alt+Right` move the cursor by word boundaries. This works in
all three input modes (substring, regex, query).

## Examples

```
# Show only errors
level == "error"

# Errors mentioning timeouts
level == "error" and message contains "timeout"

# Entries from a specific namespace, case-insensitive
(?i) label.namespace == "production"

# Exclude health checks
not message =~ /health_?check/

# Time window
timestamp >= 2024-03-15T09:00:00Z and timestamp < 2024-03-15T10:00:00Z

# Complex: errors or warnings from a specific source, excluding noise
(level == "error" or level == "warn") and source == "api-gateway" and not message contains "heartbeat"
```
