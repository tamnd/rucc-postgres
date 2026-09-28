//! `rpg asm-audit`: every inline assembly statement in the pinned tree, taken apart.
//!
//! `demands.toml` says where inline assembly is written. This says what each statement asks of
//! the compiler: its template, its outputs and inputs with their constraints, its clobbers and its
//! goto labels, with counts per constraint and per clobber at the top, since those are what a
//! compiler has to understand to accept the statement at all.
//!
//! The scanner is small and hand written, not a C parser. It walks the text once, skipping
//! comments and string and character literals, and follows preprocessor lines so that it knows
//! which macro a statement sits in and which `#if` lines guard it. When it meets `asm`, `__asm`
//! or `__asm__`, with any of `volatile`, `__volatile__`, `inline` and `goto` after it, and then an
//! opening parenthesis, it splits what is inside at the colons and commas that are not nested in
//! parentheses, brackets, braces or literals. A `__asm` with no parenthesis is the MSVC form,
//! which is recorded as it is written.
//!
//! The enclosing function is found by the simplest rule that works on Postgres: the name before
//! the parameter list of the last `{` at file level that followed a `)`. Braces on preprocessor
//! lines are not counted, because macros such as `PG_TRY` open a brace and leave it open. An
//! `#if` that opens a brace in two branches can still confuse it, so it is a hint.
//!
//! Like `rpg demands`, it counts what is written, not what one target compiles. The `guard` of a
//! statement lists the `#if` lines it sits under, which is where the target shows.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One output or input operand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operand {
    /// The symbolic name in `[name]`, when there is one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The constraint, with the quotes removed.
    pub constraint: String,
    /// The C expression, without its parentheses.
    pub expr: String,
}

/// One statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Statement {
    /// The file, relative to the source tree.
    pub file: String,
    /// The line of the keyword.
    pub line: usize,
    /// The function it is in, when the scanner could tell.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub function: String,
    /// The macro whose definition it is in, when it is in one.
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "macro")]
    pub in_macro: String,
    /// The `#if` lines around it, outermost first, without include guards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guard: Vec<String>,
    /// `asm`, `__asm` or `__asm__`.
    pub keyword: String,
    /// `volatile`, `goto` and the like, as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<String>,
    /// `extended` with colons, `basic` without, `msvc` for the MSVC block form.
    pub kind: String,
    /// The template: the text of its string literals joined, escapes left as written.
    pub template: String,
    /// Output operands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<Operand>,
    /// Input operands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Operand>,
    /// Clobbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clobbers: Vec<String>,
    /// Goto labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// `asm-audit.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Audit {
    /// The pin scanned.
    pub pin: String,
    /// Its commit.
    pub commit: String,
    /// The directories scanned, relative to the source.
    pub scanned: Vec<String>,
    /// Files scanned.
    pub file_count: usize,
    /// Statements found.
    pub statement_count: usize,
    /// Statements per file.
    pub files: BTreeMap<String, usize>,
    /// Statements per kind.
    pub kinds: BTreeMap<String, usize>,
    /// Statements per qualifier.
    pub qualifiers: BTreeMap<String, usize>,
    /// Output operands per constraint as written.
    pub output_constraints: BTreeMap<String, usize>,
    /// Input operands per constraint as written.
    pub input_constraints: BTreeMap<String, usize>,
    /// Operands per constraint letter, with the modifiers `=`, `+`, `&` and `%` counted apart and
    /// a matching constraint such as `0` counted as `matching`.
    pub constraint_letters: BTreeMap<String, usize>,
    /// Clobbers.
    pub clobbers: BTreeMap<String, usize>,
    /// The statements, in file and line order.
    #[serde(default)]
    pub asm: Vec<Statement>,
}

/// Words that may follow the keyword before the parenthesis.
const QUALIFIERS: &[&str] = &[
    "volatile",
    "__volatile__",
    "__volatile",
    "inline",
    "__inline__",
    "__inline",
    "goto",
];

/// The scanner's view of one file.
struct Scan<'a> {
    text: &'a [char],
    pos: usize,
    line: usize,
}

