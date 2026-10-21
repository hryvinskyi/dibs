//! `dibs login` and `dibs whoami`.

use std::io::{self, IsTerminal, Read};

use serde::Serialize;

use dibs_client::api::Member;
use dibs_client::credentials::{CredentialKey, TokenSource};
use dibs_client::server_url::ServerUrl;
use dibs_client::token::Token;
use dibs_client::{Client, ClientError};
use dibs_domain::project::ProjectSlug;
use dibs_domain::repo::RepoId;

use crate::cli::LoginArgs;
use crate::context::Context;
use crate::failure::{Failure, sentence};

#[derive(Serialize)]
struct SignedIn {
    server: ServerUrl,
    project: ProjectSlug,
    member: Member,
    stored_in: String,
}

/// Reads a token from standard input, checks it with the server, and stores it for the project it
/// belongs to: in the credentials file, or with `--keychain` in the OS credential store.
///
/// # Errors
/// A [`Failure`] when the token is missing, refused or of another project than the checkout's.
pub async fn login(args: LoginArgs, ctx: &Context) -> Result<(), Failure> {
    let checkout = ctx.checkout()?;
    let server = match (&args.server, &checkout) {
        (Some(text), _) => ServerUrl::parse(text)
            .map_err(|error| Failure::config("invalid_server", sentence(&error.to_string())))?,
        (None, Some(checkout)) => checkout.config().server.clone(),
        (None, None) => {
            return Err(Failure::config(
                "no_server",
                "No .dibs.toml was found here or above in this repository; name the server with dibs login --server <url>.",
            ));
        }
    };
    let token = read_token(&mut StandardInput::new(&|name| ctx.var(name)))?;
    let client = Client::new(server.clone(), token.clone(), ctx.client_options())?;
    let me = client.me().await.map_err(|error| match error {
        ClientError::Unauthorized { .. } => Failure::refused(
            "unauthorized",
            "The server refused this token; ask an admin of the project for a current one.",
        ),
        other => Failure::from(other),
    })?;
    if let Some(checkout) = checkout.filter(|checkout| checkout.config().server == server) {
        checkout.verify_project(&me).map_err(|mismatch| {
            Failure::config("project_mismatch", sentence(&mismatch.to_string()))
        })?;
    }
    let credentials = ctx.credentials()?;
    let key = CredentialKey {
        server: server.clone(),
        project: me.project.slug.clone(),
    };
    let stored_in = if args.keychain {
        credentials
            .save_in_store(&key, &token)
            .map_err(|error| Failure::config("credentials", sentence(&error.to_string())))?;
        "the operating system's credential store".to_owned()
    } else {
        credentials
            .save_in_file(&key, &token)
            .map_err(|error| Failure::config("credentials", sentence(&error.to_string())))?;
        credentials.path().display().to_string()
    };
    let answer = SignedIn {
        server,
        project: me.project.slug,
        member: me.member,
        stored_in,
    };
    ctx.printer.show(&answer, || {
        format!(
            "Signed in to the project {} on {} as {}; the token is stored in {}.",
            answer.project, answer.server, answer.member.handle, answer.stored_in
        )
    });
    Ok(())
}

/// What a person pasting a token at a terminal is asked.
const PROMPT: &str = "Paste your dibs token and press Enter (it is not shown): ";

/// What they are asked where the token cannot be read without showing it.
const VISIBLE_PROMPT: &str = "Paste your dibs token and press Enter: ";

/// The warning before a token is read visibly at a terminal.
const VISIBLE_WARNING: &str = "dibs cannot read the token without showing it here, so what you paste will be visible; to keep it off the screen, run dibs login < <file holding the token>.";

/// Where `dibs login` reads the token from.
trait TokenInput {
    /// Whether it is a terminal a person types or pastes at.
    fn is_terminal(&self) -> bool;

    /// One line typed at the terminal after `prompt`, never echoed.
    ///
    /// # Errors
    /// When it cannot be read without echo: no controlling terminal, or no console to switch the
    /// echo off on.
    fn hidden_line(&mut self, prompt: &str) -> io::Result<String>;

    /// One line typed at the terminal after `prompt`, echoed as typed.
    ///
    /// # Errors
    /// When the input cannot be read.
    fn visible_line(&mut self, prompt: &str) -> io::Result<String>;

