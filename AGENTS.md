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

// Tune worker engine count (must be called before first translation).
// Default is 1 — bump to 2–4 for batch-short-text workloads.
apple_translate_rs_sync::set_worker_num_engines(4);
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

### Worker pool (subprocess + EMTTranslator engines)

Benchmarks (zh-Hans → en, ~1310 char article, 20 texts, warmed worker, 4 engines):

| Approach | Chars/sec | Speedup vs base |
|----------|-----------|-----------------|
| TranslationSession (single proc) | 1,749 | 1.00× |
| TranslationSession (16 procs) | 2,705 | 1.55× |
| **Worker pool (single proc, 4 engines)** | **8,740** | **5.00×** |
| Worker pool (16 procs) | 8,244 | 4.71× |

The worker pool achieves 5.0× throughput by running 4 `EMTTranslator`
engines (each with `useGlobalTranslationQueue:NO`) concurrently in a
subprocess. Each engine processes texts on its own CPU core.

Engine scaling (same batch, varying engine count):
| Engines | Chars/sec | Scaling |
|---------|-----------|---------|
| 1 | 2,913 | 1.00× |
| 2 | 5,244 | 1.80× |
| 4 | 8,740 | 3.00× |
| 8 | OOM | — |

The default is 1 engine per worker. Additional engines only help when a
single `translate_batch` call contains many texts — the batch items are
distributed round-robin across engines. For single-item batches (long-form
text translation), only one engine does work regardless of count.

4 engines gives ~3× throughput over 1 engine for batch-short-text workloads
on Apple Silicon. 8 engines causes OOM from loading the ~88MB model 8×.

**Long-text throughput** (zh-Hans → en, ~1310 chars, batch size 1, multi-process):

| Engines/worker | 1 proc | 2 proc | 4 proc | 8 proc | 16 proc |
|---|---|---|---|---|---|
| 1 | 1,593 | 3,256 | 5,299 | 7,856 | 8,926 |
| 2 | 1,740 | 3,177 | 5,357 | 7,417 | 8,894 |
| 3 | 1,666 | 3,210 | 5,566 | 8,209 | 8,754 |
| 4 | 1,690 | 3,218 | 5,338 | 7,236 | 8,949 |

Engine count has no measurable effect on single-item throughput — only
process count matters. The default of 1 engine minimizes memory with no
throughput regression.

With multiple language pairs, each pair gets its own worker subprocess.
Reduce the per-worker count to keep total engines within budget:

```rust
apple_translate_rs_sync::set_worker_num_engines(2);  // bump up for batch workloads
```

Also settable via `APPLE_TRANSLATE_RS_SYNC_WORKER_NUM_ENGINES`.

For short sentence-length texts (~15-25 chars), the batch API
(`translations(from:)`) gives ~12× improvement over sequential
individual calls (40 req/s vs 3.2 req/s).

### `translationd` architecture (sampled during load)

- **One serial NSOperationQueue** (`0x73934c000`, QOS: UNSPECIFIED)
  handles ALL translations regardless of client count
- **All CPU kernels**: `dynamic_quantize_kernel_cpu`,
  `dynamic_dequantize_kernel_cpu`, `instancenorm_1d_kernel_cpu` —
  the Espresso BNNSEngine runs entirely on CPU, not ANE/GPU
- **Single-threaded quasar pipeline**: `EMTTranslator` →
  `quasar::HotfixTranslator::translate` →
  `quasar::PDecTranslator::translate` →
  `quasar::ProcessingGraph::run` →
  `ESNetworkPlan::RunClassic` → `espresso_plan_execute_sync` →
  `Espresso::layer::__launch` → `*_cpu` kernels
- Extra processes/sessions don't create additional queues — all work
  funnels through the same NSOperationQueue

### Session pool

`TranslationSession` instances are pooled (4 per language pair,
round-robin) to reduce actor contention. This gives ~7% throughput
improvement at moderate concurrency but does not change the fundamental
serial bottleneck inside `translationd`.

### `EMTTranslator` with `useGlobalTranslationQueue:NO`

`EMTTranslator` (from `EmbeddedAcousticRecognition.framework`) has a
hidden init flag:

```
initWithModelURL:task:skipNonFinalToCatchup:
  translatorCacheSize:useGlobalTranslationQueue:
```

Setting `useGlobalTranslationQueue:NO` gives each engine its own
serial dispatch queue instead of sharing `translationd`'s global one,
enabling true multi-core parallelism.

The model files are at:
- Neural model: `~/Library/Translation/AssetsV3/<pair>/MT-bi-en-zh-ja-ko-20/MT/`
  (`pyespresso.mdl.bin`, `encoder.espresso.net/weights`,
  `spm.model`, phrase-book `.dict` files)
- Pipeline config: `~/Library/Translation/AssetsV3/<pair>/mt-quasar-config.json`
  (symlink to system asset)

**Status**: `createEngine` and `engineTranslate` work correctly in a
standalone ObjC binary (translates successfully, no exceptions).
When compiled into the Rust+Swift binary, `createEngine` succeeds but
`translateString:from:to:completion:` triggers a C++ exception
(`quasar::QuasarExceptionMessage` → `std::runtime_error`) on the
engine's internal GCD queue. Rust's panic handler intercepts the
foreign unwind and aborts. `@try/@catch` only catches ObjC exceptions;
per-thread C++ try/catch can't reach the async queue.

