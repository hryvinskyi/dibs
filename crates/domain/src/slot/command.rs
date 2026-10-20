//! A shell command line split into the simple commands it runs, so a slot's pattern is matched
//! against one command at a time.
//!
//! The line is split on `;`, `&&`, `||`, `|`, `&`, parentheses and line breaks outside quotes.
//! Each command loses what only prepares it — `VAR=value` assignments, `env` and its options,
//! `command`, `exec`, `nohup`, `time`, `sudo` and its options, the shell's `if`/`then`/`do`/`!`/`{`
//! words — and a leading `./`; `sh -c`, `bash -c`, `zsh -c` and `dash -c` have their script split
//! in turn. Heredoc bodies, comments and quoted arguments are never split into commands. A line
//! whose quotes or substitutions do not close is one command, the whole line, so it errs towards
//! asking for a slot. Nothing here can see an alias, a shell function, `eval` or what a script
//! calls.

/// The most bytes of one simple command a pattern sees.
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;

/// How deep `sh -c` scripts are unwrapped.
const MAX_DEPTH: usize = 4;

/// One simple command: its words, quotes removed, and the text a pattern is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    /// The words, quotes removed.
    pub words: Vec<String>,
    /// The words joined by single spaces, cut to [`MAX_COMMAND_BYTES`] at a character boundary.
    pub text: String,
}

impl SimpleCommand {
    fn of(words: Vec<String>) -> Self {
        let text = cap(&words.join(" "));
        Self { words, text }
    }
}

/// The simple commands a command line runs, in order.
pub fn split_commands(line: &str) -> Vec<SimpleCommand> {
    split_at(line, 0)
}

fn split_at(line: &str, depth: usize) -> Vec<SimpleCommand> {
    let Some(commands) = tokenize(line) else {
        let words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        return vec![SimpleCommand {
            words,
            text: cap(line.trim()),
        }];
    };
    commands
        .into_iter()
        .flat_map(|words| unwrap(words, depth))
        .collect()
}

fn cap(text: &str) -> String {
    if text.len() <= MAX_COMMAND_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_COMMAND_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A word that only prepares the command after it, and its options that take a value.
struct Prefix {
    word: &'static str,
    /// Options followed by a value.
    with_value: &'static [&'static str],
}

const PREFIXES: &[Prefix] = &[
    Prefix {
        word: "env",
        with_value: &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"],
    },
    Prefix {
        word: "command",
        with_value: &[],
    },
    Prefix {
        word: "exec",
        with_value: &["-a"],
    },
    Prefix {
        word: "nohup",
        with_value: &[],
    },
    Prefix {
        word: "time",
        with_value: &["-f", "-o", "--format", "--output"],
    },
    Prefix {
        word: "sudo",
        with_value: &[
            "-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "-T", "--user", "--group",
            "--host", "--prompt", "--chdir",
        ],
    },
];

const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash"];

fn unwrap(mut words: Vec<String>, depth: usize) -> Vec<SimpleCommand> {
    loop {
        let Some(first) = words.first() else {
            return Vec::new();
        };
        if KEYWORDS.contains(&first.as_str()) || is_assignment(first) {
            words.remove(0);
            continue;
        }
        let Some(prefix) = PREFIXES.iter().find(|prefix| prefix.word == first) else {
            break;
        };
        words.remove(0);
        drop_options(&mut words, prefix.with_value);
    }
    if let Some(script) = shell_script(&words)
        && depth < MAX_DEPTH
    {
        return split_at(&script, depth + 1);
    }
    if let Some(first) = words.first_mut()
        && let Some(rest) = first.strip_prefix("./")
        && !rest.is_empty()
    {
        *first = rest.to_owned();
    }
    vec![SimpleCommand::of(words)]
}

