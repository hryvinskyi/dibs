//! References in a member's free text — a title, a message, a reason, a note: the addresses, the
//! tracker references, the issues and pull requests, the areas and the commits it names, found by
//! one pass over its characters so that a page can link each one and print the rest as written.

use crate::area::QualifiedArea;
use crate::item::ExternalRef;
use crate::ref_scheme::RefSchemeRegistry;
use crate::repo::RepoId;
use crate::settings::ProjectSettings;

/// What a scan knows about the project a text belongs to.
#[derive(Debug, Clone, Copy)]
pub struct RefContext<'a> {
    /// The reference schemes the deployment knows.
    pub schemes: &'a RefSchemeRegistry,
    /// The project's settings: its repositories, their remotes and their areas.
    pub settings: &'a ProjectSettings,
    /// The repository a bare `#N` and a commit hash belong to, when the text is about one.
    pub home: Option<&'a RepoId>,
}

/// A piece of a scanned text: its text as written, and the reference it is, when it is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPiece {
    /// The piece's text, exactly as it stands in the scanned text.
    pub text: String,
    /// The reference the piece is, or `None` for plain text.
    pub reference: Option<TextRef>,
}

/// A reference found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextRef {
    /// An `http` or `https` address, as written.
    Url(String),
    /// A reference of a scheme the deployment knows, such as `gh:acme/widgets#42`.
    Ticket(ExternalRef),
    /// Issue or pull request `number` of a repository of the project.
    Numbered {
        /// The repository.
        repo: RepoId,
        /// The issue's or pull request's number; never zero.
        number: u64,
    },
    /// An area the project declares.
    Area(QualifiedArea),
    /// A commit of a repository of the project, its hash lower-cased.
    Commit {
        /// The repository.
        repo: RepoId,
        /// The commit's hash, 7 to 40 hexadecimal digits in lower case.
        sha: String,
    },
}

/// The pieces of `text`, in order. Joined, their texts are `text`; adjacent plain pieces are one
/// piece.
///
/// What is recognised, where two candidates start at one place the first of this list winning:
///
/// 1. an address: `http://` or `https://`, in any case, and what follows up to whitespace, at least
///    one character of it kept once trailing punctuation is given back;
/// 2. a ticket: `<scheme>:<value>`, starting where a run of scheme characters (ASCII letters,
///    digits, `+`, `-`, `.`) starts, with a scheme the registry knows, the token up to whitespace,
///    less its trailing punctuation, being a reference [`ExternalRef::parse`] accepts;
/// 3. `owner/name#N`, where `owner/name` is, ignoring case, the path of the remote of a repository
///    of the project and `N` a positive number;
/// 4. `repo:name`, an area the project declares;
/// 5. `#N`, when the text has a home repository, at the start of the text or after whitespace or
///    one of `( [ {`;
/// 6. a commit hash, when the text has a home repository: 7 to 40 hexadecimal digits, holding at
///    least one digit and one letter.
///
/// Trailing punctuation is `. , ; : ! ? ' "` and a closing `) ] }`, given back one character at a
/// time from the end, except that a closing bracket stays while the token holds more opening than
/// closing brackets of its kind before it. Kinds 3 and 4 never start right after a letter, a digit,
/// `.`, `_`, `-`, `/` or `:`, so neither is the tail of a longer name or an address; kinds 3, 4, 5
/// and 6 never end right before a letter, a digit or `_`, and a commit hash never starts right
/// after one.
pub fn scan(text: &str, context: &RefContext<'_>) -> Vec<TextPiece> {
    let scanner = Scanner { text, context };
    let mut pieces = Vec::new();
    let mut plain_from = 0;
    let mut at = 0;
    let mut word = Word::from(text, 0);
    while let Some(next) = scanner.char_at(at) {
        if word.end < at {
            word = Word::from(text, at);
        }
        match scanner.reference_at(at, word) {
            Some((end, reference)) => {
                scanner.push_plain(&mut pieces, plain_from, at);
                pieces.push(TextPiece {
                    text: scanner.slice(at, end).to_owned(),
                    reference: Some(reference),
                });
                at = end;
                plain_from = end;
            }
            None => at += next.len_utf8(),
        }
    }
    scanner.push_plain(&mut pieces, plain_from, text.len());
    pieces
}

