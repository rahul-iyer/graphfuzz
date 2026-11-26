//! Reads ANTLR 4 grammars (`.g4`). Parser and lexer rules become flat alternatives: every parenthesized group
//! is lifted into a rule of its own, so each choice the grammar offers is a choice between alternatives.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suffix {
    One,
    Optional,
    Star,
    Plus,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Atom {
    /// A parser rule, a lexer rule, or a lifted group.
    Rule(String),
    Literal(String),
    /// Character ranges, or the characters not in them.
    Set {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Any,
    /// Every token or character except these literals, as `~('a' | 'b')`.
    NotLiterals(Vec<String>),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Element {
    pub atom: Atom,
    pub suffix: Suffix,
}

#[derive(Clone, Debug)]
pub struct Alt {
    pub elements: Vec<Element>,
    pub label: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub name: String,
    pub lexer: bool,
    pub fragment: bool,
    /// A lexer rule whose tokens the parser never sees (`-> skip`, `-> channel(HIDDEN)`).
    #[allow(dead_code)] // parsed for completeness; not used yet
    pub hidden: bool,
    /// The rule a lifted group came from.
    pub parent: Option<String>,
    pub alts: Vec<Alt>,
}

pub struct Grammar {
    pub name: String,
    #[allow(dead_code)] // parsed from `options { caseInsensitive = true; }`; sentences use the vocabulary's spelling
    pub case_insensitive: bool,
    pub rules: BTreeMap<String, Rule>,
    /// Parser rules in file order; the first is the usual start rule.
    pub order: Vec<String>,
}

// ---------------------------------------------------------------------------- tokens

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(String),
    Literal(String),
    Set(String),
    /// A brace block: an action, a predicate, or the body of options/tokens.
    Block(String),
    /// An element option, as <assoc=right>.
    Angle,
    Punct(&'static str),
}

fn unescape(text: &str) -> Result<String> {
    let mut result = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => result.push('\n'),
            Some('r') => result.push('\r'),
            Some('t') => result.push('\t'),
            Some('b') => result.push('\u{8}'),
            Some('f') => result.push('\u{c}'),
            Some('u') => {
                let mut hex = String::new();
                let rest: String = chars.clone().collect();
                if rest.starts_with('{') {
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        if c != '{' {
                            hex.push(c);
                        }
                    }
                } else {
                    for _ in 0..4 {
                        hex.push(chars.next().context("short \\u escape")?);
                    }
                }
                let code = u32::from_str_radix(&hex, 16).with_context(|| format!("bad \\u escape {hex}"))?;
                result.push(char::from_u32(code).unwrap_or('?'));
            }
            Some(other) => result.push(other),
            None => bail!("trailing backslash"),
        }
    }
    Ok(result)
}

fn tokenize(text: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '\'' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            tokens.push(Tok::Literal(unescape(&chars[start..i].iter().collect::<String>())?));
            i += 1;
        } else if c == '[' {
            let start = i + 1;
            i += 1;
            while i < chars.len() && chars[i] != ']' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            tokens.push(Tok::Set(chars[start..i].iter().collect()));
            i += 1;
        } else if c == '{' {
            let start = i + 1;
            let mut depth = 1;
            i += 1;
            while i < chars.len() && depth > 0 {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    '\'' | '"' => {
                        let quote = chars[i];
                        i += 1;
                        while i < chars.len() && chars[i] != quote {
                            if chars[i] == '\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            tokens.push(Tok::Block(chars[start..i - 1].iter().collect()));
        } else if c == '<' {
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            i += 1;
            tokens.push(Tok::Angle);
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(Tok::Name(chars[start..i].iter().collect()));
        } else {
            let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
            let punct = match two.as_str() {
                ".." => Some(".."),
                "->" => Some("->"),
                "+=" => Some("+="),
                _ => None,
            };
            if let Some(punct) = punct {
                tokens.push(Tok::Punct(punct));
                i += 2;
                continue;
            }
            let punct = match c {
                ':' => ":",
                ';' => ";",
                '|' => "|",
                '(' => "(",
                ')' => ")",
                '?' => "?",
                '*' => "*",
                '+' => "+",
                '~' => "~",
                '.' => ".",
                '=' => "=",
                '#' => "#",
                ',' => ",",
                '@' => "@",
                other => bail!("unexpected character '{other}' in grammar"),
            };
            tokens.push(Tok::Punct(punct));
            i += 1;
        }
    }
    Ok(tokens)
}

/// Character ranges of a `[...]` set.
fn parse_set(text: &str) -> Result<Vec<(char, char)>> {
    let chars: Vec<char> = unescape_set(text)?;
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            ranges.push((chars[i], chars[i + 2]));
            i += 3;
        } else {
            ranges.push((chars[i], chars[i]));
            i += 1;
        }
    }
    Ok(ranges)
}

