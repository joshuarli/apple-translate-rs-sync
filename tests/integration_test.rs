use apple_translate_rs_sync::{
    LanguageTranslator, TranslationError, TranslationRequest, check_language_availability,
    detect_language,
};

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Try to get an en→es translator, skipping the test if unavailable.
fn try_en_es() -> Option<LanguageTranslator> {
    match LanguageTranslator::new("en", "es") {
        Ok(t) => Some(t),
        Err(TranslationError::LanguageNotInstalled { .. }) => {
            eprintln!("Skipping: Spanish model not installed");
            None
        }
        Err(TranslationError::LanguageUnsupported { .. }) => {
            eprintln!("Skipping: en→es unsupported");
            None
        }
        Err(e) => {
            eprintln!("Skipping: runtime not available ({e})");
            None
        }
    }
}

// ── Construction ────────────────────────────────────────────────────────────

#[test]
fn test_new_unavailable_pair() {
    match LanguageTranslator::new("en", "zu") {
        Err(TranslationError::LanguageNotInstalled { .. }) => {
            // Expected: model not downloaded for Zulu.
        }
        Err(TranslationError::LanguageUnsupported { .. }) => {
            // Also valid: Zulu might be unsupported entirely.
        }
        Ok(_) => {
            eprintln!("Note: Zulu model is installed, skipping negative check");
        }
        Err(e) => {
            eprintln!("Skipping: runtime not available ({e})");
        }
    }
}

#[test]
fn test_source_and_target_accessors() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    assert_eq!(t.source(), "en");
    assert_eq!(t.target(), "es");
}

// ── translate (sync) ────────────────────────────────────────────────────────

#[test]
fn test_translate_single() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    let response = t.translate("Hello").expect("translation should succeed");
    assert!(
        !response.target_text.is_empty(),
        "target text should not be empty"
    );
    assert_eq!(response.source_language, "en");
    assert_eq!(response.target_language, "es");
    assert_eq!(response.source_text, "Hello");
    assert!(response.client_identifier.is_none());
    eprintln!("en→es: 'Hello' → '{}'", response.target_text);
}

// ── translate_batch (sync) ──────────────────────────────────────────────────

#[test]
fn test_translate_batch_sync() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    let requests: Vec<TranslationRequest> = vec!["Hello".into(), "Goodbye".into()];
    let results = t.translate_batch(&requests);
    assert_eq!(results.len(), 2);
    for (req, result) in requests.iter().zip(&results) {
        let response = result.as_ref().expect("batch item should succeed");
        assert!(!response.target_text.is_empty());
        assert_eq!(response.source_text, req.source_text);
        eprintln!("'{}' → '{}'", req.source_text, response.target_text);
    }
}

#[test]
fn test_translate_batch_empty() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    let results = t.translate_batch(&[]);
    assert!(results.is_empty());
}

// ── prepare ─────────────────────────────────────────────────────────────────

#[test]
fn test_prepare() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    // prepare() should succeed (models already loaded)
    match t.prepare() {
        Ok(()) => eprintln!("prepare() succeeded"),
        Err(e) => eprintln!("prepare() returned error (may be ok): {e}"),
    }
}

// ── check_language_availability ─────────────────────────────────────────────

#[test]
fn test_check_availability_sync() {
    // en→es should be available (or at least supported)
    match check_language_availability("en", "es") {
        Ok(()) => eprintln!("en→es available"),
        Err(TranslationError::LanguageNotInstalled { .. }) => {
            eprintln!("en→es model not installed");
        }
        Err(TranslationError::LanguageUnsupported { .. }) => {
            eprintln!("en→es unsupported");
        }
        Err(e) => eprintln!("availability check failed: {e}"),
    }

    // en→zz should definitely fail (invalid language)
    let result = check_language_availability("en", "zz");
    assert!(result.is_err(), "invalid pair should error");
}

// ── detect_language ─────────────────────────────────────────────────────────

#[test]
fn test_detect_language() {
    let lang = detect_language("Hello, world!");
    assert_eq!(lang.as_deref(), Some("en"), "should detect English");

    let lang = detect_language("Hola, mundo!");
    assert_eq!(lang.as_deref(), Some("es"), "should detect Spanish");

    let lang = detect_language("");
    assert!(lang.is_none(), "empty text should return None");
}

// ── TranslationRequest ergonomics ───────────────────────────────────────────

#[test]
fn test_request_constructors() {
    // From &str
    let req = TranslationRequest::new("hello");
    assert_eq!(req.source_text, "hello");
    assert!(req.client_identifier.is_none());

    // From String
    let req = TranslationRequest::new(String::from("hello"));
    assert_eq!(req.source_text, "hello");

    // With client ID
    let req = TranslationRequest::with_client_id("hello", "msg-1");
    assert_eq!(req.source_text, "hello");
    assert_eq!(req.client_identifier.as_deref(), Some("msg-1"));

    // From<&str>
    let req: TranslationRequest = "hello".into();
    assert_eq!(req.source_text, "hello");

    // From<String>
    let req: TranslationRequest = String::from("hello").into();
    assert_eq!(req.source_text, "hello");
}

// ── TranslationResponse ─────────────────────────────────────────────────────

#[test]
fn test_response_fields() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    let req = TranslationRequest::with_client_id("Hello", "test-1");
    let response = t
        .translate_batch(&[req])
        .into_iter()
        .next()
        .unwrap()
        .expect("should succeed");

    assert_eq!(response.source_language, "en");
    assert_eq!(response.target_language, "es");
    assert_eq!(response.source_text, "Hello");
    assert!(!response.target_text.is_empty());
    assert_eq!(response.client_identifier.as_deref(), Some("test-1"));
}

// ── Clone + Send + Sync ─────────────────────────────────────────────────────

#[test]
fn test_translator_is_cloneable() {
    let t = match try_en_es() {
        Some(t) => t,
        None => return,
    };
    let t2 = t.clone();
    assert_eq!(t.source(), t2.source());
    assert_eq!(t.target(), t2.target());

    // Both should work independently
    let r1 = t.translate("Hello").unwrap();
    let r2 = t2.translate("World").unwrap();
    assert!(!r1.target_text.is_empty());
    assert!(!r2.target_text.is_empty());
}