/// The longest a reference can be written, in bytes: the longest scheme, its `:`, and the longest
/// value at four bytes a character. A longer token is never a ticket.
const LONGEST_TICKET: usize = ExternalRef::LONGEST_SCHEME + 1 + ExternalRef::LONGEST_VALUE * 4;

/// The characters a token gives back to the text when they end it, besides closing brackets.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '\'', '"'];

/// The whitespace-delimited run of text a position lies in, by where it ends.
#[derive(Debug, Clone, Copy)]
struct Word {
    /// Where the run ends: the first whitespace at or after the position it was found from, or
    /// the end of the text.
    end: usize,
    /// Where the run ends once every trailing punctuation character and closing bracket is left
    /// out; never before the position it was found from.
    core_end: usize,
}

impl Word {
    /// The run that `at` starts or lies in.
    fn from(text: &str, at: usize) -> Self {
        let rest = text.get(at..).unwrap_or_default();
        let length = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = rest.get(..length).unwrap_or_default();
        let core = word.trim_end_matches(|c: char| TRAILING.contains(&c) || closes(c).is_some());
        Self {
            end: at + length,
            core_end: at + core.len(),
        }
    }
}

/// One pass over a text, asking at each position whether a reference starts there.
struct Scanner<'s> {
    text: &'s str,
    context: &'s RefContext<'s>,
}

impl Scanner<'_> {
    /// The reference that starts at `at` and the position after it, the first kind that matches
    /// winning.
    fn reference_at(&self, at: usize, word: Word) -> Option<(usize, TextRef)> {
        self.address(at, word)
            .or_else(|| self.ticket(at, word))
            .or_else(|| self.numbered(at))
            .or_else(|| self.area(at))
            .or_else(|| self.bare_number(at))
            .or_else(|| self.commit(at))
    }

    /// An `http` or `https` address up to whitespace, less its trailing punctuation.
    fn address(&self, at: usize, word: Word) -> Option<(usize, TextRef)> {
        let rest = self.text.get(at..)?.as_bytes();
        let prefix = [b"http://".as_slice(), b"https://".as_slice()]
            .into_iter()
            .find(|prefix| {
                rest.get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            })?;
        let end = at + trimmed_len(self.text.get(at..word.end)?);
        (end > at + prefix.len()).then(|| (end, TextRef::Url(self.slice(at, end).to_owned())))
    }

    /// A reference of a scheme the registry knows, up to whitespace, less its trailing
    /// punctuation.
    fn ticket(&self, at: usize, word: Word) -> Option<(usize, TextRef)> {
        if self.before(at).is_some_and(is_scheme_char) {
            return None;
        }
        if !self.char_at(at)?.is_ascii_alphabetic() {
            return None;
        }
        let scheme_end = self.run(at, is_scheme_char);
        if self.char_at(scheme_end) != Some(':')
            || !self.context.schemes.knows(self.slice(at, scheme_end))
            || word.core_end.saturating_sub(at) > LONGEST_TICKET
        {
            return None;
        }
        let end = at + trimmed_len(self.text.get(at..word.end)?);
        let reference = ExternalRef::parse(self.slice(at, end)).ok()?;
        Some((end, TextRef::Ticket(reference)))
    }

    /// `owner/name#N` of a repository of the project, by its remote's path.
    fn numbered(&self, at: usize) -> Option<(usize, TextRef)> {
        if self.before(at).is_some_and(continues_a_name) {
            return None;
        }
        let path_end = self.run(at, is_path_char);
        if path_end == at || self.char_at(path_end) != Some('#') {
            return None;
        }
        let path = self.slice(at, path_end);
        let repo = self.context.settings.repos().iter().find(|repo| {
            repo.remote
                .as_ref()
                .is_some_and(|remote| remote.path().eq_ignore_ascii_case(path))
        })?;
        let (end, number) = self.number(path_end + 1)?;
        Some((
            end,
            TextRef::Numbered {
                repo: repo.id.clone(),
                number,
            },
        ))
    }

    /// `repo:name`, an area the project declares.
    fn area(&self, at: usize) -> Option<(usize, TextRef)> {
        if self.before(at).is_some_and(continues_a_name) {
            return None;
        }
        let repo_end = self.run(at, |c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
        });
        if repo_end == at || self.char_at(repo_end) != Some(':') {
            return None;
        }
        let name_from = repo_end + 1;
        let name_end = self.run(name_from, |c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c)
        });
        let name_end = name_from + self.slice(name_from, name_end).trim_end_matches('.').len();
        if self.char_at(name_end).is_some_and(is_word_char) {
            return None;
        }
        let area = QualifiedArea::parse(self.slice(at, name_end)).ok()?;
        self.context.settings.area(&area)?;
        Some((name_end, TextRef::Area(area)))
    }

    /// `#N` of the home repository.
    fn bare_number(&self, at: usize) -> Option<(usize, TextRef)> {
        let home = self.context.home?;
        let opens = self
            .before(at)
            .is_none_or(|c| c.is_whitespace() || "([{".contains(c));
        if self.char_at(at) != Some('#') || !opens {
            return None;
        }
        let (end, number) = self.number(at + 1)?;
        Some((
            end,
            TextRef::Numbered {
                repo: home.clone(),
                number,
            },
        ))
    }

    /// A commit hash of the home repository, lower-cased.
    fn commit(&self, at: usize) -> Option<(usize, TextRef)> {
        let home = self.context.home?;
        if self.before(at).is_some_and(is_word_char) {
            return None;
        }
        let end = self.run(at, |c| c.is_ascii_hexdigit());
        let sha = self.slice(at, end);
        let shaped = (7..=40).contains(&sha.len())
            && sha.contains(|c: char| c.is_ascii_digit())
            && sha.contains(|c: char| c.is_ascii_alphabetic());
        if !shaped || self.char_at(end).is_some_and(is_word_char) {
            return None;
        }
        Some((
            end,
            TextRef::Commit {
                repo: home.clone(),
                sha: sha.to_ascii_lowercase(),
            },
        ))
    }

    /// A positive number of ASCII digits starting at `from`, not followed by a letter, a digit or
    /// `_`, and the position after it.
    fn number(&self, from: usize) -> Option<(usize, u64)> {
        let end = self.run(from, |c| c.is_ascii_digit());
        if self.char_at(end).is_some_and(is_word_char) {
            return None;
        }
        let number = self.slice(from, end).parse::<u64>().ok()?;
        (number > 0).then_some((end, number))
    }

    /// Adds the plain text between `from` and `to` as a piece, when there is any.
    fn push_plain(&self, pieces: &mut Vec<TextPiece>, from: usize, to: usize) {
        if from < to {
            pieces.push(TextPiece {
                text: self.slice(from, to).to_owned(),
                reference: None,
            });
        }
    }

    /// The end of the run of characters from `from` that satisfy `belongs`.
    fn run(&self, from: usize, belongs: impl Fn(char) -> bool) -> usize {
        let rest = self.text.get(from..).unwrap_or_default();
        from + rest.find(|c: char| !belongs(c)).unwrap_or(rest.len())
    }

    /// The character starting at `at`.
    fn char_at(&self, at: usize) -> Option<char> {
        self.text.get(at..)?.chars().next()
    }

    /// The character right before `at`.
    fn before(&self, at: usize) -> Option<char> {
        self.text.get(..at)?.chars().next_back()
    }

    /// The text between two character boundaries; empty if either is not one.
    fn slice(&self, from: usize, to: usize) -> &str {
        self.text.get(from..to).unwrap_or_default()
    }
}

