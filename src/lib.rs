mod config;
mod ffi;
mod worker_pool;

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::{Arc, LazyLock, Mutex};

use std::cell::RefCell;

// Thread-local storage for the error kind and message set by Swift via
// `__mt_set_error`. Read by `take_last_error` after a synchronous FFI
// call returns an error.
thread_local! {
    static LAST_ERROR: RefCell<(i32, String)> = const { RefCell::new((ERR_SUCCESS, String::new())) };
}

fn take_last_error() -> (i32, String) {
    LAST_ERROR.with(|cell| {
        let mut err = cell.borrow_mut();
        let result = (err.0, std::mem::take(&mut err.1));
        // Reset to success so stale values don't leak into the next call.
        err.0 = ERR_SUCCESS;
        result
    })
}

/// Called by Swift before returning an error from a synchronous FFI function.
/// Stores the error kind and message in thread-local storage so the Rust
/// wrapper can construct the appropriate [`TranslationError`] variant.
///
/// # Safety
///
/// `message_ptr` must be a valid null-terminated UTF-8 C string, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mt_set_error(kind: i32, message_ptr: *const c_char) {
    let msg = if message_ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(message_ptr) }
            .to_string_lossy()
            .into_owned()
    };
    LAST_ERROR.with(|cell| {
        let mut err = cell.borrow_mut();
        err.0 = kind;
        err.1 = msg;
    });
}

// Error kind tags communicated from Swift via `__mt_set_error`. These must
// match the constants in `TranslationWrapper.swift`.
#[allow(dead_code)]
const ERR_SUCCESS: i32 = 0;
const ERR_LANG_NOT_INSTALLED: i32 = 1;
const ERR_LANG_UNSUPPORTED: i32 = 2;
#[allow(dead_code)]
const ERR_TRANSLATION_FAILED: i32 = 3;
const ERR_TIMED_OUT: i32 = 4;

/// Errors that can occur during translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationError {
    /// Model not downloaded for this language pair.
    LanguageNotInstalled { source: String, target: String },
    /// Language pair is unsupported by the framework.
    LanguageUnsupported { source: String, target: String },
    /// Framework reported an error during translation.
    TranslationFailed { reason: String },
    /// Operation exceeded the deadline.
    TimedOut {
        operation: &'static str,
        seconds: u64,
    },
}

impl std::fmt::Display for TranslationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslationError::LanguageNotInstalled { source, target } => {
                write!(f, "language model not installed for {source} → {target}")
            }
            TranslationError::LanguageUnsupported { source, target } => {
                write!(f, "language pair unsupported: {source} → {target}")
            }
            TranslationError::TranslationFailed { reason } => {
                write!(f, "translation failed: {reason}")
            }
            TranslationError::TimedOut { operation, seconds } => {
                write!(f, "{operation} timed out after {seconds}s")
            }
        }
    }
}

impl std::error::Error for TranslationError {}

fn timed_out(operation: &'static str, seconds: u64) -> TranslationError {
    TranslationError::TimedOut { operation, seconds }
}

fn translation_failed(reason: impl Into<String>) -> TranslationError {
    TranslationError::TranslationFailed {
        reason: reason.into(),
    }
}

fn source_target_error(
    kind: i32,
    source: &str,
    target: &str,
    fallback: String,
) -> TranslationError {
    match kind {
        ERR_LANG_NOT_INSTALLED => TranslationError::LanguageNotInstalled {
            source: source.to_owned(),
            target: target.to_owned(),
        },
        ERR_LANG_UNSUPPORTED => TranslationError::LanguageUnsupported {
            source: source.to_owned(),
            target: target.to_owned(),
        },
        ERR_TIMED_OUT => timed_out("check_language_availability", 15),
        _ => translation_failed(fallback),
    }
}

type SharedWorkerPool = Arc<Mutex<worker_pool::WorkerPool>>;

static WORKER_POOL_CACHE: LazyLock<Mutex<HashMap<String, SharedWorkerPool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DISABLED_WORKER_PAIRS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn pair_key(source: &str, target: &str) -> String {
    format!("{source}->{target}")
}

fn worker_pool_for(source: &str, target: &str) -> Option<SharedWorkerPool> {
    let pair_key = pair_key(source, target);
    if DISABLED_WORKER_PAIRS.lock().ok()?.contains(&pair_key) {
        return None;
    }

    let mut cache = WORKER_POOL_CACHE.lock().ok()?;
    if let Some(pool) = cache.get(&pair_key) {
        return Some(Arc::clone(pool));
    }

    let pool = Arc::new(Mutex::new(worker_pool::WorkerPool::try_create(
        source, target,
    )?));
    cache.insert(pair_key, Arc::clone(&pool));
    Some(pool)
}

fn discard_worker_pool(source: &str, target: &str, pool: &SharedWorkerPool) {
    let pair_key = pair_key(source, target);
    if let Ok(mut cache) = WORKER_POOL_CACHE.lock()
        && cache
            .get(&pair_key)
            .is_some_and(|cached| Arc::ptr_eq(cached, pool))
    {
        cache.remove(&pair_key);
    }
}