impl Scan<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.text.get(self.pos + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek(0)?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    /// Skip a comment starting here, returning whether there was one. A block comment counts as
    /// whitespace; a line comment stops before its newline.
    fn comment(&mut self) -> bool {
        match (self.peek(0), self.peek(1)) {
            (Some('/'), Some('*')) => {
                self.pos += 2;
                while let Some(c) = self.bump() {
                    if c == '*' && self.peek(0) == Some('/') {
                        self.pos += 1;
                        break;
                    }
                }
                true
            }
            (Some('/'), Some('/')) => {
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
                true
            }
            _ => false,
        }
    }

    /// Copy a string or character literal starting here, quotes included. It stops at an
    /// unescaped newline, which is an apostrophe in an `#error` line rather than a literal.
    fn literal(&mut self, out: &mut String) {
        let quote = self.bump().unwrap_or('"');
        out.push(quote);
        while let Some(c) = self.peek(0) {
            if c == '\n' {
                return;
            }
            self.bump();
            if c == '\\' && self.peek(0) == Some('\n') {
                self.bump();
                continue;
            }
            out.push(c);
            if c == '\\' {
                if let Some(next) = self.bump() {
                    out.push(next);
                }
            } else if c == quote {
                return;
            }
        }
    }

    /// Skip whitespace, comments and backslash newlines.
    fn blank(&mut self) {
        loop {
            match self.peek(0) {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('\\') if self.peek(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                }
                Some('/') if self.comment() => {}
                _ => return,
            }
        }
    }

    fn word(&mut self) -> String {
        let mut word = String::new();
        while let Some(c) = self
            .peek(0)
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            word.push(c);
            self.pos += 1;
        }
        word
    }

    /// The rest of a preprocessor line, continuations joined, comments dropped, spaces squeezed.
    fn directive_line(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None | Some('\n') => break,
                Some('\\') if self.peek(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                    out.push(' ');
                }
                Some('/') if self.comment() => out.push(' '),
                Some('"' | '\'') => self.literal(&mut out),
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
        squeeze(&out)
    }

    /// Read from just inside an opening parenthesis to its match, splitting at the colons and
    /// commas that are not nested. Comments and backslash newlines become spaces.
    fn operands(&mut self) -> Vec<Vec<String>> {
        let mut sections = vec![Vec::new()];
        let mut item = String::new();
        let mut depth = 0usize;
        let finish = |sections: &mut Vec<Vec<String>>, item: &mut String| {
            let text = squeeze(item);
            item.clear();
            if let Some(last) = sections.last_mut()
                && !text.is_empty()
            {
                last.push(text);
            }
        };
        loop {
            match self.peek(0) {
                None => break,
                Some('"' | '\'') => self.literal(&mut item),
                Some('/') if self.comment() => item.push(' '),
                Some('\\') if self.peek(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                    item.push(' ');
                }
                Some(c) => {
                    self.bump();
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' if depth == 0 => {
                            finish(&mut sections, &mut item);
                            break;
                        }
                        ')' | ']' | '}' => depth -= 1,
                        ':' if depth == 0 => {
                            finish(&mut sections, &mut item);
                            sections.push(Vec::new());
                            continue;
                        }
                        ',' if depth == 0 => {
                            finish(&mut sections, &mut item);
                            continue;
                        }
                        _ => {}
                    }
                    item.push(c);
                }
            }
        }
        sections
    }
}

/// Collapse runs of whitespace outside literals to one space and trim.
fn squeeze(text: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            if c == '"' || c == '\'' {
                quote = Some(c);
            }
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Split leading string literals off a piece of text: their joined contents, and the rest.
/// Returns `None` when the text does not start with one.
fn leading_strings(text: &str) -> Option<(String, &str)> {
    let mut rest = text.trim_start();
    let mut joined = String::new();
    let mut any = false;
    while let Some(body) = rest.strip_prefix('"') {
        let mut end = None;
        let mut escaped = false;
        for (i, c) in body.char_indices() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = Some(i);
                break;
            }
        }
        let end = end?;
        joined.push_str(&body[..end]);
        rest = body[end + 1..].trim_start();
        any = true;
    }
    any.then_some((joined, rest))
}

/// Read one operand: `[name] "constraint" (expr)`.
fn operand(text: &str) -> Operand {
    let mut rest = text.trim();
    let mut name = String::new();
    if let Some(inner) = rest.strip_prefix('[')
        && let Some(end) = inner.find(']')
    {
        name = inner[..end].trim().to_string();
        rest = inner[end + 1..].trim_start();
    }
    let (constraint, rest) = leading_strings(rest).unwrap_or_else(|| {
        let at = rest.find('(').unwrap_or(rest.len());
        (rest[..at].trim().to_string(), &rest[at..])
    });
    let expr = rest.trim();
    let expr = expr
        .strip_prefix('(')
        .and_then(|e| e.strip_suffix(')'))
        .unwrap_or(expr)
        .trim()
        .to_string();
    Operand {
        name,
        constraint,
        expr,
    }
}