/// A set's characters with escapes resolved; an escaped '-' stays a literal dash, written as a range of itself.
fn unescape_set(text: &str) -> Result<Vec<char>> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        let escaped = chars.next().context("trailing backslash in set")?;
        let value = match escaped {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'f' => '\u{c}',
            'b' => '\u{8}',
            'u' => {
                let mut hex = String::new();
                if chars.peek() == Some(&'{') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        hex.push(c);
                    }
                } else {
                    for _ in 0..4 {
                        hex.push(chars.next().context("short \\u escape in set")?);
                    }
                }
                char::from_u32(u32::from_str_radix(&hex, 16)?).unwrap_or('?')
            }
            'p' | 'P' => {
                // A Unicode property such as \p{L}: stand in with ASCII letters.
                if chars.peek() == Some(&'{') {
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                }
                result.extend(['a', '-', 'z']);
                continue;
            }
            other => other,
        };
        // A literal dash must not read as a range.
        if value == '-' {
            result.extend(['-', '-', '-']);
            continue;
        }
        result.push(value);
    }
    Ok(result)
}

// ---------------------------------------------------------------------------- parsing

struct Reader {
    tokens: Vec<Tok>,
    position: usize,
    lifted: Vec<Rule>,
}

impl Reader {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.position)
    }

    fn next(&mut self) -> Option<Tok> {
        let token = self.tokens.get(self.position).cloned();
        self.position += 1;
        token
    }

    fn eat(&mut self, punct: &str) -> bool {
        if self.peek()
            == Some(&Tok::Punct(match punct {
                ":" => ":",
                ";" => ";",
                "|" => "|",
                "(" => "(",
                ")" => ")",
                "#" => "#",
                "->" => "->",
                "," => ",",
                ".." => "..",
                "=" => "=",
                "+=" => "+=",
                _ => return false,
            }))
        {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, punct: &str) -> Result<()> {
        if self.eat(punct) { Ok(()) } else { bail!("expected '{punct}' at {:?}", self.peek()) }
    }

    /// Alternatives up to a closing `)` or `;`.
    fn alternatives(&mut self, rule: &str, lexer: bool) -> Result<Vec<Alt>> {
        let mut alts = vec![self.alternative(rule, lexer)?];
        while self.eat("|") {
            alts.push(self.alternative(rule, lexer)?);
        }
        Ok(alts)
    }

    fn alternative(&mut self, rule: &str, lexer: bool) -> Result<Alt> {
        let mut elements = Vec::new();
        let mut label = None;
        loop {
            match self.peek() {
                None | Some(Tok::Punct(";")) | Some(Tok::Punct("|")) | Some(Tok::Punct(")")) => break,
                Some(Tok::Punct("#")) => {
                    self.next();
                    if let Some(Tok::Name(name)) = self.next() {
                        label = Some(name);
                    }
                }
                Some(Tok::Punct("->")) => {
                    // Lexer commands, as -> channel(HIDDEN), end the alternative.
                    self.next();
                    let mut depth = 0;
                    loop {
                        match self.peek() {
                            None => break,
                            Some(Tok::Punct("(")) => depth += 1,
                            Some(Tok::Punct(")")) if depth == 0 => break,
                            Some(Tok::Punct(")")) => depth -= 1,
                            Some(Tok::Punct(";")) | Some(Tok::Punct("|")) if depth == 0 => break,
                            _ => {}
                        }
                        self.next();
                    }
                }
                Some(Tok::Block(_)) => {
                    // An action, or a predicate followed by ?.
                    self.next();
                    if self.peek() == Some(&Tok::Punct("?")) {
                        self.next();
                    }
                }
                Some(Tok::Angle) => {
                    self.next();
                }
                _ => {
                    if let Some(element) = self.element(rule, lexer)? {
                        elements.push(element);
                    }
                }
            }
        }
        Ok(Alt { elements, label })
    }

    fn element(&mut self, rule: &str, lexer: bool) -> Result<Option<Element>> {
        let atom = match self.next() {
            Some(Tok::Name(name)) => {
                // A label, as name=element or name+=element.
                if self.eat("=") || self.eat("+=") {
                    return self.element(rule, lexer);
                }
                if name == "EOF" { Atom::Eof } else { Atom::Rule(name) }
            }
            Some(Tok::Literal(text)) => {
                if self.eat("..") {
                    let Some(Tok::Literal(end)) = self.next() else { bail!("bad range in {rule}") };
                    let start = text.chars().next().unwrap_or(' ');
                    let end = end.chars().next().unwrap_or(' ');
                    Atom::Set { ranges: vec![(start, end)], negated: false }
                } else {
                    Atom::Literal(text)
                }
            }
            Some(Tok::Set(text)) => Atom::Set { ranges: parse_set(&text)?, negated: false },
            Some(Tok::Punct(".")) => Atom::Any,
            Some(Tok::Punct("~")) => match self.next() {
                Some(Tok::Set(text)) => Atom::Set { ranges: parse_set(&text)?, negated: true },
                Some(Tok::Literal(text)) => Atom::NotLiterals(vec![text]),
                Some(Tok::Punct("(")) => {
                    let mut literals = Vec::new();
                    let mut ranges = Vec::new();
                    loop {
                        match self.next() {
                            Some(Tok::Literal(text)) => literals.push(text),
                            Some(Tok::Set(text)) => ranges.extend(parse_set(&text)?),
                            Some(Tok::Punct("|")) => {}
                            Some(Tok::Punct(")")) => break,
                            Some(Tok::Name(_)) => {}
                            other => bail!("unexpected {other:?} in ~( ) of {rule}"),
                        }
                    }
                    for literal in &literals {
                        if let Some(c) = literal.chars().next() {
                            ranges.push((c, c));
                        }
                    }
                    if lexer { Atom::Set { ranges, negated: true } } else { Atom::NotLiterals(literals) }
                }
                other => bail!("unexpected {other:?} after ~ in {rule}"),
            },
            Some(Tok::Punct("(")) => {
                let alts = self.alternatives(rule, lexer)?;
                self.expect(")")?;
                let name = format!("{rule}__{}", self.lifted.len() + 1);
                self.lifted.push(Rule { name: name.clone(), lexer, fragment: lexer, hidden: false, parent: Some(rule.to_string()), alts });
                Atom::Rule(name)
            }
            other => bail!("unexpected {other:?} in rule {rule}"),
        };
        let suffix = match self.peek() {
            Some(Tok::Punct("?")) => Suffix::Optional,
            Some(Tok::Punct("*")) => Suffix::Star,
            Some(Tok::Punct("+")) => Suffix::Plus,
            _ => Suffix::One,
        };
        if suffix != Suffix::One {
            self.next();
            // A non-greedy marker.
            if self.peek() == Some(&Tok::Punct("?")) {
                self.next();
            }
        }
        Ok(Some(Element { atom, suffix }))
    }
}

