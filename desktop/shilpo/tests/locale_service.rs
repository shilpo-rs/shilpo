use std::sync::Arc;

use shilpo::locale::{
    LocaleEnvironment, LocaleResolver, LocaleService, TranslationArgs, TranslationError, Translator,
};

#[test]
fn explicit_locale_precedes_posix_environment_and_normalizes_to_bcp47() {
    let environment = LocaleEnvironment {
        lc_all: Some("bn_BD.UTF-8".into()),
        lc_messages: Some("en_GB.UTF-8".into()),
        lang: Some("en_IN.UTF-8".into()),
    };

    let resolved = LocaleResolver::default().resolve(Some("en_US.UTF-8"), &environment);

    assert_eq!(resolved.as_str(), "en-US");
}

#[test]
fn empty_environment_values_do_not_shadow_lower_precedence_locale() {
    let environment = LocaleEnvironment {
        lc_all: Some(String::new()),
        lc_messages: Some("bn_BD.UTF-8".into()),
        lang: Some("en_US.UTF-8".into()),
    };

    let resolved = LocaleResolver::default().resolve(None, &environment);

    assert_eq!(resolved.as_str(), "bn-BD");
}

#[test]
fn lc_all_precedes_lc_messages_and_lang_when_config_is_absent() {
    let environment = LocaleEnvironment {
        lc_all: Some("bn-BD".into()),
        lc_messages: Some("en-US".into()),
        lang: Some("en-US".into()),
    };

    assert_eq!(
        LocaleResolver::default()
            .resolve(None, &environment)
            .as_str(),
        "bn-BD"
    );
}

#[test]
fn fluent_translator_formats_arguments_and_plural_categories() {
    let locale = LocaleResolver::default().resolve(Some("en-US"), &LocaleEnvironment::default());
    let translator = Translator::for_locale(locale);
    let mut name = TranslationArgs::new();
    name.set_string("name", "Sayeed");
    let mut one = TranslationArgs::new();
    one.set_number("count", 1);
    let mut many = TranslationArgs::new();
    many.set_number("count", 3);

    assert_eq!(
        translator.translate("welcome-user", &name).unwrap(),
        "Welcome, Sayeed."
    );
    assert_eq!(
        translator.translate("window-count", &one).unwrap(),
        "One window"
    );
    assert_eq!(
        translator.translate("window-count", &many).unwrap(),
        "3 windows"
    );
}

#[test]
fn resolver_uses_language_fallback_then_en_us_for_invalid_or_unsupported_input() {
    let resolver = LocaleResolver::default();
    let environment = LocaleEnvironment::default();

    assert_eq!(
        resolver.resolve(Some("bn-IN"), &environment).as_str(),
        "bn-BD"
    );
    assert_eq!(
        resolver.resolve(Some("zh-CN"), &environment).as_str(),
        "en-US"
    );
    assert_eq!(
        resolver
            .resolve(Some("not a locale"), &environment)
            .as_str(),
        "en-US"
    );
}

#[test]
fn partial_catalog_falls_back_and_missing_or_malformed_keys_are_reported() {
    let locale = LocaleResolver::default().resolve(Some("bn-BD"), &LocaleEnvironment::default());
    let translator = Translator::for_locale(locale);
    let mut count = TranslationArgs::new();
    count.set_number("count", 2);

    assert_eq!(
        translator
            .translate("settings-title", &TranslationArgs::new())
            .unwrap(),
        "সেটিংস"
    );
    assert_eq!(
        translator.translate("window-count", &count).unwrap(),
        "2 windows"
    );
    assert!(matches!(
        translator.translate("missing-message", &TranslationArgs::new()),
        Err(TranslationError::MissingMessage(_))
    ));
    assert!(matches!(
        translator.translate("not a key", &TranslationArgs::new()),
        Err(TranslationError::InvalidKey(_))
    ));
    assert!(matches!(
        translator.translate("welcome-user", &TranslationArgs::new()),
        Err(TranslationError::Format { .. })
    ));
}

#[test]
fn translator_is_safe_for_concurrent_reads() {
    let locale = LocaleResolver::default().resolve(Some("en-US"), &LocaleEnvironment::default());
    let translator = Arc::new(Translator::for_locale(locale));
    let readers = (0..8)
        .map(|reader| {
            let translator = translator.clone();
            std::thread::spawn(move || {
                let mut args = TranslationArgs::new();
                args.set_string("name", format!("reader-{reader}"));
                translator.translate("welcome-user", &args).unwrap()
            })
        })
        .collect::<Vec<_>>();

    for (reader, task) in readers.into_iter().enumerate() {
        assert_eq!(task.join().unwrap(), format!("Welcome, reader-{reader}."));
    }
}

#[test]
fn locale_service_publishes_one_live_refresh_when_effective_locale_changes() {
    let service = LocaleService::new(None, LocaleEnvironment::default());
    let mut updates = service.subscribe();

    assert_eq!(updates.borrow().locale.as_str(), "en-US");
    assert_eq!(updates.borrow().revision, 0);
    assert!(!service.refresh(Some("en_US.UTF-8")));
    assert!(!updates.has_changed().unwrap());

    assert!(service.refresh(Some("bn_BD.UTF-8")));
    assert!(updates.has_changed().unwrap());
    let update = updates.borrow_and_update().clone();
    assert_eq!(update.locale.as_str(), "bn-BD");
    assert_eq!(update.revision, 1);
    assert_eq!(
        service
            .translate("settings-title", &TranslationArgs::new())
            .unwrap(),
        "সেটিংস"
    );
}
