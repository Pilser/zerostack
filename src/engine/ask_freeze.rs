//! Freeze a turn while any `ask_user` is pending.
//!
//! The same `ask_user` + byte-identical args + output stall detector lives in
//! `stall.rs`; this module is the *other* backstop: when a human question is
//! outstanding, the agent must not do any other work beyond waiting. The
//! counter is per-session so main + 2 subagents asking concurrently all block
//! the same session, and each answer decrements. `wait_if_frozen` is called
//! by every non-ask tool before it starts, so siblings in the same batch
//! park instead of running while the question is open. A turn with all
//! `ask_user` calls therefore truly freezes — no tool, no final answer —
//! until the human answers or the ask times out.
//!
//! Session attribution, most-exact first: a task-local pin (runner and
//! subagent tasks are scoped at spawn via [`scoped`]), else the engine turn
//! global, else the wrapper's registered chat sessions. The process globals
//! alone are racy under concurrent turns on different sessions (last writer
//! wins), so embedders must scope spawned work — the pin is the exact
//! attribution, everything else is fallback.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::Notify;

tokio::task_local! {
    /// Session pinned for the current task. Set at spawn by [`scoped`].
    /// Task-locals do not cross `tokio::spawn`, which is exactly why the
    /// globals below cannot attribute concurrent turns — this pin can.
    static TASK_SESSION: String;
}

/// Run `fut` with `session_id` pinned for task-local freeze attribution.
/// `None` runs unscoped (legacy global resolution: tests, ACP path).
pub fn scoped<Fut>(
    session_id: Option<String>,
    fut: Fut,
) -> impl std::future::Future<Output = Fut::Output>
where
    Fut: std::future::Future,
{
    async move {
        match session_id {
            Some(id) => TASK_SESSION.scope(id, fut).await,
            None => fut.await,
        }
    }
}

/// Task-local pin, if any.
pub fn task_session() -> Option<String> {
    TASK_SESSION.try_with(|s| s.clone()).ok()
}

struct State {
    count: AtomicUsize,
    notify: Notify,
}

static MAP: LazyLock<Mutex<HashMap<String, Arc<State>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn state_for(session_id: &str) -> Arc<State> {
    let mut m = MAP.lock().unwrap_or_else(|e| e.into_inner());
    m.entry(session_id.to_string())
        .or_insert_with(|| {
            Arc::new(State {
                count: AtomicUsize::new(0),
                notify: Notify::new(),
            })
        })
        .clone()
}

/// Current session id for the running engine thread. Set by `Engine` at turn
/// start and cleared at turn end; tools read it to know which counter to check.
static CURRENT_SESSION: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

pub(crate) fn set_current_session(session_id: &str) {
    *CURRENT_SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(session_id.to_string());
}

