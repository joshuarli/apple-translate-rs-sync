use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const EMBEDDED_WORKER: &[u8] = include_bytes!(env!("APPLE_TRANSLATE_RS_SYNC_WORKER_BIN"));
const EMBEDDED_WORKER_ID: &str = env!("APPLE_TRANSLATE_RS_SYNC_WORKER_ID");

/// Default number of EMTTranslator engines per worker subprocess.
///
/// 1 is the sweet spot for minimal memory with no throughput regression:
/// each engine independently loads the ~88 MB neural model, and additional
/// engines only help when a single `translate_batch` call contains many
/// texts that can be distributed across engines. For single-item batches
/// (e.g. long-form text), only one engine does work regardless of count.
///
/// Bump to 2–4 via [`set_worker_num_engines`] for batch-short-text
/// workloads to get intra-worker parallelism (up to ~3× throughput).
pub const DEFAULT_WORKER_NUM_ENGINES: usize = 1;

static WORKER_NUM_ENGINES_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// Set the number of EMTTranslator engines per worker subprocess.
///
/// Must be called before the first translation for a language pair —
/// workers are spawned lazily and the value is read at spawn time.
/// Values outside 1..=32 are clamped.
///
/// If never called, checks the `APPLE_TRANSLATE_RS_SYNC_WORKER_NUM_ENGINES`
/// environment variable. Falls back to [`DEFAULT_WORKER_NUM_ENGINES`] (1).
pub fn set_worker_num_engines(n: usize) {
    WORKER_NUM_ENGINES_OVERRIDE.store(n, Ordering::Relaxed);
}

pub fn worker_num_engines() -> usize {
    let override_val = WORKER_NUM_ENGINES_OVERRIDE.load(Ordering::Relaxed);
    if override_val > 0 {
        return override_val;
    }
    if let Ok(env_val) = std::env::var("APPLE_TRANSLATE_RS_SYNC_WORKER_NUM_ENGINES")
        && let Ok(n) = env_val.parse::<usize>()
            && n > 0 {
                return n;
            }
    DEFAULT_WORKER_NUM_ENGINES
}

/// Normalize a BCP-47 language tag to the ICU locale format used by
/// AssetsV3 directory names (e.g. "zh-Hans" → "zh_CN", "en" → "en_US").
pub fn normalize_locale(tag: &str) -> String {
    match tag {
        "zh-Hans" => "zh_CN".into(),
        "zh-Hant" => "zh_TW".into(),
        "zh-HK" => "zh_HK".into(),
        "en" => "en_US".into(),
        "en-US" => "en_US".into(),
        "en-GB" => "en_GB".into(),
        "ja" => "ja_JP".into(),
        "ja-JP" => "ja_JP".into(),
        "ko" => "ko_KR".into(),
        "ko-KR" => "ko_KR".into(),
        "fr" => "fr_FR".into(),
        "fr-FR" => "fr_FR".into(),
        "de" => "de_DE".into(),
        "de-DE" => "de_DE".into(),
        "es" => "es_ES".into(),
        "es-ES" => "es_ES".into(),
        "pt" => "pt_BR".into(),
        "pt-BR" => "pt_BR".into(),
        "it" => "it_IT".into(),
        "it-IT" => "it_IT".into(),
        "ru" => "ru_RU".into(),
        "ru-RU" => "ru_RU".into(),
        "vi" => "vi_VN".into(),
        "vi-VN" => "vi_VN".into(),
        "th" => "th_TH".into(),
        "th-TH" => "th_TH".into(),
        "tr" => "tr_TR".into(),
        "tr-TR" => "tr_TR".into(),
        "ar" => "ar_AE".into(),
        "ar-AE" => "ar_AE".into(),
        "hi" => "hi_IN".into(),
        "hi-IN" => "hi_IN".into(),
        "id" => "id_ID".into(),
        "id-ID" => "id_ID".into(),
        "uk" => "uk_UA".into(),
        "uk-UA" => "uk_UA".into(),
        "pl" => "pl_PL".into(),
        "pl-PL" => "pl_PL".into(),
        "nl" => "nl_NL".into(),
        "nl-NL" => "nl_NL".into(),
        other if other.contains('_') => other.into(),
        other => format!("{}_{}", other, other.to_uppercase()),
    }
}

/// Find the translation worker binary.
pub fn find_worker_bin() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("APPLE_TRANSLATE_RS_SYNC_WORKER_BIN") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Some(path) = materialize_embedded_worker() {
        return Some(path);
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("translation-worker");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let generated = PathBuf::from("./generated/translation-worker");
    if generated.exists() {
        return Some(generated);
    }
    None
}

fn materialize_embedded_worker() -> Option<PathBuf> {
    let dir = std::env::temp_dir()
        .join("apple-translate-rs-sync")
        .join(EMBEDDED_WORKER_ID);
    let path = dir.join("translation-worker");

    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() == EMBEDDED_WORKER.len() as u64
    {
        return Some(path);
    }

    std::fs::create_dir_all(&dir).ok()?;
    let tmp = dir.join(format!("translation-worker.tmp.{}", std::process::id()));
    std::fs::write(&tmp, EMBEDDED_WORKER).ok()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp, perms).ok()?;
    }

    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

/// Find the AssetsV3 directory for a language pair.
pub fn find_assets_dir(src: &str, tgt: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let base = format!("{home}/Library/Translation/AssetsV3");
    let src_icu = normalize_locale(src);
    let tgt_icu = normalize_locale(tgt);

    for dir_name in [
        format!("{src_icu}-{tgt_icu}"),
        format!("{tgt_icu}-{src_icu}"),
    ] {
        let dir = PathBuf::from(&base).join(&dir_name).join("assets.json");
        if dir.exists() {
            return Some(PathBuf::from(&base).join(&dir_name));
        }
    }
    None
}
