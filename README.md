# apple-translate-rs-sync

Synchronous Rust bindings for Apple's on-device Translation framework.

The public Rust API is blocking by design. Apple's Translation framework is
async internally, but translation throughput saturates quickly on device and
batching is the useful optimization for this crate's target use cases.

## Requirements

- macOS with Apple's Translation framework available.
- Installed on-device translation assets for the source/target language pair.
- A Rust toolchain compatible with the crate's `rust-toolchain.toml`.

## Example

```rust
use apple_translate_rs_sync::{LanguageTranslator, TranslationRequest};

let translator = LanguageTranslator::new("zh-Hans", "en")?;

let single = translator.translate("你好")?;
println!("{}", single.target_text);

let requests = [
    TranslationRequest::new("你好"),
    TranslationRequest::new("世界"),
];
let batch = translator.translate_batch(&requests);
for response in batch.into_iter().flatten() {
    println!("{}", response.target_text);
}
# Ok::<(), apple_translate_rs_sync::TranslationError>(())
```

## Worker Fast Path

`translate_batch` automatically tries to use a persistent helper subprocess
hosting multiple `EMTTranslator` engines. The helper is compiled by the build
script and embedded into the Rust library, then extracted to a temp cache at
runtime when needed. If the worker or model assets are unavailable, the crate
falls back to the public `TranslationSession` batch API.

Set `APPLE_TRANSLATE_RS_SYNC_WORKER_BIN=/path/to/translation-worker` to override
the embedded helper for debugging.

### Engine count

Each worker subprocess runs one `EMTTranslator` engine by default (~88 MB RSS).
For batch translations of many short texts, increase the engine count to get
intra-worker parallelism (texts within a batch are distributed across engines):

```rust
apple_translate_rs_sync::set_worker_num_engines(4);
```

Or via environment variable: `APPLE_TRANSLATE_RS_SYNC_WORKER_NUM_ENGINES=4`

Values are clamped to 1–32. More engines only help when a single
`translate_batch` call contains many items; for single-text batches
(e.g. long-form translation) additional engines sit idle.

## Notes

This crate links Swift and Objective-C code in `build.rs` and is intended for
macOS targets.
