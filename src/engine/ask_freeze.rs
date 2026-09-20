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

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

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
static CURRENT_SESSION: LazyLock<Mutex<Option<String>>> =
    LazyLock::new(|| Mutex::new(None));

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

/// Wrapper's chat session id (set by `zw-agent-embed::new_engine` and kept
/// for subagents, whose tools run outside `Engine` and thus have no
/// `CURRENT_SESSION`). Subagent `ask_user` and sibling tools share the same
/// wrapper session, so they must block on its counter too.
static WRAPPER_SESSION: LazyLock<Mutex<Option<String>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn set_wrapper_session(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    *WRAPPER_SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(session_id.to_string());
}

fn wrapper_session_id() -> Option<String> {
    WRAPPER_SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
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
    if let Some(id) = current_session_id() {
        if is_frozen(&id) {
            return true;
        }
    }
    if let Some(id) = wrapper_session_id() {
        if is_frozen(&id) {
            return true;
        }
    }
    false
}

/// Park until no `ask_user` is pending for the current session.
/// Non-ask tools call this at the very start of `Tool::call`; `ask_user`
/// itself never calls it (it *is* the freeze source). Checks both the
/// engine's current session and the wrapper's chat session (for subagents).
pub async fn wait_if_frozen() {
    // Prefer the engine's current session when inside a turn; otherwise
    // fall back to the wrapper's chat session (subagents).
    let id = current_session_id()
        .or_else(wrapper_session_id)
        .unwrap_or_default();
    if id.is_empty() {
        return;
    }
    loop {
        let st = state_for(&id);
        if st.count.load(Ordering::SeqCst) == 0 {
            return;
        }
        st.notify.notified().await;
    }
}