/// Drops the options at the front of `words`, and the assignments `env` takes among them.
fn drop_options(words: &mut Vec<String>, with_value: &[&str]) {
    while let Some(word) = words.first() {
        if word == "--" {
            words.remove(0);
            return;
        }
        if is_assignment(word) {
            words.remove(0);
            continue;
        }
        if !word.starts_with('-') {
            return;
        }
        let takes_value = with_value.contains(&word.as_str());
        words.remove(0);
        if takes_value && !words.is_empty() {
            words.remove(0);
        }
    }
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The script of `sh -c '<script>'` and its kin.
fn shell_script(words: &[String]) -> Option<String> {
    let shell = words.first()?;
    let name = shell.rsplit('/').next().unwrap_or(shell);
    if !SHELLS.contains(&name) {
        return None;
    }
    let mut rest = words.iter().skip(1);
    while let Some(word) = rest.next() {
        if !word.starts_with('-') || word.starts_with("--") {
            return None;
        }
        if word.contains('c') {
            return rest.next().cloned();
        }
    }
    None
}

/// The command line as lists of words, one list per simple command; `None` when a quote, a
/// substitution or an escape does not close.
fn tokenize(line: &str) -> Option<Vec<Vec<String>>> {
    let chars: Vec<char> = line.chars().collect();
    let mut lexer = Lexer {
        chars: &chars,
        at: 0,
        commands: Vec::new(),
        words: Vec::new(),
        word: String::new(),
        in_word: false,
        heredocs: Vec::new(),
    };
    lexer.run()?;
    Some(lexer.commands)
}

struct Lexer<'a> {
    chars: &'a [char],
    at: usize,
    commands: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
    heredocs: Vec<(String, bool)>,
}

