//! Localization with Project Fluent, loaded through i18n-embed.
//!
//! Messages live in `i18n/<language>/singbox-board.ftl` and are embedded in
//! the binary. [`fl!`] checks message ids and arguments against the English
//! file at compile time; the tests below hold every other language to the
//! same ids and arguments.
//!
//! Clients pick their language from `--lang`, `SINGBOX_BOARD_LANG` or the
//! locale and send it with every request. The daemon answers each request in
//! the language of the client that sent it ([`scope`]) and writes its own log
//! in the language configured in daemon.toml ([`fl_log!`]).

use std::future::Future;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

use i18n_embed::LanguageLoader;
use i18n_embed::fluent::{FluentLanguageLoader, fluent_language_loader};
use i18n_embed::unic_langid::LanguageIdentifier;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhCn,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::ZhCn];

    /// Language tag as written in the message directories and sent to the daemon.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhCn => "zh-CN",
        }
    }

    /// Maps a language tag or POSIX locale (`zh-CN`, `zh_CN.UTF-8`, `en_US`)
    /// to a supported language. Every Chinese locale uses the zh-CN messages.
    pub fn parse(value: &str) -> Option<Lang> {
        let language = value
            .trim()
            .split(['_', '-', '.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match language.as_str() {
            "en" => Some(Lang::En),
            "zh" => Some(Lang::ZhCn),
            _ => None,
        }
    }

    fn from_index(index: u8) -> Lang {
        Lang::ALL
            .get(usize::from(index))
            .copied()
            .unwrap_or(Lang::En)
    }
}

static LOADERS: LazyLock<Vec<FluentLanguageLoader>> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();
    let ids: Vec<LanguageIdentifier> = Lang::ALL
        .iter()
        .map(|lang| lang.tag().parse().expect("valid language tag"))
        .collect();
    loader
        .load_languages(&Localizations, &ids)
        .expect("embedded messages load");
    // Unicode isolation marks around arguments would shift terminal columns.
    loader.set_use_isolating(false);
    ids.iter()
        .map(|id| loader.select_languages(std::slice::from_ref(id)))
        .collect()
});

static PROCESS: AtomicU8 = AtomicU8::new(0);

tokio::task_local! {
    static REQUEST: Lang;
}

/// Sets the language of this process.
pub fn set_language(lang: Lang) {
    PROCESS.store(lang as u8, Ordering::Relaxed);
}

/// The language of this process.
pub fn process_language() -> Lang {
    Lang::from_index(PROCESS.load(Ordering::Relaxed))
}

/// The language messages are produced in: the requesting client's inside
/// [`scope`], the process language elsewhere.
pub fn current() -> Lang {
    REQUEST
        .try_with(|lang| *lang)
        .unwrap_or_else(|_| process_language())
}

/// Runs `future` with messages produced in `lang`.
pub fn scope<F: Future>(lang: Lang, future: F) -> impl Future<Output = F::Output> {
    REQUEST.scope(lang, future)
}

/// Runs `f` with messages produced in `lang`; carries the language of a
/// request into `spawn_blocking`.
pub fn in_language<T>(lang: Lang, f: impl FnOnce() -> T) -> T {
    REQUEST.sync_scope(lang, f)
}

/// Runs `f` with messages produced in the process language, e.g. to build
/// text for the daemon's log while answering a request.
pub fn in_log_language<T>(f: impl FnOnce() -> T) -> T {
    in_language(process_language(), f)
}

pub fn loader() -> &'static FluentLanguageLoader {
    &LOADERS[current() as usize]
}

pub fn log_loader() -> &'static FluentLanguageLoader {
    &LOADERS[process_language() as usize]
}

/// The language the locale asks for, following gettext: the first supported
/// entry of `LANGUAGE`, then `LC_ALL`, `LC_MESSAGES` and `LANG`. `LANGUAGE`
/// is ignored while the locale is C or POSIX.
pub fn from_locale() -> Option<Lang> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let locale = var("LC_ALL")
        .or_else(|| var("LC_MESSAGES"))
        .or_else(|| var("LANG"));
    // Windows has no locale variables (unless set by hand or by a Unix-like
    // shell); the user's display language decides there.
    #[cfg(windows)]
    let locale = locale.or_else(crate::win::user_locale);
    let locale = locale?;
    let posix = locale == "C" || locale == "POSIX" || locale.starts_with("C.");
    if !posix
        && let Some(lang) = var("LANGUAGE").and_then(|list| list.split(':').find_map(Lang::parse))
    {
        return Some(lang);
    }
    Lang::parse(&locale)
}

