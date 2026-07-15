use super::{
    fnv64, persist, reconcile_retry_policy_locked, record_validation,
    record_validation_and_publish, reserve_launch, LaunchPhase, RecordValidationContext,
    RetryBudget, RetryCounters, RetryExhaustionReason, RetryScopeKey, RetryState,
    ValidatedResultPublication, ValidationTransition,
};
use crate::engine::executors::pr_followup_artifacts::{
    ClockSleeper, PrFollowupArtifactStore, RecoverableHistoryCandidate,
};
use crate::engine::executors::pr_followup_types::PrFollowupBinding;
use serde_json::json;
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
struct FixedClock;

impl ClockSleeper for FixedClock {
    fn now_rfc3339(&self) -> String {
        "2026-07-14T00:00:00Z".to_string()
    }

    fn sleep(&self, _duration: Duration) {}
}

#[test]
fn authenticated_exact_scope_tombstone_resets_prior_unreadable_scope_history() {
    let binding = validation_race_binding();
    let scope = validation_race_scope(&binding);
    let unreadable = RecoverableHistoryCandidate {
        path: "1-1-remediate_pr_followup.json".into(),
        value: Some(json!({ "artifact_sequence": 1 })),
        validation_error: Some("missing scope".to_string()),
    };
    let mut tombstone = validation_race_state(&binding);
    tombstone.budget = RetryBudget {
        max_remediation_attempts: 1,
        max_validation_retries: 1,
        max_stale_artifact_retries: 1,
    };
    tombstone.counters = RetryCounters {
        remediation_attempt_index: 1,
        validation_retry_index: 1,
        stale_artifact_retry_index: 1,
    };
    tombstone.transition_type = "terminal_tombstone".to_string();
    tombstone.launch_phase = LaunchPhase::Completed;
    tombstone.launch_ordinal = 1;
    tombstone.predecessor_artifact_sequence = None;
    tombstone.history_chain_reset = true;
    tombstone.lease_expiry = None;
    let mut tombstone_value = serde_json::to_value(&tombstone).expect("tombstone value");
    tombstone_value["artifact_sequence"] = json!(2);
    let reset = RecoverableHistoryCandidate {
        path: "2-2-post_pr_failure_terminal.json".into(),
        value: Some(tombstone_value),
        validation_error: None,
    };

    let latest = crate::engine::executors::pr_remediation::retry_history::latest_retry_snapshot(
        &[unreadable, reset],
        &scope,
    )
    .expect("authenticated reset must clear earlier unreadable scope")
    .expect("tombstone snapshot");
    assert_eq!(latest.0, 2);
    assert!(latest.2.history_chain_reset);
}

#[test]
fn configured_budget_can_only_tighten() {
    let persisted = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 3,
        "max_validation_retries": 4,
        "max_stale_artifact_retries": 5
    }))
    .expect("valid persisted budget");
    let expanded = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 9,
        "max_validation_retries": 9,
        "max_stale_artifact_retries": 9
    }))
    .expect("valid expanded budget");
    assert_eq!(persisted.tightened_with(expanded), persisted);
}

#[test]
fn transition_hash_is_stable() {
    assert_eq!(fnv64(b"retry"), 0x163c_a1f2_c427_ff19);
}

#[test]
fn reject_expansion_errors_on_increase() {
    let persisted = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 2,
        "max_validation_retries": 2,
        "max_stale_artifact_retries": 2
    }))
    .expect("valid persisted budget");
    let expanded = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 3,
        "max_validation_retries": 2,
        "max_stale_artifact_retries": 2
    }))
    .expect("valid expanded budget");
    assert!(persisted.reject_expansion(expanded).is_err());
}

#[test]
fn reject_expansion_allows_tightening() {
    let persisted = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 3,
        "max_validation_retries": 3,
        "max_stale_artifact_retries": 3
    }))
    .expect("valid persisted budget");
    let tightened = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 2,
        "max_validation_retries": 2,
        "max_stale_artifact_retries": 2
    }))
    .expect("valid tightened budget");
    assert!(persisted.reject_expansion(tightened).is_ok());
}

#[test]
fn reject_expansion_allows_equal() {
    let persisted = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 2,
        "max_validation_retries": 2,
        "max_stale_artifact_retries": 2
    }))
    .expect("valid persisted budget");
    let equal = RetryBudget::from_params(&json!({
        "max_remediation_attempts": 2,
        "max_validation_retries": 2,
        "max_stale_artifact_retries": 2
    }))
    .expect("valid equal budget");
    assert!(persisted.reject_expansion(equal).is_ok());
}

