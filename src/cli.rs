//! Help layout and parse errors of the command line in the current language.
//!
//! clap writes its section headings and error messages in English only, so
//! every command gets a help template with localized headings, and parse
//! errors are rendered here from the context clap records.

use std::error::Error as _;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Command, Error};

use crate::i18n::fl;
use crate::util::join_list;

/// A value parser error that is already in the current language; other
/// parser errors (from std or clap) are English and are not shown.
#[derive(Debug)]
pub struct LocalizedError(pub String);

impl std::fmt::Display for LocalizedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LocalizedError {}

/// Applies the localized help layout to `cmd` and all of its subcommands.
/// The `--help` argument is global and defined on the root command.
pub fn localize(cmd: Command) -> Command {
    let template = template(&cmd);
    cmd.help_template(template)
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .mut_subcommands(localize)
}

/// The space after a heading; a full-width colon needs none.
fn space_after(heading: &str) -> &'static str {
    if heading.ends_with(':') { " " } else { "" }
}

fn template(cmd: &Command) -> String {
    let styles = cmd.get_styles();
    let header = styles.get_header();
    let usage = styles.get_usage();
    let section = |title: String, tag: &str| format!("\n\n{header}{title}{header:#}\n{tag}");

    let usage_heading = fl!("cli-help-usage");
    let mut text = format!(
        "{{before-help}}{{about-with-newline}}\n{usage}{usage_heading}{usage:#}{}{{usage}}",
        space_after(&usage_heading)
    );
    if cmd.has_subcommands() {
        text.push_str(&section(fl!("cli-help-commands"), "{subcommands}"));
    }
    if cmd.get_positionals().next().is_some() {
        text.push_str(&section(fl!("cli-help-arguments"), "{positionals}"));
    }
    // Every command has at least the global options.
    text.push_str(&section(fl!("cli-help-options"), "{options}"));
    text.push_str("{after-help}");
    text
}

/// Prints a parse error in the current language and exits like clap does.
pub fn exit(err: Error) -> ! {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        err.exit();
    }
    let value = |kind| match err.get(kind) {
        Some(ContextValue::Strings(items)) => join_list(items),
        Some(value) => value.to_string(),
        None => String::new(),
    };
    let arg = value(ContextKind::InvalidArg);
    let message = match err.kind() {
        ErrorKind::UnknownArgument => fl!("cli-err-unknown-argument", arg = arg),
        ErrorKind::InvalidSubcommand => fl!(
            "cli-err-invalid-subcommand",
            name = value(ContextKind::InvalidSubcommand)
        ),
        ErrorKind::InvalidValue => {
            let mut message = fl!(
                "cli-err-invalid-value",
                arg = arg,
                value = value(ContextKind::InvalidValue)
            );
            let valid = value(ContextKind::ValidValue);
            if !valid.is_empty() {
                message.push_str("\n\n  ");
                message.push_str(&fl!("cli-err-possible-values", values = valid));
            }
            message
        }
        ErrorKind::ValueValidation => match err
            .source()
            .and_then(|source| source.downcast_ref::<LocalizedError>())
        {
            Some(reason) => fl!(
                "cli-err-validation",
                arg = arg,
                value = value(ContextKind::InvalidValue),
                reason = reason.to_string()
            ),
            None => fl!(
                "cli-err-invalid-value",
                arg = arg,
                value = value(ContextKind::InvalidValue)
            ),
        },
        ErrorKind::MissingRequiredArgument => fl!("cli-err-missing", args = arg),
        ErrorKind::ArgumentConflict => fl!(
            "cli-err-conflict",
            arg = arg,
            other = value(ContextKind::PriorArg)
        ),
        ErrorKind::MissingSubcommand => fl!(
            "cli-err-missing-subcommand",
            name = value(ContextKind::InvalidSubcommand)
        ),
        ErrorKind::NoEquals => fl!("cli-err-no-equals", arg = arg),
        ErrorKind::TooManyValues | ErrorKind::TooFewValues | ErrorKind::WrongNumberOfValues => {
            fl!("cli-err-wrong-values", arg = arg)
        }
        _ => {
            let rendered = err.render().to_string();
            let detail = rendered.lines().next().unwrap_or_default();
            fl!(
                "cli-err-generic",
                detail = detail.strip_prefix("error: ").unwrap_or(detail)
            )
        }
    };
    let mut text = fl!("error-line", message = message);
    let suggestions = [
        (ContextKind::SuggestedSubcommand, "command"),
        (ContextKind::SuggestedArg, "argument"),
        (ContextKind::SuggestedValue, "value"),
    ]
    .into_iter()
    .find_map(|(kind, name)| {
        let items = match err.get(kind)? {
            ContextValue::String(item) => vec![item.clone()],
            ContextValue::Strings(items) => items.clone(),
            ContextValue::StyledStr(item) => vec![item.to_string()],
            _ => return None,
        };
        Some((name, items))
    });
    match suggestions {
        Some((_, items)) if items.len() == 1 => {
            text.push_str("\n\n  ");
            text.push_str(&fl!("cli-err-suggestion", suggestion = items[0].clone()));
        }
        Some((kind, items)) if !items.is_empty() => {
            let quoted: Vec<String> = items
                .iter()
                .map(|item| fl!("cli-quoted", text = item.clone()))
                .collect();
            text.push_str("\n\n  ");
            text.push_str(&fl!(
                "cli-err-suggestions",
                kind = kind,
                suggestions = join_list(&quoted)
            ));
        }
        _ => {}
    }
    let usage = value(ContextKind::Usage);
    let usage = usage.strip_prefix("Usage:").unwrap_or(&usage).trim();
    if !usage.is_empty() {
        let heading = fl!("cli-help-usage");
        text.push_str(&format!("\n\n{heading}{}{usage}", space_after(&heading)));
    }
    text.push_str("\n\n");
    text.push_str(&fl!("cli-err-help-hint"));
    eprintln!("{text}");
    std::process::exit(2);
}
