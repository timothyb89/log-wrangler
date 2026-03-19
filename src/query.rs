use memchr::memmem;
use winnow::combinator::{alt, cut_err, repeat, terminated};
use winnow::error::{ContextError, ErrMode, ModalResult, StrContext, StrContextValue};
use winnow::token::{any, take_while};
use winnow::Parser;

/// A parsed query expression.
#[derive(Debug, Clone)]
pub(crate) enum QueryExpr {
    Compare {
        target: QueryTarget,
        op: QueryOp,
    },
    And(Box<QueryExpr>, Box<QueryExpr>),
    Or(Box<QueryExpr>, Box<QueryExpr>),
    Not(Box<QueryExpr>),
    CaseInsensitive(Box<QueryExpr>),
}

/// What field a comparison targets.
#[derive(Debug, Clone)]
pub(crate) enum QueryTarget {
    Message,
    Level,
    Source,
    Timestamp,
    /// A key-value attribute on the entry. Searches both source labels (Loki
    /// stream labels, journald fields) and structured fields extracted by
    /// classifiers from the message content. Accessed as `label.X` in queries.
    Label(String),
}

/// A compiled comparison operation.
#[derive(Debug, Clone)]
pub(crate) enum QueryOp {
    Eq(String),
    NotEq(String),
    Contains(CompiledSubstring),
    Regex(regex::Regex),
    NotRegex(regex::Regex),
    After(jiff::Timestamp),
    Before(jiff::Timestamp),
    AtOrAfter(jiff::Timestamp),
    AtOrBefore(jiff::Timestamp),
}

/// Pre-compiled SIMD-accelerated substring search.
#[derive(Clone)]
pub(crate) struct CompiledSubstring {
    pub text: String,
    pub finder: memmem::Finder<'static>,
}

impl std::fmt::Debug for CompiledSubstring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CompiledSubstring")
            .field(&self.text)
            .finish()
    }
}

impl CompiledSubstring {
    fn new(text: String) -> Self {
        let finder = memmem::Finder::new(text.as_bytes()).into_owned();
        Self { text, finder }
    }
}

/// A parse error with byte offset for TUI highlighting.
#[derive(Debug, Clone)]
pub(crate) struct ParseError {
    pub offset: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

impl QueryExpr {
    /// Evaluate this expression against a log entry using full Arena access.
    pub fn matches(
        &self,
        entry: &crate::log::LogEntry,
        arena: &crate::log::Arena,
        ci: bool,
    ) -> bool {
        let ctx = crate::log::IngestContext {
            rodeo: &arena.rodeo,
            labels: &arena.labels,
            structured_fields: &arena.structured_fields,
            source_names: &arena.source_names,
        };
        self.matches_ctx(entry, &ctx, ci)
    }

