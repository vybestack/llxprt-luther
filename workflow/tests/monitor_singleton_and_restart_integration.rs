//! Monitor singleton and restart policy integration tests.
//!
//! Tests for singleton lock acquisition, restart backoff policies,
//! and degraded state handling.

use std::time::Duration;

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-MON-001
/// Test: Singleton lock acquisition and release
#[tokio::test]
async fn test_singleton_lock_acquisition() {
    // GIVEN: no existing monitor lock
    let lock_path = "/tmp/luther-test.lock";
    
    // Clean up any existing lock
    let _ = std::fs::remove_file(lock_path);

    // WHEN: acquiring singleton lock
    let lock = luther_workflow::monitor::SingletonLock::acquire(lock_path)
        .expect("Should acquire singleton lock");

    // THEN: lock file exists and contains PID
    assert!(std::path::Path::new(lock_path).exists(), "Lock file should exist");
    
    let contents = std::fs::read_to_string(lock_path).expect("Should read lock file");
    let pid: u32 = contents.trim().parse().expect("Lock file should contain valid PID");
    assert_eq!(pid, std::process::id(), "Lock file should contain current process PID");

    // WHEN: attempting to acquire second lock
    let second_lock = luther_workflow::monitor::SingletonLock::acquire(lock_path);
    
    // THEN: fails with LockHeld error
    assert!(second_lock.is_err(), "Second lock acquisition should fail");
    match second_lock {
        Err(e) => {
            assert!(e.to_string().contains("lock") || e.to_string().contains("held"),
                "Error should indicate lock is held: {}", e);
        }
        Ok(_) => panic!("Expected lock to fail"),
    }

    // Clean up: drop first lock
    drop(lock);
    
    // THEN: lock file should be removed
    assert!(!std::path::Path::new(lock_path).exists(), "Lock file should be removed on drop");
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-MON-003
/// Test: Restart backoff policy (exponential backoff)
#[tokio::test]
async fn test_restart_backoff_policy() {
    // GIVEN: a restart policy with exponential backoff
    let policy = luther_workflow::monitor::RestartPolicy {
        max_restarts: 5,
        backoff_strategy: luther_workflow::monitor::BackoffStrategy::Exponential {
            initial_secs: 1,
            max_secs: 60,
            multiplier: 2.0,
        },
    };

    // WHEN: calculating backoff delays
    let delay_1 = policy.calculate_backoff(1);
    let delay_2 = policy.calculate_backoff(2);
    let delay_3 = policy.calculate_backoff(3);
    let delay_max = policy.calculate_backoff(10); // Should cap at max

    // THEN: delays follow exponential pattern
    assert_eq!(delay_1, Duration::from_secs(1), "First backoff should be initial value");
    assert_eq!(delay_2, Duration::from_secs(2), "Second backoff should be 2x initial");
    assert_eq!(delay_3, Duration::from_secs(4), "Third backoff should be 4x initial");
    assert_eq!(delay_max, Duration::from_secs(60), "Backoff should cap at max_secs");
}

/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-MON-004
/// Test: Restart limit reached triggers degraded state
#[tokio::test]
async fn test_restart_limit_degraded_state() {
    // GIVEN: a monitor with restart tracking
    let policy = luther_workflow::monitor::RestartPolicy {
        max_restarts: 3,
        backoff_strategy: luther_workflow::monitor::BackoffStrategy::Fixed {
            secs: 5,
        },
    };

    let mut tracker = luther_workflow::monitor::RestartTracker::new(policy);

    // WHEN: simulating restarts up to limit
    for _ in 0..3 {
        assert!(!tracker.should_enter_degraded(), 
            "Should not enter degraded state before limit");
        tracker.record_restart();
    }

    // THEN: after max_restarts, enters degraded state
    assert!(tracker.should_enter_degraded(), 
        "Should enter degraded state after max restarts");

    // WHEN: in degraded state
    let degraded_action = tracker.get_degraded_action();
    
    // THEN: returns appropriate degraded action
    assert!(matches!(degraded_action, 
        luther_workflow::monitor::DegradedAction::AlertAndWait),
        "Should return AlertAndWait degraded action");

    // Verify degraded actions include alerting, reduced functionality, or manual intervention
    assert!(degraded_action.requires_alert(), "Degraded action should require alert");
    assert!(!degraded_action.allows_new_work(), "Degraded action should not allow new work");
}
