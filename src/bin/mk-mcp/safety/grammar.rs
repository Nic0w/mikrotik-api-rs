//! Formal RouterOS command-line lexer and EBNF parser.
//!
//! Derived from the RouterOS Scripting grammar specification:
//! <https://github.com/wiresage/routeros-lsp/blob/master/docs/grammar.md>
//!
//! ### Supported EBNF Grammar Subset (Single API Command Line)
//! ```ebnf
//! command-line  = [ prefix ] [ path ] command { argument } ;
//! prefix        = ":" | "/" ;
//! path          = word { ( "/" | "," | space ) word } ;
//! command       = word ;
//! argument      = query | where-clause | named | unset | flag | unnamed ;
//! query         = "?" word [ "=" [ space ] value ] ;
//! where-clause  = "where" { [ "?" ] word [ "=" [ space ] value ] | [ "!" ] word } ;
//! named         = [ "=" ] word "=" [ space ] value ;
//! unset         = "!" word ;
//! flag          = word ;
//! unnamed       = value ;
//!
//! value         = string | contiguous-word ;
//! string        = '"' { char | escape } '"' ;
//! escape        = "\" ( '"' | "\" | "n" | "r" | "t" | "$" | "_" | "a" | "b" | "f" | "v" | "?" | hex hex ) ;
//! ```

use std::collections::HashMap;

/// Output of parsing a RouterOS command line according to the formal grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Canonical slash-delimited command path (e.g. `"/ip/firewall/filter/add"`).
    pub path: String,
    /// Extracted key-value command attributes (e.g. `"comment" -> "Allow incoming..."`).
    pub attributes: HashMap<String, String>,
    /// Extracted query filter words without leading `?` (e.g. `"running=true"`).
    pub query: Vec<String>,
    /// Extracted `.proplist` field names.
    pub proplist: Vec<String>,
    /// Optional grammar or structural error (e.g. multi-statement `;` or `[...]` command substitution).
    pub parse_error: Option<String>,
}

/// Lexical token kinds per Section 2 of `grammar.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// `/` path or CIDR delimiter.
    Slash,
    /// `,` path or list delimiter.
    Comma,
    /// `:` global command prefix or IPv6/MAC/time delimiter.
    Colon,
    /// `=` parameter assignment delimiter.
    Equals,
    /// `;` statement terminator.
    Semicolon,
    /// `[` command substitution open delimiter.
    LeftBracket,
    /// `]` command substitution close delimiter.
    RightBracket,
    /// Unquoted word or literal.
    Word(String),
    /// Double-quoted (or single-quoted) string literal with escapes decoded.
    QuotedString(String),
}

impl TokenKind {
    /// Returns the raw text representation when reconstructing contiguous unquoted values
    /// (such as `192.168.88.1/24`, `2001:db8::1/64`, `00:11:22:33:44:55`, or Base64 `abc=def=`).
    fn as_value_piece(&self) -> Option<&str> {
        match self {
            TokenKind::Slash => Some("/"),
            TokenKind::Comma => Some(","),
            TokenKind::Colon => Some(":"),
            TokenKind::Equals => Some("="),
            TokenKind::Word(w) | TokenKind::QuotedString(w) => Some(w.as_str()),
            TokenKind::Semicolon | TokenKind::LeftBracket | TokenKind::RightBracket => None,
        }
    }
}

/// A lexical token annotated with whether whitespace preceded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Token variant.
    pub kind: TokenKind,
    /// True if at least one whitespace character preceded this token.
    /// Enforces Section 2.3: no whitespace before `=`, optional whitespace after `=`.
    pub after_space: bool,
}