    /// Evaluate this expression using an `IngestContext` (avoids borrow conflicts
    /// when the LogView is part of the Arena being mutated).
    pub fn matches_ctx(
        &self,
        entry: &crate::log::LogEntry,
        ctx: &crate::log::IngestContext,
        ci: bool,
    ) -> bool {
        match self {
            QueryExpr::And(a, b) => a.matches_ctx(entry, ctx, ci) && b.matches_ctx(entry, ctx, ci),
            QueryExpr::Or(a, b) => a.matches_ctx(entry, ctx, ci) || b.matches_ctx(entry, ctx, ci),
            QueryExpr::Not(inner) => !inner.matches_ctx(entry, ctx, ci),
            QueryExpr::CaseInsensitive(inner) => inner.matches_ctx(entry, ctx, true),
            QueryExpr::Compare { target, op } => eval_compare(target, op, entry, ctx, ci),
        }
    }
}

fn eval_compare(
    target: &QueryTarget,
    op: &QueryOp,
    entry: &crate::log::LogEntry,
    ctx: &crate::log::IngestContext,
    ci: bool,
) -> bool {
    let rodeo = ctx.rodeo;

    match target {
        QueryTarget::Timestamp => eval_timestamp_op(op, entry),
        QueryTarget::Message => {
            let msg = rodeo.messages.resolve(&entry.message);
            eval_string_op(op, msg, ci)
        }
        QueryTarget::Level => {
            if let Some(ref level_spur) = entry.level {
                let level = rodeo.label_values.resolve(level_spur);
                eval_string_op(op, level, ci)
            } else {
                false
            }
        }
        QueryTarget::Source => {
            let name = ctx
                .source_names
                .get(entry.source_id as usize)
                .map(|s| s.as_str())
                .unwrap_or("");
            eval_string_op(op, name, ci)
        }
        QueryTarget::Label(key) => {
            // Search both source labels and structured fields (keys are in label_keys rodeo).
            let in_labels = (0..entry.labels_length).any(|i| {
                let (k, v) = &ctx.labels[entry.labels_start + i];
                let key_str = rodeo.label_keys.resolve(k);
                let key_matches = if ci {
                    key_str.eq_ignore_ascii_case(key)
                } else {
                    key_str == key
                };
                key_matches && eval_string_op(op, rodeo.label_values.resolve(v), ci)
            });
            if in_labels {
                return true;
            }
            (0..entry.structured_fields_length).any(|i| {
                let (k, v) =
                    &ctx.structured_fields[entry.structured_fields_start + i];
                let key_str = rodeo.label_keys.resolve(k);
                let key_matches = if ci {
                    key_str.eq_ignore_ascii_case(key)
                } else {
                    key_str == key
                };
                key_matches && eval_string_op(op, rodeo.label_values.resolve(v), ci)
            })
        }
    }
}

fn eval_string_op(op: &QueryOp, text: &str, ci: bool) -> bool {
    match op {
        QueryOp::Eq(pattern) => {
            if ci {
                text.eq_ignore_ascii_case(pattern)
            } else {
                text == pattern
            }
        }
        QueryOp::NotEq(pattern) => {
            if ci {
                !text.eq_ignore_ascii_case(pattern)
            } else {
                text != pattern
            }
        }
        QueryOp::Contains(cs) => {
            if ci {
                let lower_text = text.to_ascii_lowercase();
                let lower_pat = cs.text.to_ascii_lowercase();
                let finder = memmem::Finder::new(lower_pat.as_bytes());
                finder.find(lower_text.as_bytes()).is_some()
            } else {
                cs.finder.find(text.as_bytes()).is_some()
            }
        }
        QueryOp::Regex(re) => re.is_match(text),
        QueryOp::NotRegex(re) => !re.is_match(text),
        QueryOp::After(_) | QueryOp::Before(_) | QueryOp::AtOrAfter(_) | QueryOp::AtOrBefore(_) => false,
    }
}

fn eval_timestamp_op(op: &QueryOp, entry: &crate::log::LogEntry) -> bool {
    let ts = entry.timestamp.timestamp();
    match op {
        QueryOp::After(t) => ts > *t,
        QueryOp::Before(t) => ts < *t,
        QueryOp::AtOrAfter(t) => ts >= *t,
        QueryOp::AtOrBefore(t) => ts <= *t,
        QueryOp::Eq(s) => {
            if let Ok(t) = s.parse::<jiff::Timestamp>() {
                ts == t
            } else {
                false
            }
        }
        QueryOp::NotEq(s) => {
            if let Ok(t) = s.parse::<jiff::Timestamp>() {
                ts != t
            } else {
                true
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

fn backtrack() -> ErrMode<ContextError> {
    ErrMode::Backtrack(ContextError::new())
}

fn cut(msg: &'static str) -> ErrMode<ContextError> {
    let mut err = ContextError::new();
    err.push(StrContext::Expected(StrContextValue::Description(msg)));
    ErrMode::Cut(err)
}

/// Parse a query string into a `QueryExpr`.
pub(crate) fn parse_query(input: &str) -> Result<QueryExpr, ParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ParseError {
            offset: 0,
            message: "empty query".to_string(),
        });
    }

    let mut stream = input;
    match terminated(parse_expr, ws).parse_next(&mut stream) {
        Ok(e) => {
            if stream.is_empty() {
                Ok(e)
            } else {
                let offset = input.len() - stream.len();
                Err(ParseError {
                    offset,
                    message: "unexpected input".to_string(),
                })
            }
        }
        Err(err) => {
            let offset = input.len() - stream.len();
            let message = extract_error_message(&err);
            Err(ParseError { offset, message })
        }
    }
}

/// Extract a human-readable message from a winnow error's context chain.
fn extract_error_message(err: &ErrMode<ContextError>) -> String {
    let ctx = match err {
        ErrMode::Backtrack(c) | ErrMode::Cut(c) => c,
        ErrMode::Incomplete(_) => return "incomplete input".to_string(),
    };

    // Collect Expected items from the context chain.
    let mut expected: Vec<String> = Vec::new();
    let mut label: Option<&str> = None;
    for item in ctx.context() {
        match item {
            StrContext::Expected(StrContextValue::Description(desc)) => {
                expected.push((*desc).to_string());
            }
            StrContext::Expected(StrContextValue::StringLiteral(s)) => {
                expected.push(format!("`{}`", s));
            }
            StrContext::Expected(StrContextValue::CharLiteral(c)) => {
                expected.push(format!("`{}`", c));
            }
            StrContext::Label(l) => {
                label = Some(l);
            }
            _ => {}
        }
    }

    if !expected.is_empty() {
        expected.dedup();
        format!("expected {}", expected.join(" or "))
    } else if let Some(l) = label {
        format!("invalid {}", l)
    } else {
        "parse error".to_string()
    }
}

// --- Combinators ---

fn ws(input: &mut &str) -> ModalResult<()> {
    take_while(0.., |c: char| c == ' ' || c == '\t')
        .void()
        .parse_next(input)
}

fn ws1(input: &mut &str) -> ModalResult<()> {
    take_while(1.., |c: char| c == ' ' || c == '\t')
        .void()
        .parse_next(input)
}

/// Match a keyword ensuring it's not followed by word characters.
fn kw<'i>(word: &'static str, input: &mut &'i str) -> ModalResult<()> {
    winnow::token::literal(word).void().parse_next(input)?;
    if let Some(c) = input.chars().next() {
        if c.is_alphanumeric() || c == '_' {
            return Err(backtrack());
        }
    }
    Ok(())
}

fn parse_expr(input: &mut &str) -> ModalResult<QueryExpr> {
    parse_or_expr(input)
}

fn parse_or_expr(input: &mut &str) -> ModalResult<QueryExpr> {
    let first = parse_and_expr(input)?;
    let rest: Vec<QueryExpr> = repeat(0.., |input: &mut &str| {
        ws.parse_next(input)?;
        kw("or", input)?;
        ws1.parse_next(input)?;
        parse_and_expr(input)
    })
    .parse_next(input)?;
    Ok(rest
        .into_iter()
        .fold(first, |acc, rhs| QueryExpr::Or(Box::new(acc), Box::new(rhs))))
}

fn parse_and_expr(input: &mut &str) -> ModalResult<QueryExpr> {
    let first = parse_unary(input)?;
    let rest: Vec<QueryExpr> = repeat(0.., |input: &mut &str| {
        ws.parse_next(input)?;
        kw("and", input)?;
        ws1.parse_next(input)?;
        parse_unary(input)
    })
    .parse_next(input)?;
    Ok(rest
        .into_iter()
        .fold(first, |acc, rhs| QueryExpr::And(Box::new(acc), Box::new(rhs))))
}

fn parse_unary(input: &mut &str) -> ModalResult<QueryExpr> {
    ws.parse_next(input)?;

    // Try "not" prefix (lookahead via saved slice).
    {
        let mut probe = *input;
        if kw("not", &mut probe).is_ok() {
            *input = probe;
            ws1.parse_next(input)?;
            let inner = parse_unary(input)?;
            return Ok(QueryExpr::Not(Box::new(inner)));
        }
    }

    // Try "(?i)" prefix.
    if input.starts_with("(?i)") {
        "(?i)".parse_next(input)?;
        ws1.parse_next(input)?;
        let inner = parse_unary(input)?;
        return Ok(QueryExpr::CaseInsensitive(Box::new(inner)));
    }

    parse_atom(input)
}

fn parse_atom(input: &mut &str) -> ModalResult<QueryExpr> {
    ws.parse_next(input)?;

    // Try parenthesized expression.
    if input.starts_with('(') && !input.starts_with("(?i)") {
        '('.parse_next(input)?;
        let inner = cut_err(parse_expr)
            .context(StrContext::Label("parenthesized expression"))
            .parse_next(input)?;
        ws.parse_next(input)?;
        cut_err(')')
            .context(StrContext::Expected(StrContextValue::CharLiteral(')')))
            .parse_next(input)?;
        return Ok(inner);
    }

    parse_comparison(input)
}

/// Operators as an enum to avoid lifetime issues with returning &str.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    NotEq,
    RegexMatch,
    NotRegexMatch,
    Contains,
    Gt,
    Lt,
    Gte,
    Lte,
}

