//! Exact-hash stall detector for agent turns.
//!
//! The same tool called with byte-identical args producing byte-identical
//! output, N times in a row, is a loop, not progress. Anything that differs
//! (args or output) resets the streak, so legitimate polling and retries are
//! unaffected. Callers stop the turn when [`StallTracker::observe_result`]
//! returns true, instead of burning all the way to `MaxTurnsError`.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Consecutive identical (tool, args, output) pairs that declare a stall.
/// Two in a row can be a legitimate retry; three is a loop.
pub(crate) const IDENTICAL_REPEATS: usize = 3;

fn hash_pair(input_hash: u64, output: &str) -> u64 {
    let mut h = DefaultHasher::new();
    input_hash.hash(&mut h);
    output.hash(&mut h);
    h.finish()
}

/// Tracks one turn's tool (input, output) pairs by call id.
pub(crate) struct StallTracker {
    repeats: usize,
    inputs: HashMap<String, u64>,
    last: Option<u64>,
    streak: usize,
}

impl StallTracker {
    pub(crate) fn new(repeats: usize) -> Self {
        Self {
            repeats,
            inputs: HashMap::new(),
            last: None,
            streak: 0,
        }
    }

    /// Record a tool call's exact input; the matching result completes the pair.
    /// `args_json` must be canonical (e.g. `serde_json::to_string`, whose maps
    /// are key-sorted) so equivalent args hash equally.
    pub(crate) fn observe_call(&mut self, id: &str, tool: &str, args_json: &str) {
        let mut h = DefaultHasher::new();
        tool.hash(&mut h);
        args_json.hash(&mut h);
        self.inputs.insert(id.to_string(), h.finish());
    }

    /// Record a tool result. Returns true when this completes an identical
    /// streak that hits the repeat limit (caller should stop the turn).
    /// Results with no matching call are ignored and leave the streak alone.
    pub(crate) fn observe_result(&mut self, id: &str, output: &str) -> bool {
        let Some(input) = self.inputs.remove(id) else {
            return false;
        };
        let pair = hash_pair(input, output);
        if self.last == Some(pair) {
            self.streak += 1;
        } else {
            self.last = Some(pair);
            self.streak = 1;
        }
        self.streak >= self.repeats
    }

    /// Forget all state (e.g. the engine is retrying the attempt from scratch).
    pub(crate) fn reset(&mut self) {
        self.inputs.clear();
        self.last = None;
        self.streak = 0;
    }
}

#[cfg(test)]
mod tests;
