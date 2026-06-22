`apple-translate-rs-sync` exposes Apple Translation framework operations to Rust through a synchronous API. The Apple framework is async internally, but the Rust public surface is blocking because on-device translation throughput saturates quickly and batching is the main useful optimization.

## API map: Swift → Rust

| Swift | Rust |
|-------|------|
| `TranslationSession.init(installedSource:target:)` | `LanguageTranslator::new(src, tgt)` |
| `session.sourceLanguage` / `targetLanguage` | `translator.source()` / `target()` |
| `session.translate(String) -> Response` | `translator.translate(text) -> Result<Response>` |
| `session.translations(from: [Request]) -> [Response]` | `translator.translate_batch(requests)` |
| `session.prepareTranslation()` | `translator.prepare()` |
| `TranslationSession.Request` | `TranslationRequest` |
| `TranslationSession.Response` | `TranslationResponse` |
| `LanguageAvailability.status(from:to:)` | `check_language_availability(src, tgt)` |
| `NLLanguageRecognizer` | `detect_language(text)` |

The streaming `translate(batch:) -> BatchResponse` API is intentionally not
exposed. The blocking batch API `translations(from:)` covers the common
throughput case with less Rust-side machinery.

## Types

### `TranslationRequest`

Mirrors `TranslationSession.Request`:

```rust
pub struct TranslationRequest {
    pub source_text: String,
    pub client_identifier: Option<String>,
}
```

Convenience constructors: `TranslationRequest::new(text)`, `TranslationRequest::with_client_id(text, id)`. Implements `From<&str>` and `From<String>`.

### `TranslationResponse`

Mirrors `TranslationSession.Response`:

```rust
pub struct TranslationResponse {
    pub source_language: String,   // BCP-47, e.g. "zh-Hans"
    pub target_language: String,   // BCP-47, e.g. "en"
    pub source_text: String,       // original text submitted
    pub target_text: String,       // translated text
    pub client_identifier: Option<String>,  // echoed from request
}
```

### `LanguageTranslator`

Mirrors `TranslationSession(installedSource:target:)`:

```rust
let t = LanguageTranslator::new("zh-Hans", "en")?;

// Single-string
let response = t.translate("你好")?;

// Batch
let requests = vec![TranslationRequest::new("你好"), "世界".into()];
let results = t.translate_batch(&requests);

// Pre-warm
t.prepare()?;
```

### Free functions

```rust
// Language detection (synchronous by nature — NLLanguageRecognizer is sync)
let lang = detect_language("Bonjour le monde");  // Some("fr")

// Availability check
check_language_availability("zh-Hans", "en")?;
```

## Architecture

```
Rust user code
  │
  ├─ sync API
  │   translate()
  │   translate_batch()
  │   prepare()
  │   check_language_...()
  │
  └─ src/TranslationWrapper.swift
       ├─ mt_translate(source, target, text) → RustString?
       │    └─ runAsyncAndWait() → session.translate()
       ├─ mt_translate_batch(source, target, Vec<text>) → Vec<result>
       │    └─ runAsyncAndWait() → session.translations(from:)
       ├─ mt_prepare_translation(source, target) → RustString?
       │    └─ runAsyncAndWait() → session.prepareTranslation()
       ├─ SessionCache (NSLock + dict)  // TranslationSession reuse per pair
       └─ Translation.framework         // Apple on-device ML translation
```

### Sync path

`runAsyncAndWait()` bridges Swift async → sync:
1. `DispatchQueue.global().async` moves work off the calling thread.
2. `Task { await body() }` executes the async code on Swift Concurrency pool.
3. `DispatchSemaphore` + `CFRunLoopRunInMode` waits for completion.
4. A deadline prevents infinite hangs.

## Performance

### On-device ML inference is the primary bottleneck

Benchmarks (zh-Hans → en, ~219 char paragraph, 100 texts, Apple Silicon):

| Approach | Time | Throughput |
|----------|------|-----------|
| Sequential (100 individual calls, 1 session) | 36.1s | 2.8 req/s |
| Batch (1 call, 1 session) | 34.4s | 2.9 req/s |
| 2 sessions parallel (50 calls each) | 31.7s | 3.2 req/s |
| 4 sessions parallel (25 calls each) | 29.3s | 3.4 req/s |