/// The text of a template or clobber: its string literals joined, or the text itself.
fn string_or_text(text: &str) -> String {
    match leading_strings(text) {
        Some((joined, "")) => joined,
        _ => text.to_string(),
    }
}

/// One entry of the `#if` stack: the line that opened it and the branch now taken.
struct Branch {
    opened: String,
    current: String,
    /// An include guard, which is left out of `guard`.
    include_guard: bool,
}

/// Find the statements in one file's text.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn scan_text(file: &str, text: &str) -> Vec<Statement> {
    let chars: Vec<char> = text.chars().collect();
    let mut s = Scan {
        text: &chars,
        pos: 0,
        line: 1,
    };
    let mut found = Vec::new();
    let mut branches: Vec<Branch> = Vec::new();
    let mut depth = 0usize;
    let mut function = String::new();
    // The significant tokens at file level since the last `;` or `}`, for finding a function name.
    let mut recent: Vec<String> = Vec::new();
    let mut in_macro = String::new();
    let mut macro_line_end = false;
    let mut line_start = true;
    while let Some(c) = s.peek(0) {
        if c == '\n' {
            s.bump();
            line_start = true;
            if macro_line_end {
                in_macro.clear();
                macro_line_end = false;
            }
            continue;
        }
        if c == '\\' && s.peek(1) == Some('\n') {
            s.bump();
            s.bump();
            continue;
        }
        if c.is_whitespace() {
            s.bump();
            continue;
        }
        if s.comment() {
            continue;
        }
        let at_line_start = std::mem::replace(&mut line_start, false);
        if c == '#' && at_line_start {
            s.bump();
            s.blank_inline();
            let directive = s.word();
            if directive == "define" {
                s.blank_inline();
                in_macro = s.word();
                macro_line_end = true;
                continue;
            }
            let rest = s.directive_line();
            let full = squeeze(&format!("#{directive} {rest}"));
            match directive.as_str() {
                "if" | "ifdef" | "ifndef" => {
                    let include_guard = directive == "ifndef"
                        && rest.ends_with("_H")
                        && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    branches.push(Branch {
                        opened: full.clone(),
                        current: full,
                        include_guard,
                    });
                }
                "elif" | "elifdef" | "elifndef" => {
                    if let Some(top) = branches.last_mut() {
                        top.current = full;
                    }
                }
                "else" => {
                    if let Some(top) = branches.last_mut() {
                        top.current = format!("#else of {}", top.opened);
                    }
                }
                "endif" => {
                    branches.pop();
                }
                _ => {}
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let mut ignored = String::new();
            s.literal(&mut ignored);
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let line = s.line;
            let word = s.word();
            if matches!(word.as_str(), "asm" | "__asm" | "__asm__")
                && let Some(statement) = statement(&mut s, &word)
            {
                found.push(Statement {
                    file: file.to_string(),
                    line,
                    function: if in_macro.is_empty() {
                        function.clone()
                    } else {
                        String::new()
                    },
                    in_macro: in_macro.clone(),
                    guard: branches
                        .iter()
                        .filter(|b| !b.include_guard)
                        .map(|b| b.current.clone())
                        .collect(),
                    ..statement
                });
                continue;
            }
            if depth == 0 && in_macro.is_empty() {
                recent.push(word);
            }
            continue;
        }
        s.bump();
        if !in_macro.is_empty() {
            continue;
        }
        match c {
            '{' => {
                if depth == 0 {
                    function = function_name(&recent).unwrap_or_default();
                    recent.clear();
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    function.clear();
                    recent.clear();
                }
            }
            ';' if depth == 0 => recent.clear(),
            _ if depth == 0 => recent.push(c.to_string()),
            _ => {}
        }
    }
    found
}

impl Scan<'_> {
    /// Skip spaces and tabs on the current line.
    fn blank_inline(&mut self) {
        while self.peek(0).is_some_and(|c| c == ' ' || c == '\t') {
            self.pos += 1;
        }
    }
}