fn disable_worker_pair(source: &str, target: &str) {
    let pair_key = pair_key(source, target);
    if let Ok(mut disabled) = DISABLED_WORKER_PAIRS.lock() {
        disabled.insert(pair_key);
    }
}

/// A single translation request.
///
/// Mirrors [`TranslationSession.Request`](https://developer.apple.com/documentation/translation/translationsession/request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationRequest {
    /// The text to translate.
    pub source_text: String,
    /// An optional identifier echoed back in the response, useful for
    /// correlating results when translating many strings.
    pub client_identifier: Option<String>,
}

impl TranslationRequest {
    pub fn new(source_text: impl Into<String>) -> Self {
        Self {
            source_text: source_text.into(),
            client_identifier: None,
        }
    }

    pub fn with_client_id(
        source_text: impl Into<String>,
        client_identifier: impl Into<String>,
    ) -> Self {
        Self {
            source_text: source_text.into(),
            client_identifier: Some(client_identifier.into()),
        }
    }
}

impl From<&str> for TranslationRequest {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for TranslationRequest {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// The result of a single translation.
///
/// Mirrors [`TranslationSession.Response`](https://developer.apple.com/documentation/translation/translationsession/response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationResponse {
    /// The detected source language (BCP-47 code, e.g. `"zh-Hans"`).
    pub source_language: String,
    /// The target language (BCP-47 code, e.g. `"en"`).
    pub target_language: String,
    /// The original source text that was submitted.
    pub source_text: String,
    /// The translated text.
    pub target_text: String,
    /// The client identifier from the request, if any.
    pub client_identifier: Option<String>,
}

/// A translator for a specific language pair.
///
/// Mirrors [`TranslationSession`](https://developer.apple.com/documentation/translation/translationsession)
/// (specifically the `installedSource:target:` init variant for on-device-only
/// translation).
///
/// Created via [`LanguageTranslator::new`], which verifies that the requested
/// language pair is available. Once created, the translator can be used for
/// multiple [`translate`](Self::translate) and
/// [`translate_batch`](Self::translate_batch) calls.
///
/// The translator is cheap to clone — it only holds the source and target
/// language identifiers as owned strings. The underlying `TranslationSession`
/// is cached on the Swift side and reused across calls for the same language
/// pair.
///
/// # Thread safety
///
/// `LanguageTranslator` is `Send + Sync` — it contains only owned `String`
/// data. The FFI calls use `&self` (shared reference) and the Swift side
/// uses internal locks for session cache access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageTranslator {
    source: String,
    target: String,
}

/// Detect the dominant language of the given text.
///
/// Uses Apple's `NaturalLanguage` framework (`NLLanguageRecognizer`).
/// Returns a BCP-47 language code like `"en"`, `"es"`, `"fr"`,
/// or `None` if detection fails.
///
/// This call is synchronous and does not require the Swift Concurrency
/// runtime to be initialized.
pub fn detect_language(text: &str) -> Option<String> {
    ffi::ffi::mt_detect_language(text.to_owned())
}

/// Check whether the given language pair is available for on-device translation.
///
/// Mirrors `LanguageAvailability.status(from:to:)`.
///
/// Returns `Ok(())` if the pair is installed and ready, or
/// `Err(TranslationError::LanguageNotInstalled{...})` if the model needs to
/// be downloaded, or `Err(TranslationError::LanguageUnsupported{...})` if
/// the pair cannot be translated.
///
pub fn check_language_availability(source: &str, target: &str) -> Result<(), TranslationError> {
    if let Some(msg) = ffi::ffi::mt_check_languages(source.to_owned(), target.to_owned()) {
        let (kind, _detail) = take_last_error();
        return Err(source_target_error(kind, source, target, msg));
    }
    Ok(())
}

impl LanguageTranslator {
    /// Create a new translator for the given language pair.
    ///
    /// Mirrors `TranslationSession.init(installedSource:target:)`.
    ///
    /// Verifies that the on-device translation model is installed.
    /// Returns [`TranslationError::LanguageNotInstalled`] if the model hasn't
    /// been downloaded, or [`TranslationError::LanguageUnsupported`] if the
    /// pair is unsupported.
    ///
    /// Language identifiers should be valid BCP-47 codes
    /// (e.g., `"en"`, `"es"`, `"zh-Hans"`).
    ///
    pub fn new(source: &str, target: &str) -> Result<Self, TranslationError> {
        check_language_availability(source, target)?;
        Ok(Self {
            source: source.to_owned(),
            target: target.to_owned(),
        })
    }