fn parse_operator(input: &mut &str) -> ModalResult<Op> {
    alt((
        ">=".map(|_| Op::Gte),
        "<=".map(|_| Op::Lte),
        "==".map(|_| Op::Eq),
        "!=".map(|_| Op::NotEq),
        "=~".map(|_| Op::RegexMatch),
        "!~".map(|_| Op::NotRegexMatch),
        ">".map(|_| Op::Gt),
        "<".map(|_| Op::Lt),
    ))
    .parse_next(input)
    .or_else(|_: ErrMode<ContextError>| {
        // Try "contains" keyword.
        kw("contains", input)?;
        Ok(Op::Contains)
    })
}

fn parse_comparison(input: &mut &str) -> ModalResult<QueryExpr> {
    let target = parse_field_ref
        .context(StrContext::Expected(StrContextValue::Description(
            "field (message, level, source, timestamp, label.*)",
        )))
        .parse_next(input)?;
    ws.parse_next(input)?;
    let op = parse_operator
        .context(StrContext::Expected(StrContextValue::Description(
            "operator (==, !=, =~, !~, contains, >, <, >=, <=)",
        )))
        .parse_next(input)?;
    ws.parse_next(input)?;

    // After field + operator, we're committed — use cut_err for the value.
    let query_op = match (&target, op) {
        (QueryTarget::Timestamp, Op::Gt) => {
            QueryOp::After(cut_err(parse_timestamp_value)
                .context(StrContext::Expected(StrContextValue::Description("timestamp")))
                .parse_next(input)?)
        }
        (QueryTarget::Timestamp, Op::Lt) => {
            QueryOp::Before(cut_err(parse_timestamp_value)
                .context(StrContext::Expected(StrContextValue::Description("timestamp")))
                .parse_next(input)?)
        }
        (QueryTarget::Timestamp, Op::Gte) => {
            QueryOp::AtOrAfter(cut_err(parse_timestamp_value)
                .context(StrContext::Expected(StrContextValue::Description("timestamp")))
                .parse_next(input)?)
        }
        (QueryTarget::Timestamp, Op::Lte) => {
            QueryOp::AtOrBefore(cut_err(parse_timestamp_value)
                .context(StrContext::Expected(StrContextValue::Description("timestamp")))
                .parse_next(input)?)
        }
        (QueryTarget::Timestamp, Op::Eq) => {
            let ts = cut_err(parse_timestamp_value)
                .context(StrContext::Expected(StrContextValue::Description("timestamp")))
                .parse_next(input)?;
            QueryOp::Eq(ts.to_string())
        }
        (QueryTarget::Timestamp, Op::NotEq) => {
            let ts = cut_err(parse_timestamp_value)
                .context(StrContext::Expected(StrContextValue::Description("timestamp")))
                .parse_next(input)?;
            QueryOp::NotEq(ts.to_string())
        }
        (_, Op::Eq) => QueryOp::Eq(
            cut_err(parse_quoted_string)
                .context(StrContext::Expected(StrContextValue::Description("quoted string")))
                .parse_next(input)?,
        ),
        (_, Op::NotEq) => QueryOp::NotEq(
            cut_err(parse_quoted_string)
                .context(StrContext::Expected(StrContextValue::Description("quoted string")))
                .parse_next(input)?,
        ),
        (_, Op::RegexMatch) => {
            let pattern = cut_err(parse_regex_literal)
                .context(StrContext::Expected(StrContextValue::Description("regex /pattern/")))
                .parse_next(input)?;
            let re = regex::Regex::new(&pattern).map_err(|_| cut("valid regex"))?;
            QueryOp::Regex(re)
        }
        (_, Op::NotRegexMatch) => {
            let pattern = cut_err(parse_regex_literal)
                .context(StrContext::Expected(StrContextValue::Description("regex /pattern/")))
                .parse_next(input)?;
            let re = regex::Regex::new(&pattern).map_err(|_| cut("valid regex"))?;
            QueryOp::NotRegex(re)
        }
        (_, Op::Contains) => {
            let s = cut_err(parse_quoted_string)
                .context(StrContext::Expected(StrContextValue::Description("quoted string")))
                .parse_next(input)?;
            QueryOp::Contains(CompiledSubstring::new(s))
        }
        // Ordering ops on non-timestamp fields are invalid.
        (_, Op::Gt | Op::Lt | Op::Gte | Op::Lte) => {
            return Err(cut(">, <, >=, <= only valid on timestamp field"));
        }
    };

    Ok(QueryExpr::Compare {
        target,
        op: query_op,
    })
}

