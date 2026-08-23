//! Product-owned locale resolution and translation.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use gpui::{App, Global};
use unic_langid::LanguageIdentifier;

const DEFAULT_LOCALE: &str = "en-US";
const SUPPORTED_LOCALES: &[&str] = &[DEFAULT_LOCALE, "bn-BD"];

/// POSIX locale variables captured at the application process seam.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocaleEnvironment {
    pub lc_all: Option<String>,
    pub lc_messages: Option<String>,
    pub lang: Option<String>,
}

impl LocaleEnvironment {
    pub fn from_process() -> Self {
        Self {
            lc_all: std::env::var("LC_ALL").ok(),
            lc_messages: std::env::var("LC_MESSAGES").ok(),
            lang: std::env::var("LANG").ok(),
        }
    }
}

/// A supported, normalized BCP 47 application locale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLocale(&'static str);

impl ResolvedLocale {
    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

/// Resolves declarative locale intent against a captured process environment.
#[derive(Clone, Debug)]
pub struct LocaleResolver {
    supported: &'static [&'static str],
    fallback: &'static str,
}

impl Default for LocaleResolver {
    fn default() -> Self {
        Self {
            supported: SUPPORTED_LOCALES,
            fallback: DEFAULT_LOCALE,
        }
    }
}

impl LocaleResolver {
    pub fn resolve(
        &self,
        configured: Option<&str>,
        environment: &LocaleEnvironment,
    ) -> ResolvedLocale {
        let requested = configured
            .filter(|value| !value.trim().is_empty())
            .or(environment
                .lc_all
                .as_deref()
                .filter(|value| !value.trim().is_empty()))
            .or(environment
                .lc_messages
                .as_deref()
                .filter(|value| !value.trim().is_empty()))
            .or(environment
                .lang
                .as_deref()
                .filter(|value| !value.trim().is_empty()));

        requested
            .and_then(|requested| self.resolve_supported(requested))
            .unwrap_or(ResolvedLocale(self.fallback))
    }

    fn resolve_supported(&self, raw: &str) -> Option<ResolvedLocale> {
        let posix_base = raw.split(['.', '@']).next().unwrap_or(raw);
        let normalized = posix_base.replace('_', "-");
        let requested = normalized.parse::<LanguageIdentifier>().ok()?;

        self.supported
            .iter()
            .copied()
            .find(|supported| {
                supported
                    .parse::<LanguageIdentifier>()
                    .is_ok_and(|locale| locale == requested)
            })
            .or_else(|| {
                self.supported.iter().copied().find(|supported| {
                    supported
                        .parse::<LanguageIdentifier>()
                        .is_ok_and(|locale| locale.language == requested.language)
                })
            })
            .map(ResolvedLocale)
    }
}

#[derive(Clone, Debug)]
enum TranslationValue {
    String(String),
    Number(i64),
}

/// Named values supplied to a Fluent message without exposing engine types to callers.
#[derive(Clone, Debug, Default)]
pub struct TranslationArgs(HashMap<String, TranslationValue>);

impl TranslationArgs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_string(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0
            .insert(name.into(), TranslationValue::String(value.into()));
    }

    pub fn set_number(&mut self, name: impl Into<String>, value: i64) {
        self.0.insert(name.into(), TranslationValue::Number(value));
    }

    fn as_fluent(&self) -> FluentArgs<'static> {
        let mut args = FluentArgs::new();
        for (name, value) in &self.0 {
            match value {
                TranslationValue::String(value) => args.set(name.clone(), value.clone()),
                TranslationValue::Number(value) => args.set(name.clone(), *value),
            }
        }
        args
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslationError {
    InvalidKey(String),
    MissingMessage(String),
    MissingValue(String),
    Format { key: String, errors: Vec<String> },
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey(key) => write!(formatter, "invalid Fluent message key '{key}'"),
            Self::MissingMessage(key) => write!(formatter, "missing Fluent message '{key}'"),
            Self::MissingValue(key) => write!(formatter, "Fluent message '{key}' has no value"),
            Self::Format { key, errors } => {
                write!(
                    formatter,
                    "failed to format Fluent message '{key}': {}",
                    errors.join("; ")
                )
            }
        }
    }
}

impl std::error::Error for TranslationError {}

