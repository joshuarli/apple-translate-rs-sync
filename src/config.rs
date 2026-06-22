use std::path::PathBuf;

/// Number of EMTTranslator engines per worker subprocess.
/// 4 is the sweet spot on Apple Silicon: 3.0× scaling over single engine.
/// More engines cause memory contention (each loads the ~88 MB model).
pub const WORKER_NUM_ENGINES: usize = 4;

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
