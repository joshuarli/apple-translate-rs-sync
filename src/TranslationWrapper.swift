import Translation
import NaturalLanguage
import Foundation

// Error kind tags communicated to Rust via __mt_set_error.
// These must match the ERR_* constants in lib.rs.
let ERR_LANG_NOT_INSTALLED: Int32 = 1
let ERR_LANG_UNSUPPORTED: Int32 = 2
let ERR_TRANSLATION_FAILED: Int32 = 3
let ERR_TIMED_OUT: Int32 = 4

// C function exposed by Rust. Called before returning an error from a
// synchronous FFI function so the Rust wrapper can construct the
// appropriate TranslationError variant.
@_silgen_name("__mt_set_error")
func __mt_set_error(_ kind: Int32, _ messagePtr: UnsafePointer<CChar>?)

// Session cache: TranslationSession instances are reused across calls
// for the same language pair to avoid repeated model initialization overhead.
private let sessionLock = NSLock()
private var sessionCache: [String: TranslationSession] = [:]

private func getSession(src: Locale.Language, tgt: Locale.Language) -> TranslationSession {
    let key = "\(src)-\(tgt)"
    sessionLock.lock()
    if let cached = sessionCache[key] {
        sessionLock.unlock()
        return cached
    }
    sessionLock.unlock()

    let session = TranslationSession(installedSource: src, target: tgt)

    sessionLock.lock()
    sessionCache[key] = session
    sessionLock.unlock()
    return session
}

/// Run an async body on a background Dispatch queue, blocking the calling
/// thread until the body completes or the deadline expires.
///
/// Returns nil if the operation times out.
///
/// Safety: the semaphore's signal→wait edge provides happens-before ordering
/// between the writer (Task closure) and the reader (after sem.wait()).
/// Using nonisolated(unsafe) on `result` suppresses the Swift 6 Sendable
/// warning — the access pattern is single-writer, single-reader, ordered by
/// the semaphore, so no data race is possible.
private func runAsyncAndWait<T>(
    deadline: TimeInterval,
    body: @escaping () async -> T
) -> T? {
    let sem = DispatchSemaphore(value: 0)
    nonisolated(unsafe) var result: T? = nil

    DispatchQueue.global().async {
        Task {
            result = await body()
            sem.signal()
        }
    }

    let cutoff = Date(timeIntervalSinceNow: deadline)
    while sem.wait(timeout: .now() + 0.1) == .timedOut {
        if Date() > cutoff { return nil }
        CFRunLoopRunInMode(.defaultMode, 0.01, true)
    }
    return result
}

public func mt_detect_language(text: RustString) -> RustString? {
    let recognizer = NLLanguageRecognizer()
    recognizer.processString(text.toString())
    guard let language = recognizer.dominantLanguage else {
        return nil
    }
    return RustString(language.rawValue)
}

public func mt_check_languages(source: RustString, target: RustString) -> RustString? {
    let srcStr = source.toString()
    let tgtStr = target.toString()
    let src = Locale.Language(identifier: srcStr)
    let tgt = Locale.Language(identifier: tgtStr)

    let status = runAsyncAndWait(deadline: 15) {
        await LanguageAvailability().status(from: src, to: tgt)
    }

    guard let status = status else {
        let msg = "Language availability check timed out after 15s"
        msg.withCString { __mt_set_error(ERR_TIMED_OUT, $0) }
        return RustString(msg)
    }

    if status != .installed {
        let detail = switch status {
        case .supported: "model not downloaded (status: supported)"
        case .unsupported: "pair unsupported (status: unsupported)"
        default: "status: \(status)"
        }
        let msg = "Language pair not available: \(srcStr) -> \(tgtStr) (\(detail))"
        let kind = status == .unsupported ? ERR_LANG_UNSUPPORTED : ERR_LANG_NOT_INSTALLED
        msg.withCString { __mt_set_error(kind, $0) }
        return RustString(msg)
    }
    return nil
}