- **Batch API eliminates per-call overhead**: ~34.4s vs 36.1s for sequential (5% faster).
- **Multi-session gives modest parallelism**: 4 sessions are 1.2× faster than 1.
  The on-device ML engine allows some concurrent work, but scaling is sub-linear
  (the Neural Engine / GPU is a shared resource).
- **Per-call overhead is ~20ms**: `runAsyncAndWait` setup (DispatchSemaphore +
  GCD dispatch + Task creation) plus FFI string marshaling.
- **ML inference dominates**: ~340ms per 219-char text. Shorter texts give
  proportionally higher throughput.

### Throughput by text length

Translation throughput is primarily determined by ML inference time, which
scales roughly linearly with text length. Expect ~0.6 chars/ms of inference
throughput. For single-sentence translations (30-50 chars), expect 10-20 req/s.

## File Layout

```
apple-translate-rs-sync/
├── Cargo.toml
├── rust-toolchain.toml       # nightly-2026-04-20
├── build.rs                  # Generate glue → compile Swift .a → link
├── AGENTS.md
├── src/
│   └── TranslationWrapper.swift
├── src/
│   ├── lib.rs                # Public sync API: types + LanguageTranslator
│   ├── ffi.rs                # #[swift_bridge::bridge] declarations
│   └── bin/
│       ├── translate-cli.rs  # CLI tool
│       ├── test-harness.rs   # Manual end-to-end API harness
│       └── stress.rs         # Stress test binary
└── tests/
    ├── integration_test.rs   # Basic translation + batch tests
    └── stress_test.rs        # Batch vs sequential comparison
```

## Build Process (`build.rs`)

1. **Generate glue**: `parse_bridges(["src/ffi.rs"])` → `write_all_concatenated()`
   outputs Swift/C glue into `generated/`.
2. **Compile Swift**: `swiftc -emit-library -static` compiles
   `TranslationWrapper.swift` + generated glue into `libapple_translate_rs_sync_swift.a`.
3. **Link**: static library + rpath `/usr/lib/swift` for Swift runtime dylibs.

## Key Design Decisions

### Sync-only Rust API

The Rust API is synchronous. Async wrappers and scheduler/streaming helpers were
removed because they did not increase maximum translation throughput and pulled
Tokio plus callback state into a small crate. Downstream async applications can
place the blocking calls behind their own bounded worker queue if needed.

### `TranslationRequest` / `TranslationResponse` wrapper types

Rather than passing raw strings, we expose `TranslationRequest` (with optional
`client_identifier` for correlating batch results) and `TranslationResponse`
(with full metadata: source/target language, source/target text, client ID).
These directly mirror the Swift types.

### Batch API via `translations(from:)`

`TranslationSession` conforms to `Translating`, which provides:
```swift
func translations(from batch: [TranslationSession.Request]) async throws -> [TranslationSession.Response]
```

This processes N texts in **one actor call**, avoiding the serial-executor
contention of N individual `translate()` calls queuing on the actor.

### `String` not `&str` for FFI

swift-bridge Issue #265: `&str` + `Result` generates broken Swift. All FFI
parameters use owned `String`.

## Concurrency Model (Swift Side)

### TranslationSession is an actor

`TranslationSession` is a Swift **actor** — only one task executes within it
at a time. Concurrency comes from using **multiple sessions** (each is an
independent actor) or the **batch API** (N texts in one actor invocation).

### Actor reentrancy

`TranslationSession` is reentrant: if a task suspends (e.g., waiting for ML
inference), another task can execute. Our code does not depend on actor-isolated
state across suspension points.

### Swift Concurrency runtime initialization

In a Rust-hosted process:
- Do NOT create a `Task` and block during initialization — this can deadlock.
- The first FFI call triggers initialization naturally.
- `runAsyncAndWait` creates `Task` from a background Dispatch queue.
- Timeouts (15s check, 30s translate, 60s batch/prepare) prevent hangs.

## Known Limitations

### On-device ML inference bottleneck

The Apple Neural Engine processes translations serially or with limited
parallelism. Multi-session gives modest speedup (1.2× for 4 sessions) but
does not scale linearly. Throughput is ~3 translations/second for 219-char
paragraphs. This is an Apple framework limitation.

### `SwiftBridgeCore.swift` is monolithic

`write_all_concatenated` generates the full runtime. Harmless for `.a` builds
but prevents `.dylib` builds.