fn validation_race_binding() -> PrFollowupBinding {
    PrFollowupBinding {
        schema_version: 1,
        run_id: "validation-race".to_string(),
        repository_owner: "example".to_string(),
        repository_name: "workflow".to_string(),
        pr_number: 42,
        head_ref: "feature".to_string(),
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        base_ref: "main".to_string(),
        base_sha: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
    }
}

fn validation_race_scope(binding: &PrFollowupBinding) -> RetryScopeKey {
    RetryScopeKey {
        run_id: binding.run_id.clone(),
        repository_owner: binding.repository_owner.clone(),
        repository_name: binding.repository_name.clone(),
        pr_number: binding.pr_number,
        input_head_sha: binding.head_sha.clone(),
        remediation_plan_sequence: 1,
    }
}

fn validation_race_state(binding: &PrFollowupBinding) -> RetryState {
    RetryState {
        scope: validation_race_scope(binding),
        budget: RetryBudget {
            max_remediation_attempts: 2,
            max_validation_retries: 2,
            max_stale_artifact_retries: 2,
        },
        counters: RetryCounters {
            remediation_attempt_index: 1,
            validation_retry_index: 1,
            stale_artifact_retry_index: 0,
        },
        transition_id: "fnv64:durable".to_string(),
        launch_transition_id: "fnv64:launch".to_string(),
        transition_type: "fixable_malformed".to_string(),
        launch_phase: LaunchPhase::Completed,
        launch_ordinal: 1,
        remediation_step_order_index: 9,
        predecessor_artifact_sequence: None,
        history_chain_reset: false,
        validation_source_id: Some("same-source".to_string()),
        launch_result_promoted: false,
        owner_token: "durable-owner".to_string(),
        lease_expiry: None,
        invocation_timeout_seconds: 60,
        exhaustion_reason: None,
    }
}

fn spawn_validation_replay(
    store: Arc<PrFollowupArtifactStore>,
    binding: Arc<PrFollowupBinding>,
    durable: RetryState,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    result: Arc<Mutex<Option<Result<(), crate::engine::runner::EngineError>>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut stale = durable;
        stale.owner_token = "stale-owner".to_string();
        let hook = || {
            entered.wait();
            release.wait();
        };
        let transition = ValidationTransition {
            source_id: "same-source",
            validation_retry_index: 1,
            stale_artifact_retry_index: 0,
            transition_type: "fixable_malformed",
        };
        let observed = record_validation(
            RecordValidationContext {
                store: &store,
                binding: &binding,
                producer_step_id: "validate_remediation_result",
                step_order: 9,
                params: &json!({}),
                clock: &FixedClock,
                after_lock_hook: Some(&hook),
                after_transition_hook: None,
            },
            &mut stale,
            &transition,
        );
        *result.lock().expect("result lock") = Some(observed);
    })
}

#[test]
fn record_validation_replay_locks_before_reading_durable_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(PrFollowupArtifactStore::new(temp.path().join("artifacts")));
    let binding = Arc::new(validation_race_binding());
    let durable = validation_race_state(&binding);
    store
        .with_binding_publication_lock(&binding, || {
            persist(
                &store,
                &binding,
                "validate_remediation_result",
                9,
                &durable,
                &FixedClock,
            )
        })
        .expect("persist durable replay state");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let result = Arc::new(Mutex::new(None));
    let thread = spawn_validation_replay(
        store,
        binding,
        durable,
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::clone(&result),
    );
    entered.wait();
    assert!(result.lock().expect("result lock").is_none());
    release.wait();
    thread.join().expect("validation thread");
    let error = result
        .lock()
        .expect("result lock")
        .take()
        .expect("record result")
        .expect_err("stale owner must be rejected before replay return");
    assert!(error.to_string().contains("ownership changed"));
}

fn persist_completed_race_state(
    store: &PrFollowupArtifactStore,
    binding: &PrFollowupBinding,
) -> RetryState {
    let mut state = validation_race_state(binding);
    state.counters.validation_retry_index = 0;
    state.validation_source_id = None;
    state.transition_type = "launch_completed".to_string();
    store
        .with_binding_publication_lock(binding, || {
            persist(
                store,
                binding,
                "remediate_pr_followup",
                8,
                &state,
                &FixedClock,
            )
        })
        .expect("persist completed launch");
    state
}

fn spawn_validation_publication(
    store: Arc<PrFollowupArtifactStore>,
    binding: Arc<PrFollowupBinding>,
    mut state: RetryState,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
) -> std::thread::JoinHandle<super::PublishedValidatedResult> {
    std::thread::spawn(move || {
        let hook = || {
            entered.wait();
            release.wait();
        };
        record_validation_and_publish(
            RecordValidationContext {
                store: &store,
                binding: &binding,
                producer_step_id: "validate_remediation_result",
                step_order: 9,
                params: &json!({}),
                clock: &FixedClock,
                after_lock_hook: None,
                after_transition_hook: Some(&hook),
            },
            &mut state,
            &ValidationTransition {
                source_id: "published-source",
                validation_retry_index: 0,
                stale_artifact_retry_index: 0,
                transition_type: "valid",
            },
            ValidatedResultPublication {
                payload: &json!({ "validation_source_id": "published-source" }),
                failure: None,
            },
        )
        .expect("publish validation transaction")
    })
}

