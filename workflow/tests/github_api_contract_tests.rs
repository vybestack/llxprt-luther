use std::process::Command;

#[test]
fn github_api_contract_validator_accepts_phase_02_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_github_api_contract_validator"))
        .args([
            "project-plans/coderabbit/analysis/github-api-contract.md",
            "tests/fixtures/github_api_contract",
        ])
        .output()
        .expect("validator command should execute");

    assert!(
        output.status.success(),
        "validator failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("github_api_contract_validator: PASS"));
}

#[test]
fn github_api_contract_negative_fixture_rejects_missing_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_github_api_contract_validator"))
        .args([
            "project-plans/coderabbit/analysis/missing-contract.md",
            "tests/fixtures/github_api_contract",
        ])
        .output()
        .expect("validator command should execute");

    assert!(!output.status.success(), "missing contract must fail");
}