fn parse_field_ref(input: &mut &str) -> ModalResult<QueryTarget> {
    // Try prefixed fields first. Both "label." and "field." search all
    // key-value attributes (source labels + structured fields).
    if input.starts_with("label.") {
        "label.".parse_next(input)?;
        let name = parse_ident(input)?;
        return Ok(QueryTarget::Label(name.to_string()));
    }
    if input.starts_with("field.") {
        "field.".parse_next(input)?;
        let name = parse_ident(input)?;
        return Ok(QueryTarget::Label(name.to_string()));
    }

    // Try keywords (probe with saved slice to avoid consuming on failure).
    for (word, target) in [
        ("message", QueryTarget::Message),
        ("level", QueryTarget::Level),
        ("source", QueryTarget::Source),
        ("timestamp", QueryTarget::Timestamp),
    ] {
        let mut probe = *input;
        if kw(word, &mut probe).is_ok() {
            *input = probe;
            return Ok(target);
        }
    }

    Err(backtrack())
}

fn parse_ident<'i>(input: &mut &'i str) -> ModalResult<&'i str> {
    (
        take_while(1..=1, |c: char| c.is_ascii_alphabetic() || c == '_'),
        take_while(0.., |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
    )
        .take()
        .parse_next(input)
}

fn parse_quoted_string(input: &mut &str) -> ModalResult<String> {
    '"'.parse_next(input)?;
    let mut result = String::new();
    loop {
        let c: char = any.parse_next(input)?;
        match c {
            '"' => return Ok(result),
            '\\' => {
                let escaped: char = any.parse_next(input)?;
                match escaped {
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    'n' => result.push('\n'),
                    't' => result.push('\t'),
                    other => {
                        result.push('\\');
                        result.push(other);
                    }
                }
            }
            other => result.push(other),
        }
    }
}