    /// Everything that is piped in, up to its end.
    ///
    /// # Errors
    /// When the input cannot be read.
    fn everything(&mut self) -> io::Result<String>;

    /// Tells the person something before reading.
    fn warn(&mut self, text: &str);
}

/// The process's standard input; at a terminal, the line is read from the terminal with echo off,
/// unless `hides` is false.
struct StandardInput {
    /// Whether a line can be read hidden: false in a terminal known to have no console behind it,
    /// where reading the console would wait forever.
    hides: bool,
}

impl StandardInput {
    /// Standard input under this environment. On Windows, a terminal of the MSYS runtime (Git
    /// Bash's mintty, say) is a pipe that only looks like a terminal: no console stands behind it
    /// to read from without echo.
    fn new(var: &impl Fn(&str) -> Option<String>) -> Self {
        Self {
            hides: !(cfg!(windows) && var("MSYSTEM").is_some()),
        }
    }
}

impl TokenInput for StandardInput {
    fn is_terminal(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn hidden_line(&mut self, prompt: &str) -> io::Result<String> {
        if !self.hides {
            return Err(io::Error::other("no console to read from without echo"));
        }
        rpassword::prompt_password(prompt)
    }

    fn visible_line(&mut self, prompt: &str) -> io::Result<String> {
        eprint!("{prompt}");
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        Ok(line)
    }

    fn everything(&mut self) -> io::Result<String> {
        let mut text = String::new();
        io::stdin().lock().read_to_string(&mut text)?;
        Ok(text)
    }

    fn warn(&mut self, text: &str) {
        eprintln!("warning: {text}");
    }
}

/// The token on `input`: at a terminal one line, not echoed, or — where that cannot be done —
/// echoed after a warning that says so; all of the input otherwise.
fn read_token(input: &mut impl TokenInput) -> Result<Token, Failure> {
    let read = if input.is_terminal() {
        input.hidden_line(PROMPT).or_else(|_| {
            input.warn(VISIBLE_WARNING);
            input.visible_line(VISIBLE_PROMPT)
        })
    } else {
        input.everything()
    };
    let text = read.map_err(|_| {
        Failure::config(
            "no_token",
            "The token could not be read from standard input.",
        )
    })?;
    Token::parse(&text).map_err(|error| {
        Failure::config(
            "invalid_token",
            sentence(&format!("standard input holds no token: {error}")),
        )
    })
}

#[derive(Serialize)]
struct WhoAmI {
    member: Member,
    project: ProjectSlug,
    server: ServerUrl,
    repo: Option<RepoId>,
    token_from: String,
}

/// Shows who the token signs in as, in which project on which server, and where it came from.
///
/// # Errors
/// A [`Failure`] of the sign-in.
pub async fn whoami(ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let token_from = match &connection.source {
        TokenSource::Environment => "DIBS_TOKEN".to_owned(),
        TokenSource::File(path) => path.display().to_string(),
        TokenSource::Keychain => "the operating system's credential store".to_owned(),
    };
    let answer = WhoAmI {
        member: connection.me.member.clone(),
        project: connection.key.project.clone(),
        server: connection.key.server.clone(),
        repo: connection
            .checkout
            .as_ref()
            .and_then(|checkout| checkout.config().repo.clone()),
        token_from,
    };
    ctx.printer.show(&answer, || {
        let mut text = format!(
            "{} ({}) in the project {} on {}",
            answer.member.handle, answer.member.display_name, answer.project, answer.server
        );
        if let Some(repo) = &answer.repo {
            text.push_str(&format!(", repository {repo}"));
        }
        text.push_str(&format!("; token from {}.", answer.token_from));
        text
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "dibs_abcdefgh_0123456789abcdefghijklmnopqrstuv";

    /// An input that answers `text` and records how it was read; `hides` says whether it can
    /// read a line without echo.
    struct Scripted {
        terminal: bool,
        hides: bool,
        text: io::Result<String>,
        prompts: Vec<String>,
        visible: Vec<String>,
        warnings: Vec<String>,
        piped_reads: usize,
    }

    impl Scripted {
        fn new(terminal: bool, text: io::Result<String>) -> Self {
            Self {
                terminal,
                hides: true,
                text,
                prompts: Vec::new(),
                visible: Vec::new(),
                warnings: Vec::new(),
                piped_reads: 0,
            }
        }

        fn answer(&mut self) -> io::Result<String> {
            std::mem::replace(&mut self.text, Ok(String::new()))
        }
    }

    impl TokenInput for Scripted {
        fn is_terminal(&self) -> bool {
            self.terminal
        }

        fn hidden_line(&mut self, prompt: &str) -> io::Result<String> {
            self.prompts.push(prompt.to_owned());
            if self.hides {
                self.answer()
            } else {
                Err(io::Error::other("no controlling terminal"))
            }
        }

        fn visible_line(&mut self, prompt: &str) -> io::Result<String> {
            self.visible.push(prompt.to_owned());
            self.answer()
        }

        fn everything(&mut self) -> io::Result<String> {
            self.piped_reads += 1;
            self.answer()
        }

        fn warn(&mut self, text: &str) {
            self.warnings.push(text.to_owned());
        }
    }

    #[test]
    fn where_no_hidden_read_is_possible_the_line_is_read_visibly_after_one_warning() {
        let mut input = Scripted::new(true, Ok(format!("{TOKEN}\n")));
        input.hides = false;
        let token = read_token(&mut input).unwrap();
        assert_eq!(token.expose(), TOKEN);
        assert_eq!(input.prompts, [PROMPT], "the hidden read is tried first");
        assert_eq!(input.warnings, [VISIBLE_WARNING]);
        assert!(
            VISIBLE_WARNING.contains("will be visible"),
            "{VISIBLE_WARNING}"
        );
        assert!(
            VISIBLE_WARNING.contains("dibs login <"),
            "{VISIBLE_WARNING}"
        );
        assert_eq!(input.visible, [VISIBLE_PROMPT]);
        assert_eq!(input.piped_reads, 0);
    }

    #[test]
    fn a_windows_terminal_of_the_msys_runtime_reads_visibly_and_any_other_hides() {
        let msys = |name: &str| (name == "MSYSTEM").then(|| "MINGW64".to_owned());
        let plain = |_: &str| None;
        assert_eq!(StandardInput::new(&msys).hides, !cfg!(windows));
        assert!(StandardInput::new(&plain).hides);
    }

    #[test]
    fn at_a_terminal_the_token_is_read_as_one_hidden_line_after_the_prompt() {
        let mut input = Scripted::new(true, Ok(format!("{TOKEN}\n")));
        let token = read_token(&mut input).unwrap();
        assert_eq!(token.expose(), TOKEN);
        assert_eq!(input.prompts, [PROMPT]);
        assert_eq!(input.piped_reads, 0, "a terminal is never read to its end");
        assert!(input.warnings.is_empty() && input.visible.is_empty());
        assert!(PROMPT.contains("not shown"), "{PROMPT}");
    }

    #[test]
    fn piped_in_the_whole_input_is_read_and_nothing_is_prompted() {
        let mut input = Scripted::new(false, Ok(format!("\n  {TOKEN}  \n\n")));
        let token = read_token(&mut input).unwrap();
        assert_eq!(token.expose(), TOKEN);
        assert!(input.prompts.is_empty());
        assert_eq!(input.piped_reads, 1);
    }

    #[test]
    fn an_empty_or_unreadable_input_is_a_configuration_failure() {
        let empty = read_token(&mut Scripted::new(false, Ok("  \n".to_owned()))).unwrap_err();
        assert_eq!(empty.code, "invalid_token");
        assert_eq!(empty.kind, crate::failure::Kind::Config);
        let broken = || io::Error::new(io::ErrorKind::BrokenPipe, "closed");
        let piped = read_token(&mut Scripted::new(false, Err(broken()))).unwrap_err();
        assert_eq!(piped.code, "no_token");
        let mut neither = Scripted::new(true, Err(broken()));
        neither.hides = false;
        let unreadable = read_token(&mut neither).unwrap_err();
        assert_eq!(unreadable.code, "no_token");
        assert_eq!(unreadable.kind, crate::failure::Kind::Config);
        assert!(
            !unreadable.message.contains("closed"),
            "{}",
            unreadable.message
        );
    }
}