    /// The source language code (e.g. `"zh-Hans"`).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The target language code (e.g. `"en"`).
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Translate a single string.
    ///
    /// Mirrors `TranslationSession.translate(_:)`.
    ///
    /// # Errors
    ///
    /// Returns [`TranslationError::TimedOut`] if the operation exceeds the
    /// 30-second deadline, or [`TranslationError::TranslationFailed`] on
    /// framework errors.
    pub fn translate(&self, text: &str) -> Result<TranslationResponse, TranslationError> {
        let target_text =
            ffi::ffi::mt_translate(self.source.clone(), self.target.clone(), text.to_owned())
                .ok_or_else(|| {
                    let (kind, _detail) = take_last_error();
                    match kind {
                        ERR_TIMED_OUT => timed_out("translate", 30),
                        _ => translation_failed(
                            "translation failed (check stderr for details from the Swift runtime)",
                        ),
                    }
                })?;

        Ok(TranslationResponse {
            source_language: self.source.clone(),
            target_language: self.target.clone(),
            source_text: text.to_owned(),
            target_text,
            client_identifier: None,
        })
    }

    /// Translate a batch of requests.
    ///
    /// Mirrors `TranslationSession.translations(from:)`.
    ///
    /// Uses the batch API which processes all requests in one actor call —
    /// dramatically more efficient than N individual [`translate`](Self::translate)
    /// calls.
    ///
    /// Returns one `Result` per input request, in the same order.
    /// An empty slice returns an empty vec.
    pub fn translate_batch(
        &self,
        requests: &[TranslationRequest],
    ) -> Vec<Result<TranslationResponse, TranslationError>> {
        if requests.is_empty() {
            return Vec::new();
        }

        let texts: Vec<String> = requests.iter().map(|r| r.source_text.clone()).collect();

        if let Some(pool) = worker_pool_for(&self.source, &self.target) {
            if let Ok(mut guard) = pool.lock() {
                match guard.translate_batch(&texts) {
                    Ok(worker_results) if is_usable_batch(&worker_results) => {
                        return self.responses_from_targets(requests, worker_results);
                    }
                    Ok(_) => {
                        if texts.iter().any(|text| !text.is_empty()) {
                            eprintln!(
                                "apple-translate-rs-sync: worker returned no usable translations; disabling worker for {}->{} and falling back",
                                self.source, self.target
                            );
                            guard.terminate();
                            drop(guard);
                            discard_worker_pool(&self.source, &self.target, &pool);
                            disable_worker_pair(&self.source, &self.target);
                        }
                    }
                    Err(err) => {
                        eprintln!(
                            "apple-translate-rs-sync: worker failed: {err}; disabling worker for {}->{} and falling back",
                            self.source, self.target
                        );
                        guard.terminate();
                        drop(guard);
                        discard_worker_pool(&self.source, &self.target, &pool);
                        disable_worker_pair(&self.source, &self.target);
                    }
                }
            }
        }

        let results: Vec<String> =
            ffi::ffi::mt_translate_batch(self.source.clone(), self.target.clone(), texts);

        if results.is_empty() {
            let (kind, _detail) = take_last_error();
            let err = match kind {
                ERR_TIMED_OUT => timed_out("translate_batch", 60),
                _ => translation_failed("batch translation failed"),
            };
            return requests.iter().map(|_| Err(err.clone())).collect();
        }

        self.responses_from_targets(requests, results)
    }

    fn responses_from_targets(
        &self,
        requests: &[TranslationRequest],
        targets: Vec<String>,
    ) -> Vec<Result<TranslationResponse, TranslationError>> {
        requests
            .iter()
            .zip(targets)
            .map(|(req, target_text)| {
                if target_text.is_empty() {
                    Err(translation_failed("translation failed"))
                } else {
                    Ok(TranslationResponse {
                        source_language: self.source.clone(),
                        target_language: self.target.clone(),
                        source_text: req.source_text.clone(),
                        target_text,
                        client_identifier: req.client_identifier.clone(),
                    })
                }
            })
            .collect()
    }

    /// Pre-warm the translation engine.
    ///
    /// Mirrors `TranslationSession.prepareTranslation()`.
    ///
    /// Forces model download / engine warmup. Call this before a critical
    /// translation path to avoid first-use latency.
    ///
    /// Returns `Ok(())` if preparation completed, or an error on failure
    /// or timeout (60s deadline).
    pub fn prepare(&self) -> Result<(), TranslationError> {
        if let Some(msg) =
            ffi::ffi::mt_prepare_translation(self.source.clone(), self.target.clone())
        {
            let (kind, _detail) = take_last_error();
            return Err(match kind {
                ERR_TIMED_OUT => timed_out("prepare", 60),
                _ => translation_failed(msg),
            });
        }
        Ok(())
    }
}

fn is_usable_batch(results: &[String]) -> bool {
    !results.is_empty() && results.iter().any(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_send_sync() {
        fn assert_impl<T: Send + Sync>() {}
        assert_impl::<LanguageTranslator>();
        assert_impl::<TranslationError>();
        assert_impl::<TranslationRequest>();
        assert_impl::<TranslationResponse>();
    }
}