pub fn parse(text: &str) -> Result<Grammar> {
    let mut reader = Reader { tokens: tokenize(text)?, position: 0, lifted: Vec::new() };
    let mut name = String::new();
    let mut case_insensitive = false;
    let mut rules = BTreeMap::new();
    let mut order = Vec::new();
    while let Some(token) = reader.next() {
        match token {
            Tok::Name(word) if word == "grammar" || word == "parser" || word == "lexer" => {
                if word != "grammar" {
                    reader.next();
                }
                if let Some(Tok::Name(grammar)) = reader.next() {
                    name = grammar;
                }
                reader.expect(";")?;
            }
            Tok::Name(word) if word == "options" || word == "tokens" || word == "channels" => {
                if let Some(Tok::Block(body)) = reader.next()
                    && word == "options"
                    && body.replace(' ', "").contains("caseInsensitive=true")
                {
                    case_insensitive = true;
                }
            }
            Tok::Name(word) if word == "import" || word == "mode" => while !matches!(reader.next(), Some(Tok::Punct(";")) | None) {},
            Tok::Punct("@") => {
                // A named action: @header { ... } or @parser::members { ... }.
                while !matches!(reader.next(), Some(Tok::Block(_)) | None) {}
            }
            Tok::Name(word) => {
                let (fragment, rule) = if word == "fragment" {
                    match reader.next() {
                        Some(Tok::Name(rule)) => (true, rule),
                        other => bail!("expected a rule name after fragment, found {other:?}"),
                    }
                } else {
                    (false, word)
                };
                // Rule options, arguments, and returns clauses.
                while !matches!(reader.peek(), Some(Tok::Punct(":")) | None) {
                    reader.next();
                }
                reader.expect(":")?;
                let lexer = rule.chars().next().is_some_and(|c| c.is_uppercase());
                let start = reader.position;
                let alts = reader.alternatives(&rule, lexer).with_context(|| format!("in rule {rule}"))?;
                let hidden = lexer
                    && reader.tokens[start..reader.position].iter().enumerate().any(|(index, token)| {
                        *token == Tok::Punct("->")
                            && matches!(reader.tokens.get(start + index + 1), Some(Tok::Name(command)) if command == "skip" || command == "channel")
                    });
                reader.expect(";").with_context(|| format!("at the end of rule {rule}"))?;
                if !lexer {
                    order.push(rule.clone());
                }
                rules.insert(rule.clone(), Rule { name: rule, lexer, fragment, hidden, parent: None, alts });
            }
            other => bail!("unexpected {other:?} at the top level of the grammar"),
        }
    }
    for rule in reader.lifted {
        rules.insert(rule.name.clone(), rule);
    }
    // Implicit tokens (parser literals) are fine; a referenced rule that does not exist is not.
    for rule in rules.values() {
        for alt in &rule.alts {
            for element in &alt.elements {
                if let Atom::Rule(name) = &element.atom
                    && !rules.contains_key(name)
                {
                    bail!("rule {} refers to undefined rule {name}", rule.name);
                }
            }
        }
    }
    Ok(Grammar { name, case_insensitive, rules, order })
}

impl Grammar {
    pub fn load(path: &std::path::Path) -> Result<Grammar> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// The parser rule a rule belongs to: itself, or the rule a lifted group came from.
    pub fn owner<'a>(&'a self, name: &'a str) -> &'a str {
        let mut current = name;
        while let Some(parent) = self.rules.get(current).and_then(|rule| rule.parent.as_deref()) {
            current = parent;
        }
        current
    }
}
