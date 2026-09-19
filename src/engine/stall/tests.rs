use super::*;

fn args(cmd: &str) -> String {
    serde_json::json!({ "command": cmd }).to_string()
}

#[test]
fn three_identical_pairs_trigger() {
    let mut s = StallTracker::new(3);
    for i in 0..2 {
        s.observe_call(&format!("c{i}"), "bash", &args("sleep 2; echo ok"));
        assert!(
            !s.observe_result(&format!("c{i}"), "ok\n"),
            "pair {i} must not trigger"
        );
    }
    s.observe_call("c2", "bash", &args("sleep 2; echo ok"));
    assert!(
        s.observe_result("c2", "ok\n"),
        "third identical pair must trigger"
    );
}

#[test]
fn differing_output_resets() {
    let mut s = StallTracker::new(3);
    for i in 0..2 {
        s.observe_call(&format!("c{i}"), "bash", &args("check"));
        assert!(!s.observe_result(&format!("c{i}"), "ok\n"));
    }
    // Same input, new output: streak restarts, no trigger.
    s.observe_call("c2", "bash", &args("check"));
    assert!(!s.observe_result("c2", "changed\n"));
    // Two more identical to the new pair still only reach streak 2.
    s.observe_call("c3", "bash", &args("check"));
    assert!(!s.observe_result("c3", "changed\n"));
}

#[test]
fn differing_args_reset() {
    let mut s = StallTracker::new(3);
    s.observe_call("c0", "bash", &args("sleep 2; echo ok"));
    assert!(!s.observe_result("c0", "ok\n"));
    s.observe_call("c1", "bash", &args("sleep 4; echo ok"));
    assert!(!s.observe_result("c1", "ok\n"));
    // Back to the first command: streak restarts from 1, no trigger.
    s.observe_call("c2", "bash", &args("sleep 2; echo ok"));
    assert!(!s.observe_result("c2", "ok\n"));
}

#[test]
fn unknown_result_id_is_ignored() {
    let mut s = StallTracker::new(3);
    assert!(!s.observe_result("nope", "ok\n"));
    s.observe_call("c0", "bash", &args("check"));
    assert!(!s.observe_result("c0", "ok\n"));
}

#[test]
fn reset_clears_streak() {
    let mut s = StallTracker::new(3);
    for i in 0..2 {
        s.observe_call(&format!("c{i}"), "bash", &args("check"));
        assert!(!s.observe_result(&format!("c{i}"), "ok\n"));
    }
    s.reset();
    s.observe_call("c2", "bash", &args("check"));
    assert!(!s.observe_result("c2", "ok\n"));
}
