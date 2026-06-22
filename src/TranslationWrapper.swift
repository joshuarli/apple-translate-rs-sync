import Translation
import NaturalLanguage
import Foundation

// Error kind tags communicated to Rust via __mt_set_error.
let ERR_LANG_NOT_INSTALLED: Int32 = 1
let ERR_LANG_UNSUPPORTED: Int32 = 2
let ERR_TRANSLATION_FAILED: Int32 = 3
let ERR_TIMED_OUT: Int32 = 4

@_silgen_name("__mt_set_error")
func __mt_set_error(_ kind: Int32, _ messagePtr: UnsafePointer<CChar>?)

// Session pool for TranslationSession fallback path.
private let sessionLock = NSLock()
private var sessionPool: [String: [TranslationSession]] = [:]
private var sessionIndex: [String: Int] = [:]
private let POOL_SIZE = 4

private func getSession(src: Locale.Language, tgt: Locale.Language) -> TranslationSession {
    let key = "\(src)-\(tgt)"
    sessionLock.lock()
    defer { sessionLock.unlock() }
    if var pool = sessionPool[key], !pool.isEmpty {
        let idx = sessionIndex[key, default: 0]
        sessionIndex[key] = (idx + 1) % pool.count
        return pool[idx]
    }
    var pool: [TranslationSession] = []
    for _ in 0..<POOL_SIZE {
        pool.append(TranslationSession(installedSource: src, target: tgt))
    }
    sessionPool[key] = pool
    sessionIndex[key] = 1
    return pool[0]
}

private func runAsyncAndWait<T>(deadline: TimeInterval, body: @escaping () async -> T) -> T? {
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
    guard let language = recognizer.dominantLanguage else { return nil }
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
    let srcStr = source.toString()
    let tgtStr = target.toString()
    let input = text.toString()

    // Direct EMTTranslator in-process path works in standalone ObjC but
    // C++ exceptions from the quasar engine propagate through GCD worker
    // threads into Rust's panic handler (foreign exception unwinding).
    // Rust selects the isolated subprocess worker before using this fallback.

    let output = runAsyncAndWait(deadline: 30) { () -> String in
        let src = Locale.Language(identifier: srcStr)
        let tgt = Locale.Language(identifier: tgtStr)
        let session = getSession(src: src, tgt: tgt)
        do {
            return try await session.translate(input).targetText
        } catch {
            fputs("apple-translate-rs-sync: translation error: \(error.localizedDescription)\n", stderr)
            return ""
        }
    }
    guard let output = output else {
        let msg = "translation timed out after 30s"
        msg.withCString { __mt_set_error(ERR_TIMED_OUT, $0) }
        return nil
    }
    guard !output.isEmpty else {
        let msg = "translation failed"
        msg.withCString { __mt_set_error(ERR_TRANSLATION_FAILED, $0) }
        return nil
    }
    return RustString(output)
}

public func mt_translate_batch(source: RustString, target: RustString, texts: RustVec<RustString>) -> RustVec<RustString> {
    let srcStr = source.toString()
    let tgtStr = target.toString()

    var requests: [TranslationSession.Request] = []
    for text in texts {
        let s = text.as_str().toString()
        requests.append(TranslationSession.Request(sourceText: s))
    }

    let results = RustVec<RustString>()

    let responses = runAsyncAndWait(deadline: 60) { () -> [TranslationSession.Response] in
        let src = Locale.Language(identifier: srcStr)
        let tgt = Locale.Language(identifier: tgtStr)
        let session = getSession(src: src, tgt: tgt)
        do {
            return try await session.translations(from: requests)
        } catch {
            fputs("apple-translate-rs-sync: batch error: \(error.localizedDescription)\n", stderr)
            return []
        }
    }
    guard let responses = responses else {
        let msg = "batch translation timed out after 60s"
        msg.withCString { __mt_set_error(ERR_TIMED_OUT, $0) }
        return results
    }
    if responses.isEmpty {
        let msg = "batch translation failed"
        msg.withCString { __mt_set_error(ERR_TRANSLATION_FAILED, $0) }
        return results
    }
    for response in responses { results.push(value: RustString(response.targetText)) }
    return results
}

public func mt_prepare_translation(source: RustString, target: RustString) -> RustString? {
    let srcStr = source.toString()
    let tgtStr = target.toString()

    let maybeError = runAsyncAndWait(deadline: 60) { () -> String? in
        let src = Locale.Language(identifier: srcStr)
        let tgt = Locale.Language(identifier: tgtStr)
        let session = getSession(src: src, tgt: tgt)
        do { try await session.prepareTranslation(); return nil }
        catch { return error.localizedDescription }
    }
    guard let maybeError = maybeError else {
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