fn parse_regex_literal(input: &mut &str) -> ModalResult<String> {
    '/'.parse_next(input)?;
    let mut result = String::new();
    loop {
        let c: char = any.parse_next(input)?;
        match c {
            '/' => return Ok(result),
            '\\' => {
                let escaped: char = any.parse_next(input)?;
                if escaped == '/' {
                    result.push('/');
                } else {
                    result.push('\\');
                    result.push(escaped);
                }
            }
            other => result.push(other),
        }
    }
}

fn parse_timestamp_value(input: &mut &str) -> ModalResult<jiff::Timestamp> {
    let raw: &str = parse_timestamp_raw(input)?;
    if let Ok(ts) = raw.parse::<jiff::Timestamp>() {
        return Ok(ts);
    }
    if let Ok(date) = raw.parse::<jiff::civil::Date>() {
        return date
            .to_zoned(jiff::tz::TimeZone::UTC)
            .map(|z| z.timestamp())
            .map_err(|_| backtrack());
    }
    Err(backtrack())
}

/// Consume a raw timestamp string: starts with 4+ digits, then digits/separators.
fn parse_timestamp_raw<'i>(input: &mut &'i str) -> ModalResult<&'i str> {
    (
        take_while(4.., |c: char| c.is_ascii_digit()),
        take_while(0.., |c: char| {
            c.is_ascii_digit()
                || c == '-'
                || c == ':'
                || c == '.'
                || c == 'T'
                || c == 'Z'
                || c == '+'
        }),
    )
        .take()
        .parse_next(input)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_eq() {
        let expr = parse_query(r#"level == "error""#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Level,
                op: QueryOp::Eq(ref s),
            } if s == "error"
        ));
    }

    #[test]
    fn parse_not_eq() {
        let expr = parse_query(r#"level != "debug""#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Level,
                op: QueryOp::NotEq(ref s),
            } if s == "debug"
        ));
    }

    #[test]
    fn parse_contains() {
        let expr = parse_query(r#"message contains "timeout""#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Message,
                op: QueryOp::Contains(_),
            }
        ));
    }

    #[test]
    fn parse_regex() {
        let expr = parse_query(r#"message =~ /timeout.*retry/"#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Message,
                op: QueryOp::Regex(_),
            }
        ));
    }

    #[test]
    fn parse_not_regex() {
        let expr = parse_query(r#"message !~ /debug/"#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Message,
                op: QueryOp::NotRegex(_),
            }
        ));
    }

    #[test]
    fn parse_label() {
        let expr = parse_query(r#"label.namespace == "kube-system""#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Label(ref k),
                op: QueryOp::Eq(ref v),
            } if k == "namespace" && v == "kube-system"
        ));
    }

    #[test]
    fn parse_field() {
        let expr = parse_query(r#"field.request_id == "abc-123""#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Label(ref k),
                op: QueryOp::Eq(ref v),
            } if k == "request_id" && v == "abc-123"
        ));
    }

    #[test]
    fn parse_and() {
        let expr = parse_query(r#"level == "error" and message contains "timeout""#).unwrap();
        assert!(matches!(expr, QueryExpr::And(_, _)));
    }

    #[test]
    fn parse_or() {
        let expr = parse_query(r#"level == "error" or level == "warn""#).unwrap();
        assert!(matches!(expr, QueryExpr::Or(_, _)));
    }

    #[test]
    fn parse_not() {
        let expr = parse_query(r#"not level == "debug""#).unwrap();
        assert!(matches!(expr, QueryExpr::Not(_)));
    }

    #[test]
    fn parse_case_insensitive() {
        let expr = parse_query(r#"(?i) message contains "error""#).unwrap();
        assert!(matches!(expr, QueryExpr::CaseInsensitive(_)));
    }

    #[test]
    fn parse_parens_and_precedence() {
        let expr =
            parse_query(r#"(level == "error" or level == "warn") and message contains "timeout""#)
                .unwrap();
        assert!(matches!(expr, QueryExpr::And(_, _)));
        if let QueryExpr::And(lhs, _) = &expr {
            assert!(matches!(**lhs, QueryExpr::Or(_, _)));
        }
    }

    #[test]
    fn parse_timestamp_greater() {
        let expr = parse_query("timestamp > 2024-01-15T10:00:00Z").unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Timestamp,
                op: QueryOp::After(_),
            }
        ));
    }

    #[test]
    fn parse_timestamp_date_only() {
        let expr = parse_query("timestamp >= 2024-01-15").unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Timestamp,
                op: QueryOp::AtOrAfter(_),
            }
        ));
    }

    #[test]
    fn parse_timestamp_range() {
        let expr =
            parse_query("timestamp >= 2024-01-15 and timestamp < 2024-01-16").unwrap();
        assert!(matches!(expr, QueryExpr::And(_, _)));
    }

    #[test]
    fn parse_complex_nested() {
        // (?i) applies to the parenthesized subexpr (it's a unary prefix),
        // so the top level is And(CaseInsensitive(Or(...)), Not(...)).
        let expr = parse_query(
            r#"(?i) (level == "error" or level == "warn") and not message contains "health""#,
        )
        .unwrap();
        assert!(matches!(expr, QueryExpr::And(_, _)));
        if let QueryExpr::And(lhs, rhs) = &expr {
            assert!(matches!(**lhs, QueryExpr::CaseInsensitive(_)));
            assert!(matches!(**rhs, QueryExpr::Not(_)));
        }
    }

    #[test]
    fn parse_case_insensitive_whole_expr() {
        // Wrapping the entire expression requires parens.
        let expr = parse_query(
            r#"(?i) (level == "error" and message contains "health")"#,
        )
        .unwrap();
        assert!(matches!(expr, QueryExpr::CaseInsensitive(_)));
    }

    #[test]
    fn parse_error_empty() {
        let err = parse_query("").unwrap_err();
        assert_eq!(err.offset, 0);
    }

    #[test]
    fn parse_error_bad_operator() {
        let err = parse_query(r#"level ** "error""#).unwrap_err();
        assert!(err.offset > 0);
    }

    #[test]
    fn parse_error_unclosed_string() {
        let err = parse_query(r#"level == "error"#).unwrap_err();
        assert!(err.offset > 0);
    }

    #[test]
    fn parse_escaped_string() {
        let expr = parse_query(r#"message contains "hello \"world\"""#).unwrap();
        if let QueryExpr::Compare { op: QueryOp::Contains(cs), .. } = &expr {
            assert_eq!(cs.text, "hello \"world\"");
        } else {
            panic!("expected Contains");
        }
    }

    #[test]
    fn parse_source() {
        let expr = parse_query(r#"source == "prod-loki""#).unwrap();
        assert!(matches!(
            expr,
            QueryExpr::Compare {
                target: QueryTarget::Source,
                op: QueryOp::Eq(ref s),
            } if s == "prod-loki"
        ));
    }

    #[test]
    fn keyword_not_prefix_of_identifier() {
        let err = parse_query(r#"notfoo == "bar""#);
        assert!(err.is_err());
    }

    #[test]
    fn error_message_missing_operator() {
        let err = parse_query(r#"level ** "error""#).unwrap_err();
        assert!(
            err.message.contains("operator"),
            "expected 'operator' in message: {:?}",
            err.message
        );
    }

    #[test]
    fn error_message_missing_value() {
        let err = parse_query(r#"level =="#).unwrap_err();
        assert!(
            err.message.contains("quoted string"),
            "expected 'quoted string' in message: {:?}",
            err.message
        );
    }

    #[test]
    fn error_message_unclosed_paren() {
        let err = parse_query(r#"(level == "error""#).unwrap_err();
        // cut_err after '(' means this is a Cut error with context.
        assert!(
            err.message.contains(')') || err.message.contains("paren"),
            "expected ')' or 'paren' in message: {:?}",
            err.message
        );
    }

    #[test]
    fn error_message_bad_field() {
        let err = parse_query(r#"foobar == "x""#).unwrap_err();
        assert!(
            err.message.contains("field"),
            "expected 'field' in message: {:?}",
            err.message
        );
    }

    #[test]
    fn error_offset_precision() {
        // "level == " gets trimmed to "level ==", ws before value is consumed by the parser.
        let err = parse_query(r#"level == "#).unwrap_err();
        // The offset points to where parsing failed in the trimmed input.
        assert!(err.offset >= 8, "error offset should be near end of input, got {}", err.offset);
    }
}