/// The name before the parameter list that ends the tokens, when they end with `)`.
fn function_name(tokens: &[String]) -> Option<String> {
    let mut depth = 0usize;
    let mut at = tokens.len();
    while at > 0 {
        at -= 1;
        match tokens[at].as_str() {
            ")" => depth += 1,
            "(" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let name = tokens.get(at.checked_sub(1)?)?;
                    let identifier = name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
                    return identifier.then(|| name.clone());
                }
            }
            _ if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// Read a statement after its keyword, or `None` when the keyword was not the start of one.
fn statement(s: &mut Scan, keyword: &str) -> Option<Statement> {
    let start = (s.pos, s.line);
    let mut qualifiers = Vec::new();
    loop {
        s.blank();
        let before = s.pos;
        let word = s.word();
        if word.is_empty() {
            break;
        }
        if QUALIFIERS.contains(&word.as_str()) {
            qualifiers.push(word);
        } else {
            s.pos = before;
            break;
        }
    }
    let empty = Statement {
        file: String::new(),
        line: 0,
        function: String::new(),
        in_macro: String::new(),
        guard: Vec::new(),
        keyword: keyword.to_string(),
        qualifiers,
        kind: String::new(),
        template: String::new(),
        outputs: Vec::new(),
        inputs: Vec::new(),
        clobbers: Vec::new(),
        labels: Vec::new(),
    };
    if s.peek(0) == Some('(') {
        s.bump();
        let sections = s.operands();
        let section = |i: usize| sections.get(i).cloned().unwrap_or_default();
        return Some(Statement {
            kind: if sections.len() > 1 {
                "extended"
            } else {
                "basic"
            }
            .to_string(),
            template: string_or_text(&section(0).join(", ")),
            outputs: section(1).iter().map(|o| operand(o)).collect(),
            inputs: section(2).iter().map(|o| operand(o)).collect(),
            clobbers: section(3).iter().map(|c| string_or_text(c)).collect(),
            labels: section(4),
            ..empty
        });
    }
    if keyword == "__asm" {
        // The MSVC form: a block in braces, or the rest of the statement.
        let mut body = String::new();
        if s.peek(0) == Some('{') {
            s.bump();
            while let Some(c) = s.bump() {
                if c == '}' {
                    break;
                }
                body.push(c);
            }
        } else {
            while let Some(c) = s.peek(0).filter(|c| *c != ';' && *c != '\n') {
                body.push(c);
                s.pos += 1;
            }
        }
        return Some(Statement {
            kind: "msvc".to_string(),
            template: squeeze(&body),
            ..empty
        });
    }
    (s.pos, s.line) = start;
    None
}

/// Count constraint letters in one constraint.
fn letters(constraint: &str, counts: &mut BTreeMap<String, usize>) {
    for alternative in constraint.split(',') {
        let mut digits = false;
        for c in alternative.chars() {
            if c.is_ascii_digit() {
                if !digits {
                    *counts.entry("matching".to_string()).or_default() += 1;
                }
                digits = true;
                continue;
            }
            digits = false;
            if !c.is_whitespace() {
                *counts.entry(c.to_string()).or_default() += 1;
            }
        }
    }
}

/// Build the summary tables from the statements.
#[must_use]
pub fn summarize(statements: Vec<Statement>) -> Audit {
    let mut audit = Audit {
        statement_count: statements.len(),
        ..Audit::default()
    };
    for statement in &statements {
        *audit.files.entry(statement.file.clone()).or_default() += 1;
        *audit.kinds.entry(statement.kind.clone()).or_default() += 1;
        for q in &statement.qualifiers {
            *audit.qualifiers.entry(q.clone()).or_default() += 1;
        }
        for o in &statement.outputs {
            *audit
                .output_constraints
                .entry(o.constraint.clone())
                .or_default() += 1;
            letters(&o.constraint, &mut audit.constraint_letters);
        }
        for i in &statement.inputs {
            *audit
                .input_constraints
                .entry(i.constraint.clone())
                .or_default() += 1;
            letters(&i.constraint, &mut audit.constraint_letters);
        }
        for c in &statement.clobbers {
            *audit.clobbers.entry(c.clone()).or_default() += 1;
        }
    }
    audit.asm = statements;
    audit
}

/// Every C source, header, grammar and lexer file under a directory, sorted.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("c" | "h" | "y" | "l")
        ) {
            out.push(path);
        }
    }
}

/// Scan a source tree.
pub fn scan_tree(source: &Path) -> Result<Audit, String> {
    let mut files = Vec::new();
    for root in crate::demands::ROOTS {
        sources(&source.join(root), &mut files);
    }
    if files.is_empty() {
        return Err(format!("no C files under {}", source.display()));
    }
    let mut statements = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        // Most files have no assembly at all, and a substring check is much cheaper than a scan.
        if !text.contains("asm") {
            continue;
        }
        let relative = path.strip_prefix(source).unwrap_or(path);
        statements.extend(scan_text(&relative.display().to_string(), &text));
    }
    let mut audit = summarize(statements);
    audit.scanned = crate::demands::ROOTS
        .iter()
        .map(ToString::to_string)
        .collect();
    audit.file_count = files.len();
    Ok(audit)
}

