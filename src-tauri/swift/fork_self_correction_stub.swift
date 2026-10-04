import Foundation

// fork(voice-control): stub of fork_self_correction.swift for SDKs/toolchains
// without FoundationModels (mirrors upstream's apple_intelligence_stub.swift).

private typealias ResultPointer = UnsafeMutablePointer<ForkScResult>

@_cdecl("fork_sc_available")
public func forkScAvailable() -> Int32 {
    return 0
}

@_cdecl("fork_sc_prepare")
public func forkScPrepare(_ instructions: UnsafePointer<CChar>, _ freshnessMs: Int64) {}

@_cdecl("fork_sc_prepared_age_ms")
public func forkScPreparedAgeMs() -> Int64 {
    return -1
}

@_cdecl("fork_sc_run")
public func forkScRun(
    _ text: UnsafePointer<CChar>,
    _ timeoutMs: Int64
) -> UnsafeMutablePointer<ForkScResult> {
    let resultPtr = ResultPointer.allocate(capacity: 1)
    resultPtr.initialize(
        to: ForkScResult(
            text: nil,
            status: FORK_SC_UNAVAILABLE,
            error: strdup("Apple Intelligence is not available in this build (SDK requirement not met).")
        )
    )
    return resultPtr
}

@_cdecl("fork_sc_free_result")
public func forkScFreeResult(_ result: UnsafeMutablePointer<ForkScResult>?) {
    guard let result = result else { return }
    free(result.pointee.text)
    free(result.pointee.error)
    result.deallocate()
}