fn spawn_later_reservation(
    store: Arc<PrFollowupArtifactStore>,
    binding: Arc<PrFollowupBinding>,
    sent: mpsc::Sender<Result<RetryState, crate::engine::runner::EngineError>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = reserve_launch(
            &store,
            &binding,
            &json!({ "artifact_sequence": 1 }),
            &json!({}),
            "remediate_pr_followup",
            8,
            &FixedClock,
        );
        sent.send(result).expect("send reservation result");
    })
}

#[test]
fn validated_result_publication_blocks_a_later_launch_reservation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(PrFollowupArtifactStore::new(temp.path().join("artifacts")));
    let binding = Arc::new(validation_race_binding());
    let durable = persist_completed_race_state(&store, &binding);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let validation = spawn_validation_publication(
        Arc::clone(&store),
        Arc::clone(&binding),
        durable,
        Arc::clone(&entered),
        Arc::clone(&release),
    );
    entered.wait();

    let (sent, received) = mpsc::channel();
    let reservation = spawn_later_reservation(Arc::clone(&store), Arc::clone(&binding), sent);
    assert!(
        received.recv_timeout(Duration::from_millis(50)).is_err(),
        "later launch reservation must wait for validated result publication"
    );
    release.wait();
    let result_record = validation.join().expect("validation thread");
    let reserved = received
        .recv_timeout(Duration::from_secs(2))
        .expect("reservation result")
        .expect("reserve later launch");
    reservation.join().expect("reservation thread");
    let retry = store
        .read_current_json(&binding, super::RETRY_STATE_FAMILY)
        .expect("current retry state");
    assert_eq!(reserved.launch_ordinal, 2);
    assert!(
        retry["artifact_sequence"].as_u64().expect("retry sequence")
            > result_record.record.sequence.artifact_sequence,
        "later launch must be sequenced after validated result publication"
    );
}

#[test]
fn validation_publication_projects_post_transition_counters_and_replay_preserves_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = validation_race_binding();
    let mut state = persist_completed_race_state(&store, &binding);
    let publication_payload = json!({
        "validation_source_id": "malformed-source",
        "validation_retry_index": 0,
        "max_validation_retries": 99,
        "retry_scope": "malformed-agent-scope"
    });
    let transition = ValidationTransition {
        source_id: "malformed-source",
        validation_retry_index: 1,
        stale_artifact_retry_index: 0,
        transition_type: "fixable_malformed",
    };
    let publish = |state: &mut RetryState| {
        record_validation_and_publish(
            RecordValidationContext {
                store: &store,
                binding: &binding,
                producer_step_id: "validate_remediation_result",
                step_order: 9,
                params: &json!({}),
                clock: &FixedClock,
                after_lock_hook: None,
                after_transition_hook: None,
            },
            state,
            &transition,
            ValidatedResultPublication {
                payload: &publication_payload,
                failure: None,
            },
        )
        .expect("publish malformed validation")
    };

    let first = publish(&mut state);
    let durable = store
        .read_current_json(&binding, super::RETRY_STATE_FAMILY)
        .expect("durable retry state");
    let artifact = store
        .read_current_json(&binding, "pr-remediation-result")
        .expect("validated result");
    for payload in [&first.payload, &artifact] {
        assert_eq!(payload["validation_retry_index"], json!(1));
        assert_eq!(payload["retry_scope"]["validation_retry_index"], json!(1));
        assert_eq!(payload["max_validation_retries"], json!(2));
        assert_eq!(payload["retry_scope"]["max_validation_retries"], json!(2));
    }
    assert_eq!(durable["counters"]["validation_retry_index"], json!(1));

    let replay = publish(&mut state);
    assert_eq!(replay.record, first.record);
    assert_eq!(replay.payload["validation_retry_index"], json!(1));
    assert_eq!(
        replay.payload["retry_scope"]["validation_retry_index"],
        json!(1)
    );
    assert_eq!(
        store
            .read_current_json(&binding, super::RETRY_STATE_FAMILY)
            .expect("durable replay state")["counters"]["validation_retry_index"],
        json!(1)
    );
}

