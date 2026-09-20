//! End-to-end acceptance tests for the goal ledger (dev plan §4.3):
//! profile creation → three turns (one blocked on the user) → single-step
//! resume → full ledger replay, plus cross-repo JSONL interoperability and
//! goal-id path-injection rejection.
use chrono::Utc;
use openpaw_goal_ledger::{
    decide_resume, parse_user_gate_question, record_resumed, record_run_summary, run_summary_event,
    FsGoalStore, GoalEvent, GoalEventLedger, GoalFrontmatter, GoalStatus, GoalStore,
    ResumeDecision, RunOutcome, RunSummary, UserGate,
};
use std::collections::BTreeSet;
const GOAL_ID: &str = "goal_beef0001"; // hex-only suffix on purpose
fn make_goal() -> GoalFrontmatter {
    let mut fm = GoalFrontmatter::new("publish the project wiki");
    fm.id = GOAL_ID.to_string();
    fm.status = GoalStatus::Active;
    fm.success_criteria = "wiki pages live under docs/".to_string();
    fm
}
fn make_summary(turns: u32, outcome: RunOutcome, text: &str, next_action: &str) -> RunSummary {
    RunSummary {
        session_id: format!("session_t{turns}"),
        started_at: Utc::now(),
        ended_at: Utc::now(),
        turns,
        tokens_used: 100 * turns as u64,
        outcome,
        summary: text.to_string(),
        next_action: next_action.to_string(),
    }
}
fn ledger_line_count(goals_dir: &std::path::Path, goal_id: &str) -> usize {
    let path = goals_dir.join(".events").join(format!("{goal_id}.jsonl"));
    std::fs::read_to_string(path)
        .expect("ledger file must exist after appends")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}
