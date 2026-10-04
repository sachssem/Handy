//! Single-flight blocking bridge. A timeout discards the result but leaves
//! the gate held until the underlying blocking work really finishes.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub(super) struct BlockingGate(Arc<AtomicBool>);

struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl BlockingGate {
    pub(super) async fn run<T, F>(&self, timeout: Duration, work: F) -> Result<T, &'static str>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let deadline = Instant::now() + timeout;
        if timeout.is_zero() {
            return Err("timeout");
        }
        self.0
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| "busy")?;
        let permit = InFlight(self.0.clone());
        let task = tauri::async_runtime::spawn_blocking(move || {
            // Own the permit inside the worker, never in the waiting future:
            // timing out or cancelling the wait must not permit another call.
            let _permit = permit;
            let result = work();
            (Instant::now(), result)
        });
        match tokio::time::timeout(timeout, task).await {
            Err(_) => Err("timeout"),
            Ok(Err(_)) => Err("error"),
            // Tokio polls the inner future before its timer. A delayed
            // waiter may therefore see a ready but late blocking result.
            Ok(Ok((finished, result))) => completed_before_deadline(deadline, finished, result),
        }
    }
}

fn completed_before_deadline<T>(
    deadline: Instant,
    finished: Instant,
    result: T,
) -> Result<T, &'static str> {
    if finished > deadline {
        Err("timeout")
    } else {
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tokio::sync::oneshot;

    #[test]
    fn ready_but_late_results_are_discarded() {
        let deadline = Instant::now();
        assert_eq!(
            completed_before_deadline(deadline, deadline, "on time"),
            Ok("on time")
        );
        assert_eq!(
            completed_before_deadline(deadline, deadline + Duration::from_nanos(1), "late"),
            Err("timeout")
        );
    }

    #[tokio::test]
    async fn timeout_discards_late_result_and_keeps_gate_busy_until_worker_finishes() {
        let gate = BlockingGate::default();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_gate = gate.clone();
        let request = tokio::spawn(async move {
            worker_gate
                .run(Duration::from_millis(20), move || {
                    started_tx.send(()).expect("started receiver");
                    release_rx.recv().expect("release worker");
                    "late result"
                })
                .await
        });
        started_rx.await.expect("worker started");
        let result = request.await.expect("request task");
        // Always unblock our worker, including on an assertion failure.
        let busy_result = gate
            .run(Duration::from_secs(1), || "unexpected second call")
            .await;
        release_tx.send(()).expect("release sender");
        assert_eq!(result, Err("timeout"));
        assert_eq!(busy_result, Err("busy"));
        wait_until_idle(&gate).await;
        assert_eq!(
            gate.run(Duration::from_secs(1), || "fresh result").await,
            Ok("fresh result")
        );
    }

    #[tokio::test]
    async fn concurrent_calls_skip_busy_without_executing_the_closure() {
        let gate = BlockingGate::default();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_gate = gate.clone();
        let request = tokio::spawn(async move {
            worker_gate
                .run(Duration::from_secs(1), move || {
                    started_tx.send(()).expect("started receiver");
                    release_rx.recv().expect("release worker");
                    "first"
                })
                .await
        });
        started_rx.await.expect("worker started");
        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = executions.clone();
        let result = gate
            .run(Duration::from_secs(1), move || {
                counter.fetch_add(1, Ordering::Relaxed);
                "second"
            })
            .await;
        release_tx.send(()).expect("release sender");
        assert_eq!(result, Err("busy"));
        assert_eq!(executions.load(Ordering::Relaxed), 0);
        assert_eq!(request.await.expect("request task"), Ok("first"));
    }

    #[tokio::test]
    async fn panicking_worker_releases_the_gate() {
        let gate = BlockingGate::default();
        assert_eq!(
            gate.run(Duration::from_secs(1), || -> () { panic!("mock failure") })
                .await,
            Err("error")
        );
        assert_eq!(
            gate.run(Duration::from_secs(1), || "recovered").await,
            Ok("recovered")
        );
    }

    async fn wait_until_idle(gate: &BlockingGate) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.0.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("mock worker finished");
    }
}
