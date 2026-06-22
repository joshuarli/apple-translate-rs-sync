use apple_translate_rs_sync::{
    LanguageTranslator, TranslationRequest, check_language_availability, detect_language,
};

const PAIR: (&str, &str) = ("zh-Hans", "en");
const TEXT: &str = "你好";
const TEXTS: &[&str] = &["你好", "世界", "谢谢"];

struct State {
    passed: usize,
    failed: usize,
}

impl State {
    fn pass(&mut self, desc: &str) {
        self.passed += 1;
        eprintln!("  PASS  {desc}");
    }

    fn fail(&mut self, desc: &str, err: impl std::fmt::Display) {
        self.failed += 1;
        eprintln!("  FAIL  {desc}: {err}");
    }
}

fn main() {
    let mut s = State {
        passed: 0,
        failed: 0,
    };

    eprintln!(
        "=== Integration test harness ({} → {}) ===\n",
        PAIR.0, PAIR.1
    );

    // ── detect_language ──────────────────────────────────────────────────

    eprintln!("--- detect_language ---");
    match detect_language("Hello, world!") {
        Some(ref lang) if lang == "en" => s.pass("detect English"),
        other => s.fail(
            "detect English",
            format_args!("expected Some(\"en\"), got {other:?}"),
        ),
    }
    match detect_language("你好世界") {
        Some(ref lang) if lang.starts_with("zh") => s.pass("detect Chinese"),
        other => s.fail(
            "detect Chinese",
            format_args!("expected zh-*, got {other:?}"),
        ),
    }
    if detect_language("").is_none() {
        s.pass("detect empty");
    } else {
        s.fail("detect empty", "expected None");
    }

    // ── check_language_availability ──────────────────────────────────────

    eprintln!("\n--- check_language_availability ---");
    match check_language_availability(PAIR.0, PAIR.1) {
        Ok(()) => s.pass("availability sync"),
        Err(e) => s.fail("availability sync", e),
    }
    match check_language_availability("en", "zz") {
        Err(_) => s.pass("invalid pair errors"),
        Ok(()) => s.fail("invalid pair errors", "expected error for en→zz"),
    }

    // ── Construction ─────────────────────────────────────────────────────

    eprintln!("\n--- LanguageTranslator ---");
    let t = match LanguageTranslator::new(PAIR.0, PAIR.1) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("FATAL: cannot create translator: {e}");
            std::process::exit(1);
        }
    };
    if t.source() == PAIR.0 && t.target() == PAIR.1 {
        s.pass("source/target accessors");
    } else {
        s.fail("accessors", format_args!("expected {}/{}", PAIR.0, PAIR.1));
    }

    let t2 = t.clone();
    if t2.source() == t.source() && t2.target() == t.target() {
        s.pass("clone");
    } else {
        s.fail("clone", "mismatch");
    }

    // ── translate (sync) ─────────────────────────────────────────────────

    eprintln!("\n--- translate (sync) ---");
    match t.translate(TEXT) {
        Ok(r) => {
            if r.target_text.is_empty() {
                s.fail("translate single", "empty target_text");
            } else if r.source_text != TEXT {
                s.fail(
                    "translate single",
                    format_args!("source_text mismatch: {:?}", r.source_text),
                );
            } else if r.source_language != PAIR.0 {
                s.fail(
                    "translate single",
                    format_args!("source_language mismatch: {}", r.source_language),
                );
            } else if r.target_language != PAIR.1 {
                s.fail(
                    "translate single",
                    format_args!("target_language mismatch: {}", r.target_language),
                );
            } else {
                eprintln!("    '{}' → '{}'", TEXT, r.target_text);
                s.pass("translate single");
            }
        }
        Err(e) => s.fail("translate single", e),
    }

    // ── translate_batch (sync) ───────────────────────────────────────────

    eprintln!("\n--- translate_batch (sync) ---");
    let requests: Vec<TranslationRequest> =
        TEXTS.iter().map(|t| TranslationRequest::new(*t)).collect();
    let results = t.translate_batch(&requests);
    if results.len() != 3 {
        s.fail(
            "batch sync",
            format_args!("expected 3 results, got {}", results.len()),
        );
    } else {
        let mut ok = true;
        for (req, result) in requests.iter().zip(&results) {
            match result {
                Ok(r) => {
                    if r.source_text != req.source_text {
                        ok = false;
                    }
                    eprintln!("    '{}' → '{}'", req.source_text, r.target_text);
                }
                Err(e) => {
                    s.fail("batch sync item", e);
                    ok = false;
                }
            }
        }
        if ok {
            s.pass("batch sync");
        }
    }

    let reqs_with_ids = vec![
        TranslationRequest::with_client_id("你好", "a"),
        TranslationRequest::with_client_id("世界", "b"),
    ];
    let results = t.translate_batch(&reqs_with_ids);
    let mut ids_ok = true;
    for (req, result) in reqs_with_ids.iter().zip(&results) {
        match result {
            Ok(r) if r.client_identifier != req.client_identifier => ids_ok = false,
            Err(e) => {
                s.fail("batch client ID", e);
                ids_ok = false;
            }
            _ => {}
        }
    }
    if ids_ok {
        s.pass("batch with client IDs");
    } else {
        s.fail("batch client IDs", "mismatch");
    }

    if t.translate_batch(&[]).is_empty() {
        s.pass("batch empty");
    } else {
        s.fail("batch empty", "expected empty");
    }

    // ── prepare ──────────────────────────────────────────────────────────

    eprintln!("\n--- prepare ---");
    match t.prepare() {
        Ok(()) => s.pass("prepare sync"),
        Err(e) => s.fail("prepare sync", e),
    }
    // ── TranslationRequest ergonomics ────────────────────────────────────

    eprintln!("\n--- TranslationRequest ergonomics ---");
    let req: TranslationRequest = "hello".into();
    if req.source_text == "hello" && req.client_identifier.is_none() {
        s.pass("From<&str>");
    } else {
        s.fail("From<&str>", "unexpected value");
    }

    let req: TranslationRequest = String::from("hello").into();
    if req.source_text == "hello" {
        s.pass("From<String>");
    } else {
        s.fail("From<String>", "unexpected value");
    }

    let req = TranslationRequest::with_client_id("hello", "id-1");
    if req.source_text == "hello" && req.client_identifier.as_deref() == Some("id-1") {
        s.pass("with_client_id");
    } else {
        s.fail("with_client_id", "unexpected value");
    }

    // ── Summary ──────────────────────────────────────────────────────────

    eprintln!(
        "\n=== Results: {} passed, {} failed ===",
        s.passed, s.failed
    );
    if s.failed > 0 {
        std::process::exit(1);
    }
}