pub(crate) fn clear_current_session() {
    *CURRENT_SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn current_session_id() -> Option<String> {
    CURRENT_SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Wrapper chat sessions known to this process (registered by
/// `zw-agent-embed::new_engine`, one per embedded engine). A SET, not a
/// single slot: engines are created per session and never dropped, so
/// last-writer-wins would silently re-attribute every subagent tool to
/// whichever session was created last.
static WRAPPER_SESSIONS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn set_wrapper_session(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    WRAPPER_SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id.to_string());
}

/// Forget a wrapper session (engine teardown). Embedded engines currently
/// live forever, so this is for headless churn and tests.
pub fn clear_wrapper_session(session_id: &str) {
    WRAPPER_SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session_id);
}

fn wrapper_sessions() -> Vec<String> {
    WRAPPER_SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

pub fn inc(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    state_for(session_id).count.fetch_add(1, Ordering::SeqCst);
}

pub fn dec(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    let st = state_for(session_id);
    let prev = st.count.fetch_sub(1, Ordering::SeqCst);
    if prev <= 1 {
        st.count.store(0, Ordering::SeqCst);
        st.notify.notify_waiters();
    } else {
        st.notify.notify_waiters();
    }
}

pub fn is_frozen(session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    MAP.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .map(|s| s.count.load(Ordering::SeqCst) > 0)
        .unwrap_or(false)
}

pub fn is_frozen_current() -> bool {
    candidate_sessions().iter().any(|id| is_frozen(id))
}

/// Best-effort single-session attribution for the caller, most-exact first:
/// task-local pin, else engine turn global, else the lone wrapper session.
/// `None` when unattributable (no context at all, or several wrapper
/// sessions with no pin) — callers that need a scope should propagate the
/// ambiguity to [`wait_if_frozen`], not guess.
pub fn resolve_session() -> Option<String> {
    if let Some(id) = task_session() {
        return Some(id);
    }
    if let Some(id) = current_session_id() {
        return Some(id);
    }
    let wrappers = wrapper_sessions();
    if wrappers.len() == 1 {
        return wrappers.into_iter().next();
    }
    None
}

/// Sessions relevant to the calling tool, most-exact first. A single entry
/// is the exact attribution; several entries mean concurrent turns outside
/// any pin — ambiguous, and [`wait_if_frozen`] parks on any of them (the
/// safe direction) instead of attributing to the wrong one.
fn candidate_sessions() -> Vec<String> {
    if let Some(id) = task_session() {
        return vec![id];
    }
    if let Some(id) = current_session_id() {
        return vec![id];
    }
    wrapper_sessions()
}

async fn wait_for_session(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    loop {
        let st = state_for(session_id);
        if st.count.load(Ordering::SeqCst) == 0 {
            return;
        }
        st.notify.notified().await;
    }
}

/// Park until no `ask_user` is pending for the current session.
/// Non-ask tools call this at the very start of `Tool::call`; `ask_user`
/// itself never calls it (it *is* the freeze source). Attribution is exact
/// inside scoped runner/subagent tasks; outside any pin with several live
/// sessions it parks while ANY known session is frozen rather than checking
/// the wrong counter.
pub async fn wait_if_frozen() {
    let candidates = candidate_sessions();
    if candidates.len() <= 1 {
        if let Some(id) = candidates.into_iter().next() {
            wait_for_session(&id).await;
        }
        return;
    }
    // Ambiguous: re-check the whole set on every wakeup so no session's
    // answer is missed while parked on another session's notify.
    loop {
        let frozen: Vec<String> = candidates
            .iter()
            .filter(|s| is_frozen(s))
            .cloned()
            .collect();
        if frozen.is_empty() {
            return;
        }
        wait_for_session(&frozen[0]).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: freeze globals are process-wide; every test uses unique session
    // ids and only asserts on state it controls (task-local pins win over
    // everything, so scoped assertions are deterministic under parallel
    // test execution).

    #[tokio::test]
    async fn task_pin_wins_for_attribution() {
        scoped(Some("freeze-test-pin-a".to_string()), async {
            assert_eq!(resolve_session().as_deref(), Some("freeze-test-pin-a"));
            assert_eq!(candidate_sessions(), vec!["freeze-test-pin-a".to_string()]);
        })
        .await;
    }

    #[tokio::test]
    async fn wrapper_set_remembers_all_sessions() {
        set_wrapper_session("freeze-test-w1");
        set_wrapper_session("freeze-test-w2");
        let all = wrapper_sessions();
        assert!(all.contains(&"freeze-test-w1".to_string()));
        assert!(all.contains(&"freeze-test-w2".to_string()));
        clear_wrapper_session("freeze-test-w1");
        clear_wrapper_session("freeze-test-w2");
        let rest = wrapper_sessions();
        assert!(!rest.contains(&"freeze-test-w1".to_string()));
        assert!(!rest.contains(&"freeze-test-w2".to_string()));
    }

    #[tokio::test]
    async fn scoped_wait_unblocks_on_dec() {
        let sid = "freeze-test-park-b";
        inc(sid);
        assert!(is_frozen(sid));
        let waiter = tokio::spawn(scoped(Some(sid.to_string()), wait_if_frozen()));
        // Give the waiter a chance to park on the frozen counter.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        dec(sid);
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("waiter hung after dec")
            .expect("waiter panicked");
        assert!(!is_frozen(sid));
    }
}
