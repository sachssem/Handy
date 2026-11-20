import Dispatch
import Foundation
import FoundationModels

// fork(voice-control): dedicated on-device session for the self-correction
// pass. Unlike upstream's per-call bridge (apple_intelligence.swift) it keeps
// ONE session with the self-correction instructions prewarmed ahead of the
// transcript, generates plain greedy text with a tight token cap, cancels the
// generation on timeout, and swaps in a fresh prewarmed session after every
// use so no transcript history accumulates.
//
// Compiled by build.rs (`fork_self_correction_build.rs`) for Apple Silicon
// targets whose SDK ships FoundationModels; fork_self_correction_stub.swift
// otherwise.

private typealias ResultPointer = UnsafeMutablePointer<ForkScResult>

/// Session state shared across FFI calls. Every field is guarded by `lock`.
private final class SessionState: @unchecked Sendable {
    let lock = NSLock()
    /// Instructions of the current/next session; nil until the first prepare.
    var instructions: String?
    /// The ready, prewarmed `LanguageModelSession` (typed `AnyObject` so the
    /// global stays usable on the macOS 11 deployment target).
    var session: AnyObject?
    /// Uptime (ns) at which `session` was prewarmed.
    var preparedAt: UInt64 = 0
    /// A background prepare is creating the next session.
    var preparing = false
    /// A generation remains active until respond unwinds, even after timeout.
    var inFlight = false
}

private let state = SessionState()

/// Generation output handed from the detached task to the blocked FFI caller.
/// Read only after the semaphore signalled, so no lock is needed.
private final class Outcome: @unchecked Sendable {
    var text: String?
    var error: String?
}

private func duplicateCString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    return text.withCString { strdup($0) }
}

private func modelAvailable() -> Bool {
    guard #available(macOS 26.0, *) else { return false }
    if case .available = SystemLanguageModel.default.availability {
        return true
    }
    return false
}

@available(macOS 26.0, *)
private func makePrewarmedSession(_ instructions: String) -> LanguageModelSession {
    let session = LanguageModelSession(model: .default, instructions: instructions)
    session.prewarm()
    return session
}

/// Create + prewarm the next session in the background unless one is ready
/// or already on its way.
private func schedulePrepare() {
    guard #available(macOS 26.0, *) else { return }
    state.lock.lock()
    guard let instructions = state.instructions,
          state.session == nil, !state.preparing, !state.inFlight else {
        state.lock.unlock()
        return
    }
    state.preparing = true
    state.lock.unlock()

    Task.detached(priority: .utility) {
        let session = makePrewarmedSession(instructions)
        // Instructions changed while this one was being built: build for the
        // new ones instead.
        if !adopt(session, builtFor: instructions) {
            schedulePrepare()
        }
    }
}

/// Store a freshly prewarmed session as the ready one; false when it was built
/// for stale instructions. Synchronous so the lock is never held across an
/// `await`.
private func adopt(_ session: AnyObject, builtFor instructions: String) -> Bool {
    state.lock.lock()
    defer { state.lock.unlock() }
    state.preparing = false
    guard state.instructions == instructions else { return false }
    if state.session == nil {
        state.session = session
        state.preparedAt = DispatchTime.now().uptimeNanoseconds
    }
    return true
}

private func finishGeneration() {
    state.lock.lock()
    state.inFlight = false
    state.lock.unlock()
    schedulePrepare()
}

@_cdecl("fork_sc_available")
public func forkScAvailable() -> Int32 {
    return modelAvailable() ? 1 : 0
}

@_cdecl("fork_sc_prepare")
public func forkScPrepare(_ instructions: UnsafePointer<CChar>, _ freshnessMs: Int64) {
    let swiftInstructions = String(cString: instructions)
    state.lock.lock()
    let ageMs = (DispatchTime.now().uptimeNanoseconds &- state.preparedAt) / 1_000_000
    if state.instructions != swiftInstructions || ageMs > UInt64(max(0, freshnessMs)) {
        state.instructions = swiftInstructions
        // A long-idle session may have lost its resident model under memory
        // pressure. Release it and prewarm a replacement at recording start.
        state.session = nil
    }
    state.lock.unlock()
    guard modelAvailable() else { return }
    schedulePrepare()
}