public func mt_translate(source: RustString, target: RustString, text: RustString) -> RustString? {
    let src = Locale.Language(identifier: source.toString())
    let tgt = Locale.Language(identifier: target.toString())
    let input = text.toString()

    // Availability was already checked in mt_check_languages when the
    // translator was constructed. We skip the redundant check here.
    // Session is reused from the cache for subsequent calls.

    let output = runAsyncAndWait(deadline: 30) { () -> String in
        let session = getSession(src: src, tgt: tgt)
        do {
            let response = try await session.translate(input)
            return response.targetText
        } catch {
            fputs("apple-translate-rs-sync: translation error: \(error.localizedDescription)\n", stderr)
            return ""  // empty signals error to caller
        }
    }

    guard let output = output else {
        // Timeout — runAsyncAndWait returned nil.
        let msg = "translation timed out after 30s"
        msg.withCString { __mt_set_error(ERR_TIMED_OUT, $0) }
        return nil
    }
    guard !output.isEmpty else {
        // Framework error — the closure caught an error and returned "".
        let msg = "translation failed"
        msg.withCString { __mt_set_error(ERR_TRANSLATION_FAILED, $0) }
        return nil
    }
    return RustString(output)
}

/// Batch-translate multiple texts in a single `TranslationSession` invocation.
///
/// Uses `session.translations(from:)` which processes all requests in one
/// actor call — dramatically more efficient than N individual `translate()`
/// calls, which would each contend for the actor's serial executor.
///
/// Returns a `RustVec<RustString>` with one result per input text.
/// An empty result vec signals that the entire batch failed (timeout or error).
public func mt_translate_batch(source: RustString, target: RustString, texts: RustVec<RustString>) -> RustVec<RustString> {
    let src = Locale.Language(identifier: source.toString())
    let tgt = Locale.Language(identifier: target.toString())

    var requests: [TranslationSession.Request] = []
    for text in texts {
        requests.append(TranslationSession.Request(sourceText: text.as_str().toString()))
    }

    let responses = runAsyncAndWait(deadline: 60) { () -> [TranslationSession.Response] in
        let session = getSession(src: src, tgt: tgt)
        do {
            return try await session.translations(from: requests)
        } catch {
            fputs("apple-translate-rs-sync: batch error: \(error.localizedDescription)\n", stderr)
            return []
        }
    }

    let results = RustVec<RustString>()
    guard let responses = responses else {
        // Timeout — runAsyncAndWait returned nil.
        let msg = "batch translation timed out after 60s"
        msg.withCString { __mt_set_error(ERR_TIMED_OUT, $0) }
        return results
    }
    if responses.isEmpty {
        // Framework error — the closure caught an error and returned [].
        let msg = "batch translation failed"
        msg.withCString { __mt_set_error(ERR_TRANSLATION_FAILED, $0) }
        return results
    }
    for response in responses {
        results.push(value: RustString(response.targetText))
    }
    return results
}

/// Pre-warm the translation engine for a language pair.
///
/// Calls `session.prepareTranslation()` which forces model download / engine
/// warmup. Call this before a critical translation path to avoid first-use
/// latency. Returns nil on success, Some(error) on failure or timeout.
public func mt_prepare_translation(source: RustString, target: RustString) -> RustString? {
    let src = Locale.Language(identifier: source.toString())
    let tgt = Locale.Language(identifier: target.toString())

    let maybeError = runAsyncAndWait(deadline: 60) { () -> String? in
        let session = getSession(src: src, tgt: tgt)
        do {
            try await session.prepareTranslation()
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    // runAsyncAndWait returns T? (nil on timeout), and the closure returns
    // String? (nil on success). Flatten the double optional.
    guard let maybeError = maybeError else {
        // Timeout — runAsyncAndWait returned nil.
        let msg = "prepare timed out after 60s"
        msg.withCString { __mt_set_error(ERR_TIMED_OUT, $0) }
        return RustString(msg)
    }
    if let errorMsg = maybeError {
        errorMsg.withCString { __mt_set_error(ERR_TRANSLATION_FAILED, $0) }
        return RustString(errorMsg)
    }
    return nil
}