/// Tokenizes a raw RouterOS command string according to Section 2 of `grammar.md`.
pub fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut chars = input.chars().peekable();
    let mut after_space = false;

    while let Some(&ch) = chars.peek() {
        // 2.3 Whitespace
        if ch.is_whitespace() {
            after_space = true;
            chars.next();
            continue;
        }

        // 2.2 Line joining outside strings: backslash followed by newline
        if ch == '\\' {
            chars.next();
            if chars.peek() == Some(&'\r') {
                chars.next();
            }
            if chars.peek() == Some(&'\n') {
                chars.next();
                after_space = true;
                continue;
            }
            // Otherwise treat as part of a word
            let mut word = String::from("\\");
            read_unquoted_word(&mut chars, &mut word);
            tokens.push(Token {
                kind: TokenKind::Word(word),
                after_space,
            });
            after_space = false;
            continue;
        }

        // 2.1 Comments: `#` at start of command or after `;` (Rule [V] L-01, L-02)
        if ch == '#'
            && (tokens.is_empty()
                || matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Semicolon)))
        {
            for c in chars.by_ref() {
                if c == '\n' || c == '\r' {
                    break;
                }
            }
            after_space = true;
            continue;
        }

        // 2.4 & 2.5 Delimiters and statement terminators
        match ch {
            '/' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Slash,
                    after_space,
                });
                after_space = false;
                continue;
            }
            ',' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    after_space,
                });
                after_space = false;
                continue;
            }
            ':' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Colon,
                    after_space,
                });
                after_space = false;
                continue;
            }
            '=' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Equals,
                    after_space,
                });
                after_space = false;
                continue;
            }
            ';' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::Semicolon,
                    after_space,
                });
                after_space = false;
                continue;
            }
            '[' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::LeftBracket,
                    after_space,
                });
                after_space = false;
                continue;
            }
            ']' => {
                chars.next();
                tokens.push(Token {
                    kind: TokenKind::RightBracket,
                    after_space,
                });
                after_space = false;
                continue;
            }
            // 2.6 Double-quoted string literal
            '"' => {
                let s = read_double_quoted_string(&mut chars)?;
                tokens.push(Token {
                    kind: TokenKind::QuotedString(s),
                    after_space,
                });
                after_space = false;
                continue;
            }
            // Resilient support for single-quoted strings occasionally emitted by LLMs
            '\'' => {
                let s = read_single_quoted_string(&mut chars)?;
                tokens.push(Token {
                    kind: TokenKind::QuotedString(s),
                    after_space,
                });
                after_space = false;
                continue;
            }
            _ => {}
        }

        // Unquoted word (Note: `#` inside a word like `a#b` or `?#|` is part of the word per [V] L-02)
        let mut word = String::new();
        read_unquoted_word(&mut chars, &mut word);
        if !word.is_empty() {
            tokens.push(Token {
                kind: TokenKind::Word(word),
                after_space,
            });
            after_space = false;
        }
    }

    Ok(tokens)
}

fn is_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '/' | ',' | ':' | '=' | ';' | '[' | ']' | '"' | '\'')
}

fn read_unquoted_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
    while let Some(&ch) = chars.peek() {
        if is_delimiter(ch) {
            break;
        }
        out.push(ch);
        chars.next();
    }
}