/// Immutable, concurrent Fluent translator for one resolved application locale.
pub struct Translator {
    locale: ResolvedLocale,
    selected: FluentBundle<FluentResource>,
    fallback: Option<FluentBundle<FluentResource>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleUpdate {
    pub locale: ResolvedLocale,
    pub revision: u64,
}

struct LocaleServiceInner {
    resolver: LocaleResolver,
    environment: LocaleEnvironment,
    translator: arc_swap::ArcSwap<Translator>,
    revision: AtomicU64,
    updates: tokio::sync::watch::Sender<LocaleUpdate>,
    refresh_lock: Mutex<()>,
}

/// The single application-locale authority for one Shilpo process.
#[derive(Clone)]
pub struct LocaleService(Arc<LocaleServiceInner>);

impl LocaleService {
    pub fn new(configured: Option<&str>, environment: LocaleEnvironment) -> Self {
        let resolver = LocaleResolver::default();
        let locale = resolver.resolve(configured, &environment);
        let translator = Arc::new(Translator::for_locale(locale.clone()));
        let (updates, _) = tokio::sync::watch::channel(LocaleUpdate {
            locale,
            revision: 0,
        });
        Self(Arc::new(LocaleServiceInner {
            resolver,
            environment,
            translator: arc_swap::ArcSwap::from(translator),
            revision: AtomicU64::new(0),
            updates,
            refresh_lock: Mutex::new(()),
        }))
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<LocaleUpdate> {
        self.0.updates.subscribe()
    }

    pub fn current_locale(&self) -> ResolvedLocale {
        self.0.translator.load().locale().clone()
    }

    pub fn translate(&self, key: &str, args: &TranslationArgs) -> Result<String, TranslationError> {
        self.0.translator.load().translate(key, args)
    }

    /// Applies new declarative locale intent and reports whether the effective locale changed.
    pub fn refresh(&self, configured: Option<&str>) -> bool {
        let _guard = self
            .0
            .refresh_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let locale = self.0.resolver.resolve(configured, &self.0.environment);
        if self.0.translator.load().locale() == &locale {
            return false;
        }

        self.0
            .translator
            .store(Arc::new(Translator::for_locale(locale.clone())));
        let revision = self.0.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.0
            .updates
            .send_replace(LocaleUpdate { locale, revision });
        true
    }
}

/// Adapter from Shilpo's resolved locale into the generic M3 component catalogue.
#[derive(Clone, Copy, Debug, Default)]
pub struct M3LocaleAdapter;

impl M3LocaleAdapter {
    pub fn apply(self, locale: &ResolvedLocale) {
        shilpo_m3e::set_locale(locale.as_str());
    }
}

/// GPUI access point for the process-owned [`LocaleService`].
pub struct ApplicationLocale {
    service: LocaleService,
}

impl Global for ApplicationLocale {}

impl ApplicationLocale {
    pub fn install(configured: Option<&str>, cx: &mut App) {
        if cx.has_global::<Self>() {
            Self::apply_config(configured, cx);
            return;
        }
        let service = LocaleService::new(configured, LocaleEnvironment::from_process());
        M3LocaleAdapter.apply(&service.current_locale());
        cx.set_global(Self { service });
    }

    /// Applies a committed config locale and refreshes every open window when it changes.
    pub fn apply_config(configured: Option<&str>, cx: &mut App) -> bool {
        if !cx.has_global::<Self>() {
            Self::install(configured, cx);
            return true;
        }
        let service = cx.global::<Self>().service.clone();
        if !service.refresh(configured) {
            return false;
        }
        M3LocaleAdapter.apply(&service.current_locale());
        cx.refresh_windows();
        true
    }

    pub fn translate(
        key: &str,
        args: &TranslationArgs,
        cx: &App,
    ) -> Result<String, TranslationError> {
        cx.global::<Self>().service.translate(key, args)
    }

    pub fn current_locale(cx: &App) -> ResolvedLocale {
        cx.global::<Self>().service.current_locale()
    }
}

impl Translator {
    pub fn for_locale(locale: ResolvedLocale) -> Self {
        let selected_source = match locale.as_str() {
            "bn-BD" => include_str!("../../locales/bn-BD/main.ftl"),
            _ => include_str!("../../locales/en-US/main.ftl"),
        };
        let selected = build_bundle(locale.as_str(), selected_source);
        let fallback = (locale.as_str() != DEFAULT_LOCALE)
            .then(|| build_bundle(DEFAULT_LOCALE, include_str!("../../locales/en-US/main.ftl")));
        Self {
            locale,
            selected,
            fallback,
        }
    }

    pub fn locale(&self) -> &ResolvedLocale {
        &self.locale
    }

    pub fn translate(&self, key: &str, args: &TranslationArgs) -> Result<String, TranslationError> {
        if !valid_message_key(key) {
            return Err(TranslationError::InvalidKey(key.to_owned()));
        }
        if self.selected.has_message(key) {
            return format_message(&self.selected, key, args);
        }
        if let Some(fallback) = &self.fallback
            && fallback.has_message(key)
        {
            return format_message(fallback, key, args);
        }
        Err(TranslationError::MissingMessage(key.to_owned()))
    }
}

fn build_bundle(locale: &str, source: &str) -> FluentBundle<FluentResource> {
    let language = locale
        .parse::<LanguageIdentifier>()
        .expect("supported locales are valid BCP 47 identifiers");
    let resource = FluentResource::try_new(source.to_owned()).unwrap_or_else(|(_, errors)| {
        panic!("embedded {locale} Fluent catalog is invalid: {errors:?}")
    });
    let mut bundle = FluentBundle::new_concurrent(vec![language]);
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("embedded {locale} Fluent catalog conflicts: {errors:?}"));
    bundle
}

fn format_message(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: &TranslationArgs,
) -> Result<String, TranslationError> {
    let message = bundle
        .get_message(key)
        .ok_or_else(|| TranslationError::MissingMessage(key.to_owned()))?;
    let value = message
        .value()
        .ok_or_else(|| TranslationError::MissingValue(key.to_owned()))?;
    let fluent_args = args.as_fluent();
    let mut errors = Vec::new();
    let rendered = bundle
        .format_pattern(value, Some(&fluent_args), &mut errors)
        .into_owned();
    if errors.is_empty() {
        Ok(rendered)
    } else {
        Err(TranslationError::Format {
            key: key.to_owned(),
            errors: errors.into_iter().map(|error| error.to_string()).collect(),
        })
    }
}

fn valid_message_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}
