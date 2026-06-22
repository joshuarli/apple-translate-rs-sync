#[swift_bridge::bridge]
pub mod ffi {
    extern "Swift" {
        fn mt_detect_language(text: String) -> Option<String>;

        fn mt_check_languages(source: String, target: String) -> Option<String>;

        fn mt_translate(source: String, target: String, text: String) -> Option<String>;

        fn mt_translate_batch(source: String, target: String, texts: Vec<String>) -> Vec<String>;

        fn mt_prepare_translation(source: String, target: String) -> Option<String>;
    }
}
