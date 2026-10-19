//! `dibs login` and `dibs whoami`.

use std::io::{IsTerminal, Read};

use serde::Serialize;

use dibs_client::api::Member;
use dibs_client::credentials::{CredentialKey, TokenSource};
use dibs_client::server_url::ServerUrl;
use dibs_client::token::Token;
use dibs_client::{Client, ClientError, ClientOptions};
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
    let token = read_token()?;
    let client = Client::new(server.clone(), token.clone(), ClientOptions::default())?;
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

/// The token on standard input: one line at a terminal, all of it otherwise.
fn read_token() -> Result<Token, Failure> {
    let stdin = std::io::stdin();
    let mut text = String::new();
    let read = if stdin.is_terminal() {
        eprint!("Paste your dibs token and press Enter: ");
        stdin.read_line(&mut text).map(|_| ())
    } else {
        stdin.lock().read_to_string(&mut text).map(|_| ())
    };
    read.map_err(|_| {
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