The helper code lives in `src/EngineHelper.m` — compiled and linked
but the direct call path in `TranslationWrapper.swift` is disabled
pending a subprocess-based isolation approach.

### Throughput by text length

Translation throughput is primarily determined by ML inference time, which
scales roughly linearly with text length. Expect ~0.6 chars/ms of inference
throughput. For single-sentence translations (30-50 chars), expect 10-20 req/s.

### Performance verification process

`src/bin/stress.rs` is the single manual verification suite. It contains both
functional checks and throughput checks:

```bash
cargo run --release --bin stress -- functional
cargo run --release --bin stress -- batch
cargo run --release --bin stress -- long
cargo run --release --bin stress -- parallel
cargo run --release --bin stress -- all
```

- `functional`: language detection, availability, single translation,
  batch translation, client identifiers, `prepare()`, and worker startup.
- `batch`: short sentence `translate_batch` throughput in one process.
- `long`: article-length multi-process `translate_batch` throughput,
  reported as chars/sec.
- `parallel`: concurrent `translate()` throughput for the
  `TranslationSession` fallback path.
- `all`: all of the above. This can take several minutes.

For any refactor touching `src/lib.rs`, `src/worker_pool.rs`,
`src/translation-worker.m`, `src/TranslationWrapper.swift`, or `build.rs`:

1. Run `cargo test` and `cargo run --release --bin stress -- functional`.
2. Run the relevant `stress` mode on the baseline commit and on the
   refactor commit using the same machine, same installed models, same power
   state, and no other translation workload.
3. Ignore the first run if it includes model or worker startup. Compare the
   median of at least three warm runs.
4. Treat changes within about 5-10% as noise unless the same direction repeats
   across all runs. Investigate larger regressions before merging.
5. Record the command output, commit SHAs, macOS version, machine model, and
   whether `translation-worker: 4 engines ready` appeared.

A convenient before/after pattern is:

```bash
git worktree add /tmp/apple-translate-baseline <baseline-sha>
(cd /tmp/apple-translate-baseline && cargo run --release --bin stress -- long)
cargo run --release --bin stress -- long
git worktree remove /tmp/apple-translate-baseline
```

## File Layout

```
apple-translate-rs-sync/
├── Cargo.toml
├── rust-toolchain.toml       # nightly-2026-04-20
├── build.rs                  # Generate glue → compile Swift + ObjC → link
├── AGENTS.md
├── src/
│   ├── TranslationWrapper.swift  # Public FFI + session pool
│   ├── EngineHelper.m            # ObjC helper (linked into lib, direct path disabled)
│   ├── translation-worker.m      # Standalone ObjC binary: EMTTranslator engines in subprocess
│   ├── lib.rs                    # Public sync API + worker pool integration
│   ├── ffi.rs                    # #[swift_bridge::bridge] declarations
│   ├── worker_pool.rs            # Subprocess manager for translation-worker
│   └── bin/
│       ├── translate-cli.rs      # CLI tool
│       └── stress.rs             # Functional + throughput verification suite
└── tests/
    └── integration_test.rs       # Basic translation + batch tests
```

## Build Process (`build.rs`)

1. **Generate glue**: `parse_bridges(["src/ffi.rs"])` → `write_all_concatenated()`
   outputs Swift/C glue into `generated/`.
2. **Compile ObjC helper**: `clang -c src/EngineHelper.m -fobjc-arc` →
   `EngineHelper.o`.
3. **Compile + link Swift**: `swiftc -emit-library -static` compiles
   `TranslationWrapper.swift` + generated glue, links `EngineHelper.o`
   into `libapple_translate_rs_sync_swift.a`.
4. **Link**: static library + rpath `/usr/lib/swift` for Swift runtime dylibs.

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

`translationd` serializes all translation work through a single
NSOperationQueue. The Espresso neural engine runs entirely on CPU
(`*_cpu` kernels), not ANE/GPU. Multi-session gives modest speedup
(1.5× at 16 concurrent processes) via pipelining, not parallel
execution. Max aggregate throughput is ~2,700 chars/s for article-length
zh-Hans→en text. This is a `translationd` architecture limitation.

### Worker subprocess

The `translation-worker` binary (`src/translation-worker.m`) hosts
`EMTTranslator` engines in a standalone ObjC process, isolating C++
exceptions from the Rust FFI boundary. `src/worker_pool.rs` manages
the subprocess lifecycle and caches workers per language pair.

Each worker runs `worker_num_engines()` engines (default 1, overridable
via `set_worker_num_engines()` or the `APPLE_TRANSLATE_RS_SYNC_WORKER_NUM_ENGINES`
env var). The worker drains its autorelease pool after each batch to
avoid unbounded memory growth.

The worker uses a count-based stdin/stdout protocol:
```
<stdin>  <count>\n<text1>\n<text2>\n...
<stdout> <translated1>\n<translated2>\n...
```
Send `count=0` to shut down the worker.

### TranslationSession fallback

When the worker binary or AssetsV3 directory can't be found,
`translate_batch` falls back to the TranslationSession path
(session pool with 4 actors per pair).

### `SwiftBridgeCore.swift` is monolithic

`write_all_concatenated` generates the full runtime. Harmless for `.a` builds
but prevents `.dylib` builds.