/// 1–4: profile creation → three turns → resume → full ledger replay.
#[test]
fn passbook_full_flow_e2e() {
    let tmp = tempfile::tempdir().unwrap();
    let goals_dir = tmp.path().to_path_buf();
    let store = FsGoalStore::new(goals_dir.clone());
    let ledger = GoalEventLedger::new(&goals_dir);
    // ── 1. Create the profile and read it back ──────────────────────────
    let goal = make_goal();
    let path = store
        .create_goal(goal.clone(), "passbook e2e body\n")
        .expect("profile creation must succeed");
    assert!(path.to_string_lossy().ends_with(".md"));
    let doc = store
        .load_goal(GOAL_ID)
        .unwrap()
        .expect("profile must load after creation");
    assert_eq!(doc.frontmatter.id, GOAL_ID);
    assert_eq!(doc.frontmatter.objective, "publish the project wiki");
    assert_eq!(doc.frontmatter.status, GoalStatus::Active);
    assert_eq!(
        doc.frontmatter.success_criteria,
        "wiki pages live under docs/"
    );
    assert!(doc.body.contains("passbook e2e body"));
    assert_eq!(store.list_goals().unwrap().len(), 1);
    ledger
        .append(&GoalEvent::new(
            "created",
            GOAL_ID,
            serde_json::json!({ "objective": doc.frontmatter.objective }),
        ))
        .unwrap();
    assert_eq!(ledger_line_count(&goals_dir, GOAL_ID), 1);
    // ── 2. Three ledger turns (every append is fsynced to disk) ─────────
    let mut fm = doc.frontmatter;
    // Turn 1: [GOAL_PROGRESS:0.3]
    let text1 = "drafted the outline [GOAL_PROGRESS:0.3]";
    let (outcome1, progress1) = RunOutcome::parse_from_summary(text1);
    assert_eq!(outcome1, RunOutcome::Progress);
    assert_eq!(progress1, Some(0.3));
    let s1 = make_summary(1, outcome1, text1, "write the pages");
    record_run_summary(&ledger, GOAL_ID, &s1).unwrap();
    fm.current_belief = s1.summary.clone();
    fm.next_action = s1.next_action.clone();
    fm.last_run_at = Some(Utc::now());
    store.save_goal(&fm, "").unwrap();
    assert_eq!(ledger_line_count(&goals_dir, GOAL_ID), 2);
    // Turn 2: [GOAL_PROGRESS:0.6] (value > 1 parsed as a percentage)
    let text2 = "wrote half the pages [GOAL_PROGRESS:60]";
    let (outcome2, progress2) = RunOutcome::parse_from_summary(text2);
    assert_eq!(outcome2, RunOutcome::Progress);
    assert_eq!(progress2, Some(0.6));
    let s2 = make_summary(2, outcome2, text2, "polish and cross-link pages");
    record_run_summary(&ledger, GOAL_ID, &s2).unwrap();
    fm.next_action = s2.next_action.clone();
    fm.last_run_at = Some(Utc::now());
    store.save_goal(&fm, "").unwrap();
    assert_eq!(ledger_line_count(&goals_dir, GOAL_ID), 3);
    // Turn 3: blocked on the user → marker parse path opens a user gate.
    let text3 =
        "the wiki needs a toolchain decision [GOAL_BLOCKED_ON_USER: Which wiki engine should we use?]";
    let question = parse_user_gate_question(text3).expect("BLOCKED_ON_USER marker must parse");
    assert_eq!(question, "Which wiki engine should we use?");
    let gate = UserGate::new(&question);
    fm.user_gates.push(gate);
    let (outcome3, _) = RunOutcome::parse_from_summary(text3);
    assert_eq!(
        outcome3,
        RunOutcome::NoProgress,
        "BLOCKED marker is not a progress marker"
    );
    let s3 = make_summary(
        3,
        RunOutcome::Blocked,
        text3,
        "wait for the user's engine choice",
    );
    record_run_summary(&ledger, GOAL_ID, &s3).unwrap();
    ledger
        .append(&GoalEvent::new(
            "user_gate_created",
            GOAL_ID,
            serde_json::json!({ "gate_id": fm.user_gates[0].id, "question": question }),
        ))
        .unwrap();
    fm.next_action = s3.next_action.clone();
    fm.last_run_at = Some(Utc::now());
    store.save_goal(&fm, "").unwrap();
    assert_eq!(ledger_line_count(&goals_dir, GOAL_ID), 5);
    // ── 3. Resume: decision from the ledger's last run summary ──────────
    let events = ledger.query(GOAL_ID).unwrap();
    // The unresolved gate wins: surface the question.
    assert_eq!(
        decide_resume(&fm, &events),
        ResumeDecision::WaitForUser {
            question: "Which wiki engine should we use?".to_string()
        }
    );
    // The user answers → gate resolved → resume seeded by the recorded
    // next_action of the newest run summary.
    fm.user_gates[0].resolved = true;
    fm.user_gates[0].resolution = Some("plain markdown".to_string());
    store.save_goal(&fm, "").unwrap();
    ledger
        .append(&GoalEvent::new(
            "user_gate_resolved",
            GOAL_ID,
            serde_json::json!({ "resolved": 1, "answer": "plain markdown" }),
        ))
        .unwrap();
    let reloaded = store.load_goal(GOAL_ID).unwrap().unwrap().frontmatter;
    assert!(
        reloaded.user_gates[0].resolved,
        "gate resolution must persist"
    );
    let events = ledger.query(GOAL_ID).unwrap();
    assert_eq!(
        decide_resume(&reloaded, &events),
        ResumeDecision::Run {
            next_action: "wait for the user's engine choice".to_string(),
        },
        "resume decision must be seeded by the last run summary's next_action"
    );
    record_resumed(&ledger, GOAL_ID, "wait for the user's engine choice").unwrap();
    // ── 4. Full ledger replay from a freshly opened ledger ──────────────
    let reopened = GoalEventLedger::new(&goals_dir);
    let all = reopened.query(GOAL_ID).unwrap();
    let kinds: Vec<&str> = all.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "created",
            "run_summary",
            "run_summary",
            "run_summary",
            "user_gate_created",
            "user_gate_resolved",
            "resumed",
        ],
        "event sequence must replay in append order"
    );
    assert!(
        all.iter().all(|e| e.goal_id == GOAL_ID),
        "every event carries the goal id"
    );
    // Field spot-checks on the replayed events.
    assert_eq!(all[0].payload["objective"], "publish the project wiki");
    assert_eq!(all[1].payload["next_action"], "write the pages");
    assert_eq!(all[2].payload["turns"], 2);
    assert_eq!(
        all[3].payload["outcome"], "blocked",
        "RunOutcome serializes snake_case"
    );
    assert_eq!(
        all[3].payload["next_action"],
        "wait for the user's engine choice"
    );
    assert_eq!(
        all[4].payload["question"],
        "Which wiki engine should we use?"
    );
    assert_eq!(all[5].payload["answer"], "plain markdown");
    assert_eq!(
        all[6].payload["next_action"],
        "wait for the user's engine choice"
    );
    // Timestamps are monotonic in append order.
    for pair in all.windows(2) {
        assert!(pair[0].timestamp <= pair[1].timestamp);
    }
    // The last run summary payload deserializes back into RunSummary.
    let last: RunSummary = serde_json::from_value(all[3].payload.clone()).unwrap();
    assert_eq!(last.turns, 3);
    assert_eq!(last.outcome, RunOutcome::Blocked);
    assert_eq!(last.next_action, "wait for the user's engine choice");
}
/// 5: cross-repo ledger interoperability — an upstream-schema JSONL line
/// parses on this side, and lines written here carry the same JSON key set.
#[test]
fn ledger_lines_are_interoperable_with_upstream_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let goals_dir = tmp.path().to_path_buf();
    let ledger = GoalEventLedger::new(&goals_dir);
    let id = "goal_dead0042"; // hex-only suffix
                              // (a) Hand-written upstream-schema event line → parseable here.
    let upstream_line = concat!(
        r#"{"type":"run_summary","timestamp":"2026-08-30T12:00:00Z","goal_id":"goal_dead0042","payload":"#,
        r#"{"session_id":"session_upstream","started_at":"2026-08-30T11:59:00Z","ended_at":"2026-08-30T12:00:00Z","turns":2,"tokens_used":900,"outcome":"progress","summary":"draft done [GOAL_PROGRESS:0.5]","next_action":"review the draft"}}"#
    );
    let events_dir = goals_dir.join(".events");
    std::fs::create_dir_all(&events_dir).unwrap();
    std::fs::write(
        events_dir.join(format!("{id}.jsonl")),
        format!("{upstream_line}\n"),
    )
    .unwrap();
    let parsed = ledger.query(id).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].event_type, "run_summary");
    assert_eq!(parsed[0].goal_id, id);
    let upstream_summary: RunSummary = serde_json::from_value(parsed[0].payload.clone())
        .expect("upstream run_summary payload must deserialize");
    assert_eq!(upstream_summary.next_action, "review the draft");
    assert_eq!(upstream_summary.outcome, RunOutcome::Progress);
    assert_eq!(upstream_summary.turns, 2);
    assert_eq!(upstream_summary.tokens_used, 900);
    // (b) A line written here → raw JSON key set matches the upstream shape.
    let ours = make_summary(1, RunOutcome::Progress, "local turn", "next local step");
    record_run_summary(&ledger, id, &ours).unwrap();
    let raw = std::fs::read_to_string(events_dir.join(format!("{id}.jsonl"))).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "one upstream line + one local line");
    let local: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let local_keys: BTreeSet<&str> = local
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected_keys: BTreeSet<&str> = ["type", "timestamp", "goal_id", "payload"]
        .into_iter()
        .collect();
    assert_eq!(
        local_keys, expected_keys,
        "local ledger line must carry exactly the upstream key set"
    );
    // The timestamp field round-trips as an RFC3339 DateTime.
    let ts: chrono::DateTime<Utc> =
        serde_json::from_value(local["timestamp"].clone()).expect("timestamp must be RFC3339");
    let _ = ts;
    // The payload key set matches the upstream RunSummary field set.
    let payload_keys: BTreeSet<&str> = local["payload"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected_payload: BTreeSet<&str> = [
        "session_id",
        "started_at",
        "ended_at",
        "turns",
        "tokens_used",
        "outcome",
        "summary",
        "next_action",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        payload_keys, expected_payload,
        "run_summary payload must match the upstream schema field set"
    );
    // And the profile schema round-trips through the frontmatter codec too:
    // an upstream-style goal document loads here unchanged.
    let store = FsGoalStore::new(goals_dir.clone());
    let profile = make_goal();
    let upstream_doc = format!(
        "---\nid: {}\ncreated: 2026-08-30T10:00:00Z\nsource: user\nconfidence: high\nobjective: upstream profile\nstatus: active\npriority: normal\nexecution_hint: direct\nquota:\n  compute: 1\n  window_hours: 24\n  allowed_turns: 100\n  spent_turns: 5\n  state: eligible\n---\nupstream body\n",
        profile.id
    );
    let doc_path = goals_dir.join("goal-upstream-style.md");
    std::fs::write(&doc_path, upstream_doc).unwrap();
    let loaded = store
        .load_goal(&profile.id)
        .unwrap()
        .expect("upstream profile must load");
    assert_eq!(loaded.frontmatter.objective, "upstream profile");
    assert_eq!(loaded.frontmatter.status, GoalStatus::Active);
    assert_eq!(loaded.frontmatter.quota.spent_turns, 5);
    assert_eq!(loaded.frontmatter.quota.window_hours, 24);
}
/// 6: goal-id path injection is rejected at every entry point, and an
/// injected id produces no files.
#[test]
fn path_injection_ids_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let goals_dir = tmp.path().to_path_buf();
    let store = FsGoalStore::new(goals_dir.clone());
    let ledger = GoalEventLedger::new(&goals_dir);
    let bad_ids = [
        "../evil",
        "goal_abc/../def",
        "/absolute",
        "goal_\\evil",
        "C:\\evil",
        "goal_",          // empty hex part
        "goal_not_hex!",  // non-hex characters
        "missing_prefix", // no goal_ prefix
        "",
    ];
    for bad in bad_ids {
        let event = GoalEvent::new("created", bad, serde_json::json!({}));
        assert!(ledger.append(&event).is_err(), "append must reject {bad:?}");
        assert!(ledger.query(bad).is_err(), "query must reject {bad:?}");
        let mut fm = GoalFrontmatter::new("injection probe");
        fm.id = bad.to_string();
        assert!(
            store.create_goal(fm, "body\n").is_err(),
            "create_goal must reject {bad:?}"
        );
    }
    // No injected id may leave any file behind.
    assert!(
        !goals_dir.join(".events").exists(),
        "no events dir may be created"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&goals_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        leftovers.is_empty(),
        "no file may be created from an injected id"
    );
    // Sanity: a legitimate hex id still works after all the rejections.
    let good = make_summary(1, RunOutcome::Progress, "ok", "keep going");
    let ledger = GoalEventLedger::new(&goals_dir);
    ledger
        .append(&run_summary_event("goal_cafe0042", &good).unwrap())
        .unwrap();
    assert_eq!(ledger.query("goal_cafe0042").unwrap().len(), 1);
}