@_cdecl("fork_sc_prepared_age_ms")
public func forkScPreparedAgeMs() -> Int64 {
    state.lock.lock()
    defer { state.lock.unlock() }
    guard state.session != nil else { return -1 }
    let elapsed = DispatchTime.now().uptimeNanoseconds &- state.preparedAt
    return Int64(elapsed / 1_000_000)
}

@_cdecl("fork_sc_run")
public func forkScRun(
    _ text: UnsafePointer<CChar>,
    _ timeoutMs: Int64
) -> UnsafeMutablePointer<ForkScResult> {
    let swiftText = String(cString: text)
    let resultPtr = ResultPointer.allocate(capacity: 1)
    resultPtr.initialize(to: ForkScResult(text: nil, status: FORK_SC_ERROR, error: nil))

    // Report the outstanding generation before consulting availability: a
    // timed-out request still owns the model even if availability changed.
    state.lock.lock()
    let busy = state.inFlight || state.preparing
    state.lock.unlock()
    if busy {
        resultPtr.pointee.status = FORK_SC_UNAVAILABLE
        resultPtr.pointee.error = duplicateCString("busy")
        return resultPtr
    }

    guard #available(macOS 26.0, *), modelAvailable() else {
        resultPtr.pointee.status = FORK_SC_UNAVAILABLE
        resultPtr.pointee.error = duplicateCString(
            "Apple Intelligence is not currently available on this device."
        )
        return resultPtr
    }

    // Take the ready session: it serves exactly this one prompt.
    state.lock.lock()
    guard !state.inFlight, !state.preparing else {
        state.lock.unlock()
        resultPtr.pointee.status = FORK_SC_UNAVAILABLE
        resultPtr.pointee.error = duplicateCString("busy")
        return resultPtr
    }
    guard let instructions = state.instructions else {
        state.lock.unlock()
        resultPtr.pointee.error = duplicateCString("fork_sc_prepare was never called.")
        return resultPtr
    }
    // Claim before cold-session construction too: no concurrent prepare/run
    // may create another session while this generation owns the model.
    state.inFlight = true
    let ready = state.session as? LanguageModelSession
    state.session = nil
    state.lock.unlock()

    let session = ready ?? makePrewarmedSession(instructions)

    // The guard only accepts deletions, so the answer is never longer than
    // the input; ~2x its tokens (≈ 4 chars per token) leaves room for
    // punctuation drift while bounding a runaway generation.
    let maxTokens = max(48, swiftText.unicodeScalars.count / 2)
    let options = GenerationOptions(sampling: .greedy, maximumResponseTokens: maxTokens)

    let outcome = Outcome()
    let semaphore = DispatchSemaphore(value: 0)
    let task = Task.detached(priority: .userInitiated) {
        do {
            let response = try await session.respond(to: swiftText, options: options)
            outcome.text = response.content
        } catch {
            outcome.error = String(describing: error)
        }
        // respond has finished or cancellation has actually unwound. The
        // blocked caller may already have returned; only this task releases
        // the generation claim and schedules a replacement session.
        finishGeneration()
        semaphore.signal()
    }

    let deadline = DispatchTime.now() + .milliseconds(Int(max(0, timeoutMs)))
    if semaphore.wait(timeout: deadline) == .timedOut {
        task.cancel()
        resultPtr.pointee.status = FORK_SC_TIMEOUT
        return resultPtr
    }

    if let output = outcome.text {
        resultPtr.pointee.status = FORK_SC_OK
        resultPtr.pointee.text = duplicateCString(output)
    } else {
        resultPtr.pointee.error = duplicateCString(outcome.error ?? "Unknown error")
    }
    return resultPtr
}

@_cdecl("fork_sc_free_result")
public func forkScFreeResult(_ result: UnsafeMutablePointer<ForkScResult>?) {
    guard let result = result else { return }
    free(result.pointee.text)
    free(result.pointee.error)
    result.deallocate()
}