/// Reads a RouterOS double-quoted string, decoding escape sequences per Section 2.6 of `grammar.md`.
fn read_double_quoted_string(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<String, String> {
    // Consume opening '"'
    chars.next();
    let mut out = String::new();

    while let Some(ch) = chars.next() {
        if ch == '"' {
            return Ok(out);
        }
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('$') => out.push('$'),
                // `\_` is a documented space escape in RouterOS (Section 2.6)
                Some('_') => out.push(' '),
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some('f') => out.push('\x0C'),
                Some('v') => out.push('\x0B'),
                Some('?') => out.push('?'),
                // Line continuation inside string (Rule [V] L-04):
                // removes backslash, newline, and leading whitespace on the next line.
                Some('\r') => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    while let Some(&next_ws) = chars.peek() {
                        if next_ws == ' ' || next_ws == '\t' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                Some('\n') => {
                    while let Some(&next_ws) = chars.peek() {
                        if next_ws == ' ' || next_ws == '\t' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                // Hex escape `\XX`
                Some(h1) if h1.is_ascii_hexdigit() => {
                    if let Some(&h2) = chars.peek() {
                        if h2.is_ascii_hexdigit() {
                            chars.next();
                            let hex_str = [h1, h2].iter().collect::<String>();
                            if let Ok(byte) = u8::from_str_radix(&hex_str, 16) {
                                out.push(byte as char);
                                continue;
                            }
                        }
                    }
                    out.push('\\');
                    out.push(h1);
                }
                Some(other) => {
                    out.push(other);
                }
                None => {
                    return Err("Unterminated escape sequence in string literal".to_string());
                }
            }
        } else {
            out.push(ch);
        }
    }

    Err("Unterminated double-quoted string literal".to_string())
}

fn read_single_quoted_string(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<String, String> {
    chars.next();
    let mut out = String::new();

    while let Some(ch) = chars.next() {
        if ch == '\'' {
            return Ok(out);
        }
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                out.push(escaped);
            } else {
                return Err("Unterminated escape sequence in single-quoted string".to_string());
            }
        } else {
            out.push(ch);
        }
    }

    Err("Unterminated single-quoted string literal".to_string())
}

/// Known RouterOS command verbs (Section 4.3 + API verbs).
fn is_known_verb(word: &str) -> bool {
    matches!(
        word,
        "print"
            | "get"
            | "find"
            | "export"
            | "add"
            | "set"
            | "remove"
            | "enable"
            | "disable"
            | "comment"
            | "unset"
            | "move"
            | "edit"
            | "reset"
            | "reset-counters"
            | "reset-counters-all"
            | "listen"
            | "monitor"
            | "monitor-traffic"
            | "inspect"
            | "ping"
            | "traceroute"
            | "dns-lookup"
            | "reboot"
            | "shutdown"
            | "reset-configuration"
            | "bandwidth-test"
            | "format"
            | "uninstall"
            | "upgrade"
    )
}

/// Verbs that accept an unnamed positional `.id` / `numbers` argument in CLI syntax.
fn is_item_target_verb(verb: &str) -> bool {
    matches!(
        verb,
        "set"
            | "remove"
            | "enable"
            | "disable"
            | "comment"
            | "reset"
            | "reset-counters"
            | "get"
            | "edit"
    )
}

/// Checks whether `tokens[idx..]` begins a `named` (`[=]word=value`) or `query` (`?word=value`) assignment.
fn is_assignment_at(tokens: &[Token], idx: usize) -> Option<(String, usize)> {
    if idx >= tokens.len() {
        return None;
    }

    // Case 1: `word=...` (Rule [D]: no space between word and `=`)
    if let TokenKind::Word(key) = &tokens[idx].kind {
        if idx + 1 < tokens.len()
            && tokens[idx + 1].kind == TokenKind::Equals
            && !tokens[idx + 1].after_space
        {
            return Some((key.clone(), idx + 2));
        }
        // Also handle `.proplist` or `proplist` when split by `:`? No, `.` is inside `Word`.
    }

    // Case 2: API-style pre-prefixed `=word=...`
    if tokens[idx].kind == TokenKind::Equals
        && idx + 2 < tokens.len()
        && !tokens[idx + 1].after_space
        && tokens[idx + 2].kind == TokenKind::Equals
        && !tokens[idx + 2].after_space
    {
        if let TokenKind::Word(key) = &tokens[idx + 1].kind {
            return Some((key.clone(), idx + 3));
        }
    }

    None
}

/// Consumes the `value` part of `named = word "=" [ space ] value`.
fn consume_value(tokens: &[Token], idx: &mut usize) -> String {
    if *idx >= tokens.len() {
        return String::new();
    }

    // Never consume statement terminators or brackets as part of a simple value
    if matches!(
        tokens[*idx].kind,
        TokenKind::Semicolon | TokenKind::LeftBracket | TokenKind::RightBracket
    ) {
        return String::new();
    }

    // If there is whitespace after `=`, check if the next token is actually the NEXT argument
    // rather than the value of this `=` (e.g. `=detail= =count-only=` or `detail= log=yes`).
    if tokens[*idx].after_space {
        if is_assignment_at(tokens, *idx).is_some() {
            return String::new();
        }
        if let TokenKind::Word(w) = &tokens[*idx].kind {
            if w.starts_with('?')
                || w.starts_with('!')
                || matches!(
                    w.to_ascii_lowercase().as_str(),
                    "detail" | "brief" | "terse" | "without-paging" | "count-only" | "where"
                )
            {
                return String::new();
            }
        }
        if tokens[*idx].kind == TokenKind::Equals {
            return String::new();
        }
    }

    // If the value is a quoted string, consume it directly
    if let TokenKind::QuotedString(s) = &tokens[*idx].kind {
        *idx += 1;
        return s.clone();
    }

    // Otherwise, consume the contiguous unquoted value (`after_space == false` on subsequent pieces)
    let mut val = String::new();
    if let Some(piece) = tokens[*idx].kind.as_value_piece() {
        val.push_str(piece);
        *idx += 1;
    } else {
        return String::new();
    }

    while *idx < tokens.len() && !tokens[*idx].after_space {
        if let Some(piece) = tokens[*idx].kind.as_value_piece() {
            val.push_str(piece);
            *idx += 1;
        } else {
            break;
        }
    }

    val
}

/// Consumes a single contiguous unquoted word/value in the arguments section.
fn consume_contiguous_arg(tokens: &[Token], idx: &mut usize) -> String {
    if let TokenKind::QuotedString(s) = &tokens[*idx].kind {
        *idx += 1;
        return s.clone();
    }

    let mut val = String::new();
    if let Some(piece) = tokens[*idx].kind.as_value_piece() {
        val.push_str(piece);
        *idx += 1;
    } else {
        *idx += 1;
        return val;
    }

    while *idx < tokens.len() && !tokens[*idx].after_space {
        if let Some(piece) = tokens[*idx].kind.as_value_piece() {
            val.push_str(piece);
            *idx += 1;
        } else {
            break;
        }
    }

    val
}

/// Parses a RouterOS command line into a canonical path, attributes, queries, and proplist.
pub fn parse_command_line(raw: &str) -> ParsedCommand {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ParsedCommand {
            path: "/".to_string(),
            attributes: HashMap::new(),
            query: Vec::new(),
            proplist: Vec::new(),
            parse_error: None,
        };
    }

    let tokens = match tokenize(trimmed) {
        Ok(t) => t,
        Err(e) => {
            return ParsedCommand {
                path: "/".to_string(),
                attributes: HashMap::new(),
                query: Vec::new(),
                proplist: Vec::new(),
                parse_error: Some(e),
            };
        }
    };

    if tokens.is_empty() {
        return ParsedCommand {
            path: "/".to_string(),
            attributes: HashMap::new(),
            query: Vec::new(),
            proplist: Vec::new(),
            parse_error: None,
        };
    }

    // Check for disallowed grammar constructs in single-sentence API mode:
    // 1. Command substitution `[ ... ]`
    if tokens
        .iter()
        .any(|t| matches!(t.kind, TokenKind::LeftBracket | TokenKind::RightBracket))
    {
        return ParsedCommand {
            path: "/".to_string(),
            attributes: HashMap::new(),
            query: Vec::new(),
            proplist: Vec::new(),
            parse_error: Some(
                "CLI command substitution '[...]' is not supported by the RouterOS binary API; query items with 'print' first to obtain their '.id'".to_string(),
            ),
        };
    }

    // 2. Multi-statement `;` with tokens following it
    if let Some(semi_pos) = tokens.iter().position(|t| t.kind == TokenKind::Semicolon) {
        if tokens[semi_pos + 1..]
            .iter()
            .any(|t| t.kind != TokenKind::Semicolon)
        {
            return ParsedCommand {
                path: "/".to_string(),
                attributes: HashMap::new(),
                query: Vec::new(),
                proplist: Vec::new(),
                parse_error: Some(
                    "Multiple statements separated by ';' are not permitted in a single API call"
                        .to_string(),
                ),
            };
        }
    }

    let mut path_segments: Vec<String> = Vec::new();
    let mut attributes: HashMap<String, String> = HashMap::new();
    let mut query: Vec<String> = Vec::new();
    let mut proplist: Vec<String> = Vec::new();

    // =========================================================================
    // Phase 1: Parse `[ prefix ] [ path ] command`
    // =========================================================================
    let mut i = 0;
    while i < tokens.len() {
        // Stop if we hit `;`
        if tokens[i].kind == TokenKind::Semicolon {
            break;
        }

        // Stop if this token starts a `named` or `query` assignment (`key=value` or `=key=value`)
        if is_assignment_at(&tokens, i).is_some() {
            break;
        }

        // Stop if this token is a quoted string (Rule [V] L-22: paths cannot contain quoted strings)
        if matches!(tokens[i].kind, TokenKind::QuotedString(_)) {
            break;
        }

        // Stop if this token is a query (`?`) or unset (`!`)
        if let TokenKind::Word(w) = &tokens[i].kind {
            if w.starts_with('?') || w.starts_with('!') {
                break;
            }
        }

        // If preceded by whitespace and we already have path segments, check if the verb is complete
        if tokens[i].after_space && !path_segments.is_empty() {
            // If the next token is `/` or `,`, the path is still continuing (e.g. `/ip /address`)
            if !matches!(tokens[i].kind, TokenKind::Slash | TokenKind::Comma) {
                let last_seg = path_segments.last().map(String::as_str).unwrap_or("");
                if is_known_verb(last_seg) {
                    break;
                }
                if let TokenKind::Word(w) = &tokens[i].kind {
                    if w.starts_with('*')
                        || w.chars().all(|c| c.is_ascii_digit())
                        || matches!(
                            w.to_ascii_lowercase().as_str(),
                            "detail"
                                | "brief"
                                | "terse"
                                | "without-paging"
                                | "count-only"
                                | "where"
                        )
                    {
                        break;
                    }
                }
            }
        }

        match &tokens[i].kind {
            TokenKind::Slash | TokenKind::Comma | TokenKind::Colon => {
                i += 1;
            }
            TokenKind::Word(w) => {
                let trimmed_w = w.trim();
                if !trimmed_w.is_empty() {
                    path_segments.push(trimmed_w.to_string());
                }
                i += 1;
            }
            _ => break,
        }
    }

    let normalized_path = if path_segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path_segments.join("/"))
    };

    let verb = path_segments.last().map(String::as_str).unwrap_or("");

    // =========================================================================
    // Phase 2: Parse `{ argument }`
    // =========================================================================
    let mut in_where = false;

    while i < tokens.len() {
        if tokens[i].kind == TokenKind::Semicolon {
            i += 1;
            continue;
        }

        // 1. Named assignment or `?key=value` query assignment
        if let Some((raw_key, val_start_idx)) = is_assignment_at(&tokens, i) {
            i = val_start_idx;
            let val = consume_value(&tokens, &mut i);

            if raw_key == ".proplist" || raw_key == "proplist" {
                for p in val.split(',') {
                    let p_trim = p.trim();
                    if !p_trim.is_empty() {
                        proplist.push(p_trim.to_string());
                    }
                }
            } else if let Some(q_key) = raw_key.strip_prefix('?') {
                if val.is_empty() {
                    query.push(q_key.to_string());
                } else {
                    query.push(format!("{q_key}={val}"));
                }
            } else if in_where {
                query.push(format!("{raw_key}={val}"));
            } else {
                attributes.insert(raw_key, val);
            }
            continue;
        }

        // 2. Stray `=` (e.g. trailing `=` after a flag like `=detail=`)
        if tokens[i].kind == TokenKind::Equals {
            i += 1;
            continue;
        }

        // 3. Bare words, flags, `where`, `?query`, `!unset`, or positional arguments
        let arg = consume_contiguous_arg(&tokens, &mut i);
        if arg.is_empty() {
            continue;
        }

        // `where` keyword initiating filter expressions on `print` / `find`
        if (verb == "print" || verb == "find") && arg.eq_ignore_ascii_case("where") {
            in_where = true;
            continue;
        }

        // CLI display-only flags on `print` (API `print` returns all fields unpaged by default)
        if verb == "print"
            && matches!(
                arg.to_ascii_lowercase().as_str(),
                "detail" | "brief" | "terse" | "without-paging"
            )
        {
            continue;
        }

        // Bare `?query` word without `=` (e.g. `?#|` or `?running`)
        if let Some(q) = arg.strip_prefix('?') {
            if !q.is_empty() {
                query.push(q.to_string());
            }
            continue;
        }

        // `unset = "!" word` (Section 4 Rule [D])
        if let Some(unset_param) = arg.strip_prefix('!') {
            if !unset_param.is_empty() {
                if in_where {
                    query.push(format!("{unset_param}=false"));
                } else if verb == "unset" {
                    attributes.insert("value-name".to_string(), unset_param.to_string());
                } else {
                    attributes.insert(format!("!{unset_param}"), String::new());
                }
            }
            continue;
        }

        // Bare word inside `where` clause (e.g. `print where running` -> `?running=true`)
        if in_where {
            query.push(format!("{arg}=true"));
            continue;
        }

        // Unnamed positional `.id` / `numbers` on item-targeting commands (`set *1`, `remove 0`)
        if is_item_target_verb(verb)
            && (arg.starts_with('*') || arg.chars().all(|c| c.is_ascii_digit() || c == ','))
        {
            attributes.entry(".id".to_string()).or_insert(arg);
            continue;
        }

        // Unnamed positional target address on diagnostic commands (`/ping 1.1.1.1`)
        if matches!(verb, "ping" | "traceroute" | "dns-lookup")
            && !attributes.contains_key("address")
        {
            attributes.insert("address".to_string(), arg);
            continue;
        }

        // General bare flag (e.g. `count-only`, `once`)
        attributes.insert(arg, String::new());
    }

    ParsedCommand {
        path: normalized_path,
        attributes,
        query,
        proplist,
        parse_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_firewall_add_with_quoted_comment_and_trailing_args() {
        let cmd = parse_command_line(
            r#"/ip/firewall/filter/add action=accept chain=input dst-port=1337 protocol=tcp connection-state=new comment="Allow incoming IPv4 traffic on port 1337" log=yes priority=1"#,
        );
        assert_eq!(cmd.parse_error, None);
        assert_eq!(cmd.path, "/ip/firewall/filter/add");
        assert_eq!(
            cmd.attributes.get("action").map(String::as_str),
            Some("accept")
        );
        assert_eq!(
            cmd.attributes.get("chain").map(String::as_str),
            Some("input")
        );
        assert_eq!(
            cmd.attributes.get("dst-port").map(String::as_str),
            Some("1337")
        );
        assert_eq!(
            cmd.attributes.get("protocol").map(String::as_str),
            Some("tcp")
        );
        assert_eq!(
            cmd.attributes.get("connection-state").map(String::as_str),
            Some("new")
        );
        assert_eq!(
            cmd.attributes.get("comment").map(String::as_str),
            Some("Allow incoming IPv4 traffic on port 1337")
        );
        assert_eq!(cmd.attributes.get("log").map(String::as_str), Some("yes"));
        assert_eq!(
            cmd.attributes.get("priority").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn test_string_escape_sequences_per_grammar_spec() {
        // Section 2.6: \" \\ \n \r \t \$ \_ (space) and \XX hex
        let cmd = parse_command_line(
            r#"/ip/firewall/filter/set *1 comment="Quote: \"Hello\_World\"\nCost: \$10\21""#,
        );
        assert_eq!(cmd.parse_error, None);
        assert_eq!(cmd.path, "/ip/firewall/filter/set");
        assert_eq!(cmd.attributes.get(".id").map(String::as_str), Some("*1"));
        assert_eq!(
            cmd.attributes.get("comment").map(String::as_str),
            Some("Quote: \"Hello World\"\nCost: $10!")
        );
    }

    #[test]
    fn test_whitespace_after_equals_and_complex_unquoted_values() {
        // Rule [X] L-09: whitespace after `=` is accepted
        // Unquoted values with `/`, `:`, `,`, `=`
        let cmd = parse_command_line(
            r#"/ip/address/add address=192.168.88.1/24 interface=ether1 comment= "LAN Gateway""#,
        );
        assert_eq!(cmd.parse_error, None);
        assert_eq!(cmd.path, "/ip/address/add");
        assert_eq!(
            cmd.attributes.get("address").map(String::as_str),
            Some("192.168.88.1/24")
        );
        assert_eq!(
            cmd.attributes.get("interface").map(String::as_str),
            Some("ether1")
        );
        assert_eq!(
            cmd.attributes.get("comment").map(String::as_str),
            Some("LAN Gateway")
        );
    }

    #[test]
    fn test_where_clause_and_quoted_queries() {
        let cmd = parse_command_line(
            r#"/ip/firewall/filter print detail where chain=forward comment="Allow LAN" !disabled"#,
        );
        assert_eq!(cmd.parse_error, None);
        assert_eq!(cmd.path, "/ip/firewall/filter/print");
        assert_eq!(
            cmd.query,
            vec![
                "chain=forward".to_string(),
                "comment=Allow LAN".to_string(),
                "disabled=false".to_string()
            ]
        );
    }

    #[test]
    fn test_rejects_multi_statement_and_command_substitution() {
        let multi = parse_command_line("/interface/print ; /system/reboot");
        assert!(multi.parse_error.is_some());

        let subst = parse_command_line("/ip/firewall/filter/remove [find where dst-port=1337]");
        assert!(subst.parse_error.is_some());
    }

    #[test]
    fn test_unterminated_quote_returns_parse_error() {
        let bad = parse_command_line(r#"/ip/firewall/filter/add comment="unterminated"#);
        assert!(bad.parse_error.is_some());
    }
}