/// The header written above the tables.
pub const HEADER: &str =
    "# Every inline assembly statement in the pinned Postgres tree, written by rpg asm-audit.
#
# A text scan, not a parse: it lists what is written, not what a given target compiles, and
# `guard` shows the #if lines each statement sits under. The counts at the top are per operand
# and per clobber. `constraint-letters` splits each constraint into its letters, with the
# modifiers = + & % counted on their own and a matching constraint such as \"0\" as `matching`.

";

/// Write the file.
pub fn save(path: &Path, audit: &Audit) -> Result<(), String> {
    let body = toml::to_string(audit).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{HEADER}{body}"))
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"#ifndef S_LOCK_H
#define S_LOCK_H
/* __asm__ ("not this") */
#if defined(__x86_64__)
static __inline__ int
tas(volatile slock_t *lock)
{
	slock_t _res = 1;
	__asm__ __volatile__(
		"	lock			\n"
		"	xchgb	%0,%1	\n"
:		"+q"(_res), "+m"(*lock)
:		/* no inputs */
:		"memory", "cc");
	return (int) _res;
}
#elif defined(__powerpc__)
#define pg_memory_barrier_impl()	__asm__ __volatile__ ("sync" : : : "memory")
#else
#define S_UNLOCK(lock)	\
	do { __asm__ __volatile__("" : : : "memory");  *(lock) = 0; } while (0)
#endif
int f(int x) { char *s = "asm(no)"; asm goto ("jmp %l[out]" : : [v] "r,m" (x ? (1) : 2), "0" (x) : : out); out: return 0; }
static inline void spin(void) { __asm rep nop; }
int asm_count;
#endif
"#;

    #[test]
    fn statements_are_taken_apart() {
        let found = scan_text("s_lock.h", TEXT);
        assert_eq!(found.len(), 5, "{found:#?}");

        let tas = &found[0];
        assert_eq!((tas.line, tas.function.as_str()), (9, "tas"));
        assert_eq!(tas.qualifiers, ["__volatile__"]);
        assert_eq!(tas.kind, "extended");
        assert_eq!(tas.template, r"	lock			\n	xchgb	%0,%1	\n");
        assert_eq!(tas.outputs.len(), 2);
        assert_eq!(tas.outputs[1].constraint, "+m");
        assert_eq!(tas.outputs[1].expr, "*lock");
        assert!(tas.inputs.is_empty());
        assert_eq!(tas.clobbers, ["memory", "cc"]);
        assert_eq!(tas.guard, ["#if defined(__x86_64__)"]);

        let barrier = &found[1];
        assert_eq!(barrier.in_macro, "pg_memory_barrier_impl");
        assert_eq!(barrier.template, "sync");
        assert_eq!(barrier.guard, ["#elif defined(__powerpc__)"]);

        let unlock = &found[2];
        assert_eq!((unlock.line, unlock.in_macro.as_str()), (21, "S_UNLOCK"));
        assert_eq!(unlock.template, "");
        assert_eq!(unlock.guard, ["#else of #if defined(__x86_64__)"]);

        let goto = &found[3];
        assert_eq!(goto.function, "f");
        assert!(goto.in_macro.is_empty());
        assert_eq!(goto.qualifiers, ["goto"]);
        assert_eq!(goto.inputs[0].name, "v");
        assert_eq!(goto.inputs[0].constraint, "r,m");
        assert_eq!(goto.inputs[0].expr, "x ? (1) : 2");
        assert_eq!(goto.inputs[1].constraint, "0");
        assert_eq!(goto.labels, ["out"]);
        assert!(goto.guard.is_empty());

        let msvc = &found[4];
        assert_eq!(
            (msvc.kind.as_str(), msvc.template.as_str()),
            ("msvc", "rep nop")
        );
        assert_eq!(msvc.function, "spin");
    }

    #[test]
    fn counts_are_per_operand_and_per_letter() {
        let audit = summarize(scan_text("s_lock.h", TEXT));
        assert_eq!(audit.statement_count, 5);
        assert_eq!(audit.output_constraints["+q"], 1);
        assert_eq!(audit.input_constraints["r,m"], 1);
        assert_eq!(audit.constraint_letters["+"], 2);
        assert_eq!(audit.constraint_letters["matching"], 1);
        assert_eq!(audit.constraint_letters["r"], 1);
        assert_eq!(audit.clobbers["memory"], 3);
        assert_eq!(audit.kinds["msvc"], 1);
        let text = toml::to_string(&audit).unwrap();
        let back: Audit = toml::from_str(&text).unwrap();
        assert_eq!(back, audit);
    }
}