#[test]
fn reservation_tightening_below_consumed_persists_valid_exhaustion_across_reconstruction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = validation_race_binding();
    let mut durable = validation_race_state(&binding);
    durable.budget = RetryBudget {
        max_remediation_attempts: 3,
        max_validation_retries: 3,
        max_stale_artifact_retries: 3,
    };
    durable.counters.validation_retry_index = 2;
    store
        .with_binding_publication_lock(&binding, || {
            persist(
                &store,
                &binding,
                "validate_remediation_result",
                9,
                &durable,
                &FixedClock,
            )
        })
        .expect("persist retry state");

    let params = json!({
        "max_remediation_attempts": 3,
        "max_validation_retries": 1,
        "max_stale_artifact_retries": 3,
        "timeout_seconds": 60
    });
    let plan = json!({ "artifact_sequence": 1 });
    let error = reserve_launch(
        &store,
        &binding,
        &plan,
        &params,
        "remediate_pr_followup",
        8,
        &FixedClock,
    )
    .expect_err("consumed validation budget must exhaust reservation");
    assert!(error.to_string().contains("retry budget exhausted"));

    let reconstructed =
        super::load_current_state(&store, &binding, &validation_race_scope(&binding))
            .expect("reconstruct retry state")
            .expect("persisted retry state");
    assert_eq!(reconstructed.budget.max_validation_retries, 2);
    assert_eq!(reconstructed.counters.validation_retry_index, 2);
    assert_eq!(
        reconstructed.exhaustion_reason,
        Some(RetryExhaustionReason::ValidationRetries)
    );
}

#[test]
fn validation_tightening_below_consumed_persists_valid_exhaustion_across_reconstruction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = validation_race_binding();
    let mut durable = validation_race_state(&binding);
    durable.budget = RetryBudget {
        max_remediation_attempts: 3,
        max_validation_retries: 3,
        max_stale_artifact_retries: 3,
    };
    durable.counters.stale_artifact_retry_index = 2;
    store
        .with_binding_publication_lock(&binding, || {
            persist(
                &store,
                &binding,
                "validate_remediation_result",
                9,
                &durable,
                &FixedClock,
            )
        })
        .expect("persist retry state");

    let params = json!({
        "max_remediation_attempts": 3,
        "max_validation_retries": 3,
        "max_stale_artifact_retries": 1
    });
    let mut caller_state = durable;
    let transition = ValidationTransition {
        source_id: "new-source",
        validation_retry_index: 1,
        stale_artifact_retry_index: 2,
        transition_type: "fixable_malformed",
    };
    let error = record_validation(
        RecordValidationContext {
            store: &store,
            binding: &binding,
            producer_step_id: "validate_remediation_result",
            step_order: 9,
            params: &params,
            clock: &FixedClock,
            after_lock_hook: None,
            after_transition_hook: None,
        },
        &mut caller_state,
        &transition,
    )
    .expect_err("consumed stale budget must exhaust validation");
    assert!(error.to_string().contains("retry budget exhausted"));

    let reconstructed =
        super::load_current_state(&store, &binding, &validation_race_scope(&binding))
            .expect("reconstruct retry state")
            .expect("persisted retry state");
    assert_eq!(reconstructed.budget.max_stale_artifact_retries, 2);
    assert_eq!(reconstructed.counters.stale_artifact_retry_index, 2);
    assert_eq!(
        reconstructed.exhaustion_reason,
        Some(RetryExhaustionReason::StaleArtifactRetries)
    );
}

#[test]
fn terminal_policy_reconciliation_keeps_consumed_counters_valid() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = PrFollowupArtifactStore::new(temp.path().join("artifacts"));
    let binding = validation_race_binding();
    let mut durable = validation_race_state(&binding);
    durable.budget = RetryBudget {
        max_remediation_attempts: 3,
        max_validation_retries: 3,
        max_stale_artifact_retries: 3,
    };
    durable.counters.validation_retry_index = 2;
    store
        .with_binding_publication_lock(&binding, || {
            persist(
                &store,
                &binding,
                "validate_remediation_result",
                9,
                &durable,
                &FixedClock,
            )?;
            reconcile_retry_policy_locked(
                &store,
                &binding,
                "post_pr_failure_terminal",
                13,
                &mut durable,
                RetryBudget {
                    max_remediation_attempts: 2,
                    max_validation_retries: 1,
                    max_stale_artifact_retries: 2,
                },
                &FixedClock,
            )
        })
        .expect("terminal policy reconciliation");

    let reconstructed =
        super::load_current_state(&store, &binding, &validation_race_scope(&binding))
            .expect("reconstruct retry state")
            .expect("persisted retry state");
    reconstructed
        .counters
        .validate_against_budget(reconstructed.budget)
        .expect("reconciled state must satisfy persisted invariants");
    assert_eq!(reconstructed.budget.max_validation_retries, 2);
    assert_eq!(
        reconstructed.exhaustion_reason,
        Some(RetryExhaustionReason::ValidationRetries)
    );
}