/// The length of `token` once its trailing punctuation is given back: one character at a time
/// from the end, a closing bracket staying while the token holds more opening than closing
/// brackets of its kind before it.
fn trimmed_len(token: &str) -> usize {
    let mut open = [0_i64; 3];
    for c in token.chars() {
        if let Some(kind) = opens(c) {
            open[kind] += 1;
        } else if let Some(kind) = closes(c) {
            open[kind] -= 1;
        }
    }
    let mut end = token.len();
    while let Some(last) = token.get(..end).and_then(|kept| kept.chars().next_back()) {
        let given_back = match closes(last) {
            Some(kind) if open[kind] < 0 => {
                open[kind] += 1;
                true
            }
            Some(_) => false,
            None => TRAILING.contains(&last),
        };
        if !given_back {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

/// The kind of bracket `c` opens: round, square or curly.
fn opens(c: char) -> Option<usize> {
    match c {
        '(' => Some(0),
        '[' => Some(1),
        '{' => Some(2),
        _ => None,
    }
}

/// The kind of bracket `c` closes: round, square or curly.
fn closes(c: char) -> Option<usize> {
    match c {
        ')' => Some(0),
        ']' => Some(1),
        '}' => Some(2),
        _ => None,
    }
}

/// Whether `c` is part of a scheme's name.
fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "+-.".contains(c)
}

/// Whether `c` is part of a repository's path as a numbered reference writes it.
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._-/".contains(c)
}

/// Whether a reference starting right after `c` would be the tail of a longer name or address.
fn continues_a_name(c: char) -> bool {
    c.is_alphanumeric() || "._-/:".contains(c)
}

/// Whether `c` is a letter, a digit or `_`, which a reference never touches at its ends.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::settings;

    fn project() -> ProjectSettings {
        settings(
            r#"{"repos":[
                {"id":"api","remote":"https://github.com/acme/widgets.git",
                 "areas":[{"name":"billing","paths":["billing/**"]},
                          {"name":"search","paths":["search/**"]}]},
                {"id":"web","remote":"git@github.com:acme/site.git",
                 "areas":[{"name":"pages","paths":["pages/**"]}]}]}"#,
        )
    }

    fn repo(id: &str) -> RepoId {
        RepoId::parse(id).unwrap()
    }

    fn ticket(text: &str) -> TextRef {
        TextRef::Ticket(ExternalRef::parse(text).unwrap())
    }

    fn numbered(id: &str, number: u64) -> TextRef {
        TextRef::Numbered {
            repo: repo(id),
            number,
        }
    }

    fn area(text: &str) -> TextRef {
        TextRef::Area(QualifiedArea::parse(text).unwrap())
    }

    fn commit(id: &str, sha: &str) -> TextRef {
        TextRef::Commit {
            repo: repo(id),
            sha: sha.to_owned(),
        }
    }

    fn url(text: &str) -> TextRef {
        TextRef::Url(text.to_owned())
    }

    /// The pieces of `text` scanned in the test project, with `home` as its home repository,
    /// after checking what every scan holds to: the pieces join back to the text, none is empty,
    /// and no two plain pieces are adjacent.
    fn pieces(text: &str, home: Option<&str>) -> Vec<TextPiece> {
        let schemes = RefSchemeRegistry::standard();
        let settings = project();
        let home = home.map(repo);
        let context = RefContext {
            schemes: &schemes,
            settings: &settings,
            home: home.as_ref(),
        };
        let pieces = scan(text, &context);
        let joined: String = pieces.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(joined, text, "the pieces of {text:?} join back to it");
        assert!(
            pieces.iter().all(|piece| !piece.text.is_empty()),
            "no piece of {text:?} is empty: {pieces:?}"
        );
        assert!(
            pieces
                .windows(2)
                .all(|pair| pair[0].reference.is_some() || pair[1].reference.is_some()),
            "no two plain pieces of {text:?} are adjacent: {pieces:?}"
        );
        pieces
    }

    /// The references of `text`, each with its piece's text.
    fn refs(text: &str, home: Option<&str>) -> Vec<(String, TextRef)> {
        pieces(text, home)
            .into_iter()
            .filter_map(|piece| piece.reference.map(|reference| (piece.text, reference)))
            .collect()
    }

    fn one(text: &str, reference: TextRef) -> Vec<(String, TextRef)> {
        vec![(text.to_owned(), reference)]
    }

    #[test]
    fn each_kind_is_recognised_alone_as_its_exact_text() {
        let cases: Vec<(&str, &str, TextRef)> = vec![
            (
                "see https://example.com/a?b=1#c now",
                "https://example.com/a?b=1#c",
                url("https://example.com/a?b=1#c"),
            ),
            (
                "HTTP://Example.com/x",
                "HTTP://Example.com/x",
                url("HTTP://Example.com/x"),
            ),
            (
                "fixes gh:acme/widgets#42 today",
                "gh:acme/widgets#42",
                ticket("gh:acme/widgets#42"),
            ),
            (
                "merged acme/widgets#7 into main",
                "acme/widgets#7",
                numbered("api", 7),
            ),
            ("acme/site#3", "acme/site#3", numbered("web", 3)),
            (
                "touches api:billing too",
                "api:billing",
                area("api:billing"),
            ),
            ("web:pages", "web:pages", area("web:pages")),
            ("see #12 first", "#12", numbered("api", 12)),
            (
                "reverted a1b2c3d yesterday",
                "a1b2c3d",
                commit("api", "a1b2c3d"),
            ),
        ];
        for (text, piece, reference) in cases {
            assert_eq!(refs(text, Some("api")), one(piece, reference), "{text:?}");
        }
    }

    #[test]
    fn the_text_around_a_reference_is_kept_as_plain_pieces() {
        assert_eq!(
            pieces("see #12 first", Some("api")),
            vec![
                TextPiece {
                    text: "see ".to_owned(),
                    reference: None,
                },
                TextPiece {
                    text: "#12".to_owned(),
                    reference: Some(numbered("api", 12)),
                },
                TextPiece {
                    text: " first".to_owned(),
                    reference: None,
                },
            ]
        );
    }

    #[test]
    fn an_address_keeps_what_it_holds_whole() {
        assert_eq!(
            refs("https://example.com/x#42 gh:acme/widgets#1", Some("api")),
            vec![
                (
                    "https://example.com/x#42".to_owned(),
                    url("https://example.com/x#42")
                ),
                ("gh:acme/widgets#1".to_owned(), ticket("gh:acme/widgets#1")),
            ]
        );
        let inside = "https://example.com/?r=gh:acme/widgets#42&s=api:billing";
        assert_eq!(refs(inside, Some("api")), one(inside, url(inside)));
    }

    #[test]
    fn trailing_punctuation_is_given_back_to_the_text() {
        for (text, address) in [
            ("at https://example.com/a.", "https://example.com/a"),
            ("at https://example.com/a, then", "https://example.com/a"),
            ("(at https://example.com/a)", "https://example.com/a"),
            ("\"https://example.com/a\"", "https://example.com/a"),
            ("'https://example.com/a'!?;:", "https://example.com/a"),
            ("https://example.com/a]}", "https://example.com/a"),
            (
                "(see https://example.com/a_(b))",
                "https://example.com/a_(b)",
            ),
            ("https://example.com/[x].", "https://example.com/[x]"),
        ] {
            assert_eq!(refs(text, None), one(address, url(address)), "{text:?}");
        }
    }

    #[test]
    fn an_address_needs_something_after_its_scheme() {
        for text in ["https://", "https:// x", "http://.", "(https://)", "https:"] {
            assert_eq!(refs(text, Some("api")), vec![], "{text:?}");
        }
    }

    #[test]
    fn a_ticket_is_one_reference_and_never_a_numbered_one() {
        assert_eq!(
            refs("gh:acme/widgets#42", Some("api")),
            one("gh:acme/widgets#42", ticket("gh:acme/widgets#42"))
        );
        assert_eq!(
            refs("(gh:Acme/Widgets#42).", None),
            one("gh:Acme/Widgets#42", ticket("gh:acme/widgets#42"))
        );
    }

    #[test]
    fn a_scheme_the_registry_does_not_know_is_no_ticket() {
        for text in [
            "jira:ACME-12",
            "note: later",
            "gh:acme",
            "gh:",
            "xgh:acme/widgets#1",
        ] {
            assert_eq!(refs(text, None), vec![], "{text:?}");
        }
    }

    #[test]
    fn a_numbered_reference_names_a_repository_of_the_project() {
        assert_eq!(
            refs("acme/widgets#7", None),
            one("acme/widgets#7", numbered("api", 7))
        );
        assert_eq!(
            refs("ACME/Widgets#7.", None),
            one("ACME/Widgets#7", numbered("api", 7))
        );
        for text in [
            "other/repo#7",
            "acme/widgets#0",
            "acme/widgets#",
            "acme/widgets#7a",
            "example.com/acme/widgets#7",
            "x/acme/widgets#7",
            "gh:acme/widgets#x",
            "a.acme/widgets#7",
            "acme/widgets#99999999999999999999999",
        ] {
            assert_eq!(refs(text, None), vec![], "{text:?}");
        }
    }

    #[test]
    fn an_area_is_one_the_settings_declare() {
        assert_eq!(
            refs("on api:billing.", None),
            one("api:billing", area("api:billing"))
        );
        for text in [
            "api:nope",
            "https:",
            "web:billing",
            "x-api:billing",
            "a.api:billing",
            "api:billingX",
            "API:billing",
        ] {
            assert_eq!(refs(text, None), vec![], "{text:?}");
        }
    }

    #[test]
    fn a_bare_number_needs_a_home_and_a_boundary() {
        assert_eq!(refs("#12", Some("web")), one("#12", numbered("web", 12)));
        assert_eq!(refs("(#12)", Some("api")), one("#12", numbered("api", 12)));
        assert_eq!(refs("[#12]", Some("api")), one("#12", numbered("api", 12)));
        assert_eq!(refs("{#12}", Some("api")), one("#12", numbered("api", 12)));
        assert_eq!(
            refs("x\n#12.", Some("api")),
            one("#12", numbered("api", 12))
        );
        assert_eq!(refs("#12", None), vec![]);
        for text in ["abc#12", "#12a", "#12_", "#", "# 12", "#0", "a.#12"] {
            assert_eq!(refs(text, Some("api")), vec![], "{text:?}");
        }
    }

    #[test]
    fn a_commit_is_a_hex_run_of_seven_to_forty_with_a_digit_and_a_letter() {
        assert_eq!(
            refs("a1b2c3d", Some("api")),
            one("a1b2c3d", commit("api", "a1b2c3d"))
        );
        assert_eq!(
            refs("A1B2C3D4E5", Some("web")),
            one("A1B2C3D4E5", commit("web", "a1b2c3d4e5"))
        );
        let forty = "a1".repeat(20);
        assert_eq!(
            refs(&forty, Some("api")),
            one(&forty, commit("api", &forty))
        );
        assert_eq!(refs("a1b2c3d", None), vec![]);
        let forty_one = format!("{forty}b");
        for text in [
            "deadbeef",
            "1234567",
            "a1b2c3",
            forty_one.as_str(),
            "a1b2c3dz",
            "za1b2c3d",
            "_a1b2c3d",
            "a1b2c3d_",
            "éa1b2c3d",
        ] {
            assert_eq!(refs(text, Some("api")), vec![], "{text:?}");
        }
    }

    #[test]
    fn where_two_start_at_one_place_the_earlier_kind_wins() {
        // A deployment that registers a scheme named like a repository reads `api:billing` as a
        // ticket of that scheme rather than as the area.
        #[derive(Debug)]
        struct Anything;
        impl crate::ref_scheme::RefScheme for Anything {
            fn canonical(&self, value: &str) -> Result<String, crate::ref_scheme::RefRefused> {
                Ok(value.to_owned())
            }
            fn link(&self, _value: &str) -> Option<String> {
                None
            }
            fn repository(&self, _value: &str) -> Option<crate::repo::RemoteUrl> {
                None
            }
        }
        let schemes = RefSchemeRegistry::empty().with("api", Box::new(Anything));
        let settings = project();
        let context = RefContext {
            schemes: &schemes,
            settings: &settings,
            home: None,
        };
        assert_eq!(
            scan("api:billing", &context),
            vec![TextPiece {
                text: "api:billing".to_owned(),
                reference: Some(ticket("api:billing")),
            }]
        );
    }

    #[test]
    fn every_text_joins_back_whatever_it_holds() {
        for text in [
            "",
            " ",
            "é",
            "日本",
            "🦀",
            "é #12 日本 a1b2c3d 🦀 api:billing https://example.com/é🦀 acme/site#2",
            "https://日本.example.com/🦀)",
            "#12é a1b2c3dé éapi:billing 🦀#12",
            "gh:🦀 acme/🦀#1 api:🦀",
            "((((https://example.com/(((",
            "\u{feff}#1\u{2028}#2\t#3\r\n",
        ] {
            let _ = pieces(text, Some("api"));
            let _ = pieces(text, None);
        }
        let all: String = (0u32..0x0800)
            .filter_map(char::from_u32)
            .flat_map(|c| [c, '#', '1', ' ', c, 'a', '1', 'b', '2', 'c', '3', 'd', c])
            .collect();
        let _ = pieces(&all, Some("api"));
    }

    #[test]
    fn adjacent_plain_text_is_one_piece() {
        assert_eq!(
            pieces("see abc#12 and #12a or deadbeef", Some("api")),
            vec![TextPiece {
                text: "see abc#12 and #12a or deadbeef".to_owned(),
                reference: None,
            }]
        );
        assert_eq!(pieces("", None), vec![]);
    }

    #[test]
    fn references_follow_one_another() {
        assert_eq!(
            refs("#1 #2,api:search;a1b2c3d", Some("api")),
            vec![
                ("#1".to_owned(), numbered("api", 1)),
                ("#2".to_owned(), numbered("api", 2)),
                ("api:search".to_owned(), area("api:search")),
                ("a1b2c3d".to_owned(), commit("api", "a1b2c3d")),
            ]
        );
    }
}
