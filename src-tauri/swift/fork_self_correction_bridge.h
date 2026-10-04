#ifndef fork_self_correction_bridge_h
#define fork_self_correction_bridge_h

// fork(voice-control): C surface of the self-correction on-device session
// (fork_self_correction.swift / fork_self_correction_stub.swift).

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// ForkScResult.status
#define FORK_SC_OK 0
#define FORK_SC_TIMEOUT 1
#define FORK_SC_ERROR 2
#define FORK_SC_UNAVAILABLE 3

typedef struct {
    char* text;  // Only set when status == FORK_SC_OK
    int status;
    char* error; // Only set when status == FORK_SC_ERROR / FORK_SC_UNAVAILABLE
} ForkScResult;

// 1 when the on-device model is usable right now.
int fork_sc_available(void);

// Idempotent, non-blocking: create + prewarm the session for `instructions`
// on a background task unless a session no older than `freshness_ms` is ready
// or a replacement is on its way. Deferred while a generation is in flight.
// Releases stale ready sessions first.
void fork_sc_prepare(const char* instructions, int64_t freshness_ms);

// Milliseconds since the ready session was prewarmed, -1 when none is ready.
int64_t fork_sc_prepared_age_ms(void);

// Blocking: generate with the ready (or a cold) session; returns after at most
// `timeout_ms`, cancelling the generation on timeout. Free with
// fork_sc_free_result. UNAVAILABLE with error "busy" when a prior generation
// is still unwinding cancellation, or session preparation is in progress.
ForkScResult* fork_sc_run(const char* text, int64_t timeout_ms);

void fork_sc_free_result(ForkScResult* result);

#ifdef __cplusplus
}
#endif

#endif /* fork_self_correction_bridge_h */