impl Lexer<'_> {
    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn push(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }

    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
            self.in_word = false;
        }
    }

    fn end_command(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.commands.push(std::mem::take(&mut self.words));
        }
    }

    fn run(&mut self) -> Option<()> {
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '\'' => self.single_quoted()?,
                '"' => self.double_quoted()?,
                '\\' => match self.peek(0) {
                    Some('\n') => self.at += 1,
                    Some(next) => {
                        self.at += 1;
                        self.push(next);
                    }
                    None => return None,
                },
                '$' if self.peek(0) == Some('(') => {
                    self.push('$');
                    self.substitution()?;
                }
                '`' => self.backquoted()?,
                ' ' | '\t' | '\r' => self.end_word(),
                '\n' => {
                    self.end_command();
                    self.skip_heredocs();
                }
                ';' | '(' | ')' => self.end_command(),
                '|' => {
                    if matches!(self.peek(0), Some('|' | '&')) {
                        self.at += 1;
                    }
                    self.end_command();
                }
                '&' => {
                    if self.word.ends_with(['>', '<']) || self.peek(0) == Some('>') {
                        self.push('&');
                    } else {
                        if self.peek(0) == Some('&') {
                            self.at += 1;
                        }
                        self.end_command();
                    }
                }
                '#' if !self.in_word => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                '<' if !self.in_word && self.peek(0) == Some('<') && self.peek(1) != Some('<') => {
                    self.at += 1;
                    self.heredoc()?;
                }
                other => self.push(other),
            }
        }
        self.end_command();
        Some(())
    }

    fn single_quoted(&mut self) -> Option<()> {
        self.in_word = true;
        loop {
            let c = self.peek(0)?;
            self.at += 1;
            if c == '\'' {
                return Some(());
            }
            self.word.push(c);
        }
    }

    fn double_quoted(&mut self) -> Option<()> {
        self.in_word = true;
        loop {
            let c = self.peek(0)?;
            self.at += 1;
            match c {
                '"' => return Some(()),
                '\\' => {
                    let next = self.peek(0)?;
                    self.at += 1;
                    match next {
                        '"' | '\\' | '$' | '`' => self.word.push(next),
                        '\n' => {}
                        other => {
                            self.word.push('\\');
                            self.word.push(other);
                        }
                    }
                }
                other => self.word.push(other),
            }
        }
    }

    /// `$( … )`, kept in the word, nested parentheses and quotes inside it included.
    fn substitution(&mut self) -> Option<()> {
        let mut depth = 0usize;
        loop {
            let c = self.peek(0)?;
            self.at += 1;
            self.word.push(c);
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(());
                    }
                }
                '\'' => loop {
                    let inner = self.peek(0)?;
                    self.at += 1;
                    self.word.push(inner);
                    if inner == '\'' {
                        break;
                    }
                },
                '\\' => {
                    let inner = self.peek(0)?;
                    self.at += 1;
                    self.word.push(inner);
                }
                _ => {}
            }
        }
    }

    fn backquoted(&mut self) -> Option<()> {
        self.push('`');
        loop {
            let c = self.peek(0)?;
            self.at += 1;
            self.word.push(c);
            if c == '`' {
                return Some(());
            }
        }
    }

    /// `<<WORD`, `<<-WORD`, `<<'WORD'`: the body that follows the line is skipped at its end.
    fn heredoc(&mut self) -> Option<()> {
        self.end_word();
        let strip_tabs = self.peek(0) == Some('-');
        if strip_tabs {
            self.at += 1;
        }
        while matches!(self.peek(0), Some(' ' | '\t')) {
            self.at += 1;
        }
        let mut delimiter = String::new();
        while let Some(c) = self.peek(0) {
            if c.is_whitespace() || ";|&()<>".contains(c) {
                break;
            }
            self.at += 1;
            if c != '\'' && c != '"' && c != '\\' {
                delimiter.push(c);
            }
        }
        if delimiter.is_empty() {
            return None;
        }
        self.heredocs.push((delimiter, strip_tabs));
        Some(())
    }

    fn skip_heredocs(&mut self) {
        for (delimiter, strip_tabs) in std::mem::take(&mut self.heredocs) {
            loop {
                if self.peek(0).is_none() {
                    return;
                }
                let start = self.at;
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.at += 1;
                }
                let line: String = self.chars[start..self.at].iter().collect();
                if self.peek(0) == Some('\n') {
                    self.at += 1;
                }
                let line = if strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if line == delimiter {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(line: &str) -> Vec<String> {
        split_commands(line).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn the_splitter_table() {
        let cases: &[(&str, &[&str])] = &[
            ("./deploy.sh prod", &["deploy.sh prod"]),
            ("cd deploy && ./release.sh", &["cd deploy", "release.sh"]),
            ("a; b || c | d & e", &["a", "b", "c", "d", "e"]),
            ("a\nb", &["a", "b"]),
            ("sh -c './deploy.sh prod'", &["deploy.sh prod"]),
            ("bash -lc 'cd x && make release'", &["cd x", "make release"]),
            ("/bin/sh -c \"./deploy.sh\"", &["deploy.sh"]),
            ("FOO=1 BAR=2 ./deploy.sh", &["deploy.sh"]),
            ("env -i FOO=1 ./deploy.sh", &["deploy.sh"]),
            ("env -u HOME make release", &["make release"]),
            ("sudo -u deploy ./deploy.sh", &["deploy.sh"]),
            ("nohup time -p command ./deploy.sh &", &["deploy.sh"]),
            ("exec ./deploy.sh", &["deploy.sh"]),
            ("echo \"./deploy.sh\"", &["echo ./deploy.sh"]),
            (
                "echo 'never run ./release.sh'",
                &["echo never run ./release.sh"],
            ),
            (
                "cat <<EOF > notes.txt\n./deploy.sh\nEOF\nls",
                &["cat > notes.txt", "ls"],
            ),
            (
                "cat <<-'END'\n\t./deploy.sh\n\tEND\n./after.sh",
                &["cat", "after.sh"],
            ),
            ("./deploy.sh 2>&1 | tee log", &["deploy.sh 2>&1", "tee log"]),
            ("./deploy.sh &> log", &["deploy.sh &> log"]),
            (
                "if ./check.sh; then ./deploy.sh; fi",
                &["check.sh", "deploy.sh", "fi"],
            ),
            ("(cd x && ./deploy.sh)", &["cd x", "deploy.sh"]),
            ("echo $(./deploy.sh) done", &["echo $(./deploy.sh) done"]),
            ("ls # ./deploy.sh", &["ls"]),
            ("a\\\nb", &["ab"]),
            ("", &[]),
        ];
        for (line, expected) in cases {
            assert_eq!(texts(line), *expected, "{line:?}");
        }
    }

    #[test]
    fn an_unbalanced_line_is_one_command_matched_whole() {
        assert_eq!(
            texts("echo 'oops && ./deploy.sh"),
            ["echo 'oops && ./deploy.sh"]
        );
        assert_eq!(texts("echo $(./deploy.sh"), ["echo $(./deploy.sh"]);
        assert_eq!(texts("echo \"x"), ["echo \"x"]);
    }

    #[test]
    fn words_keep_quoted_spaces() {
        let commands = split_commands("git commit -m 'a b' && git push origin main");
        assert_eq!(commands[0].words, ["git", "commit", "-m", "a b"]);
        assert_eq!(commands[1].words, ["git", "push", "origin", "main"]);
    }

    #[test]
    fn a_long_command_is_cut_at_a_character_boundary() {
        let line = format!("echo {}", "é".repeat(MAX_COMMAND_BYTES));
        let text = &split_commands(&line)[0].text;
        assert!(text.len() <= MAX_COMMAND_BYTES);
        assert!(text.starts_with("echo é"));
    }

    #[test]
    fn nested_shells_unwrap_to_a_limit() {
        assert_eq!(texts("sh -c \"bash -c './deploy.sh'\""), ["deploy.sh"]);
    }
}