/// Resolves a configured language: `auto` (or nothing) follows the locale.
/// Returns `Err` with the rejected value when it names no supported language.
pub fn resolve(setting: Option<&str>) -> Result<Lang, String> {
    match setting.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(from_locale().unwrap_or(Lang::En)),
        Some(value) if value.eq_ignore_ascii_case("auto") => Ok(from_locale().unwrap_or(Lang::En)),
        Some(value) => Lang::parse(value).ok_or_else(|| value.to_owned()),
    }
}

/// A message in the [`current`] language.
macro_rules! fl {
    ($($args:tt)*) => {
        i18n_embed_fl::fl!(crate::i18n::loader(), $($args)*)
    };
}

/// A message in the process language, for lines the daemon keeps in its log.
macro_rules! fl_log {
    ($($args:tt)*) => {
        i18n_embed_fl::fl!(crate::i18n::log_loader(), $($args)*)
    };
}

pub(crate) use {fl, fl_log};

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    /// Message id → argument names, read from the raw file so that a message
    /// the parser rejects still shows up.
    fn messages(lang: Lang) -> BTreeMap<String, BTreeSet<String>> {
        let path = format!("{}/singbox-board.ftl", lang.tag());
        let file = Localizations::get(&path).expect("message file exists");
        let text = std::str::from_utf8(&file.data).unwrap();
        let mut messages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut current: Option<String> = None;
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if !line.starts_with(' ') {
                let (id, _) = line.split_once('=').expect("`id = value`");
                let id = id.trim().to_owned();
                assert!(
                    messages.insert(id.clone(), BTreeSet::new()).is_none(),
                    "{path}: duplicate message {id}"
                );
                current = Some(id);
            }
            let args = messages.get_mut(current.as_ref().unwrap()).unwrap();
            // Variables are `$name` inside a placeable; a `$` in text is literal.
            let mut depth = 0usize;
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    '$' if depth > 0 => {
                        let mut name = String::new();
                        while let Some(&c) = chars.peek() {
                            if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                                break;
                            }
                            name.push(c);
                            chars.next();
                        }
                        args.insert(name);
                    }
                    _ => {}
                }
            }
        }
        messages
    }

    #[test]
    fn every_language_has_every_message_with_the_same_arguments() {
        let reference = messages(Lang::En);
        for lang in Lang::ALL {
            let translated = messages(lang);
            for (id, args) in &reference {
                let Some(found) = translated.get(id) else {
                    panic!("{}: missing message {id}", lang.tag());
                };
                assert_eq!(found, args, "{}: arguments of {id}", lang.tag());
            }
            for id in translated.keys() {
                assert!(
                    reference.contains_key(id),
                    "{}: unknown message {id}",
                    lang.tag()
                );
            }
            // Every message must also survive the Fluent parser.
            let parsed: BTreeSet<String> = LOADERS[lang as usize]
                .with_message_iter(&lang.tag().parse().unwrap(), |iter| {
                    iter.map(|message| message.id.name.to_owned()).collect()
                });
            let raw: BTreeSet<String> = translated.keys().cloned().collect();
            assert_eq!(
                parsed,
                raw,
                "{}: messages Fluent could not parse",
                lang.tag()
            );
        }
    }

    #[test]
    fn messages_avoid_decorative_punctuation() {
        // Separators such as the middle dot or the em dash are not used in
        // product copy; lists use commas, ranges and asides use words.
        const BANNED: [char; 6] = ['·', '•', '・', '—', '–', '‧'];
        for lang in Lang::ALL {
            let path = format!("{}/singbox-board.ftl", lang.tag());
            let file = Localizations::get(&path).unwrap();
            let text = std::str::from_utf8(&file.data).unwrap();
            for (number, line) in text.lines().enumerate() {
                if let Some(c) = line.chars().find(|c| BANNED.contains(c)) {
                    panic!("{path}:{}: {c:?} in {line:?}", number + 1);
                }
            }
        }
    }

    #[test]
    fn arguments_are_not_isolated() {
        let text = LOADERS[Lang::En as usize].get_args_concrete(
            "state-pid-uptime",
            [("pid", "42".into()), ("uptime", "00:05".into())]
                .into_iter()
                .collect(),
        );
        assert!(!text.contains(['\u{2068}', '\u{2069}']), "{text:?}");
        assert!(text.contains("42"));
    }

    #[test]
    fn locale_names() {
        assert_eq!(Lang::parse("zh_CN.UTF-8"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse("zh-Hans"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse("zh_TW"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::parse("C.UTF-8"), None);
        assert_eq!(Lang::parse("de_DE"), None);
        assert_eq!(resolve(Some("zh-CN")), Ok(Lang::ZhCn));
        assert_eq!(resolve(Some("fr")), Err("fr".to_owned()));
    }

    #[test]
    fn requests_use_their_own_language() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let process = process_language();
        let inside = runtime.block_on(scope(Lang::ZhCn, async { current() }));
        assert_eq!(inside, Lang::ZhCn);
        assert_eq!(current(), process);
    }
}
