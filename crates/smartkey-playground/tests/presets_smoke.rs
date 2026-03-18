use smartkey_playground::{Playground, Scenario};

fn run_preset(scenario: Scenario) -> smartkey_playground::TypingReport {
    let reports = Playground::new()
        .scenario(scenario)
        .accept_threshold(0.3)
        .run();
    assert_eq!(reports.len(), 1);
    reports.into_iter().next().unwrap()
}

#[test]
fn smoke_english_prose() {
    let report = run_preset(Scenario::english_prose());
    assert!(report.words_total > 0, "should have words");
    assert!(report.total_chars > 0, "should have chars");
    assert!(report.total_keystrokes > 0, "should have keystrokes");
}

#[test]
fn smoke_bulgarian_chat() {
    let report = run_preset(Scenario::bulgarian_chat());
    assert!(report.words_total > 0);
    assert!(report.total_chars > 0);
}

#[test]
fn smoke_mixed_language() {
    let report = run_preset(Scenario::mixed_language());
    assert!(report.words_total > 0);
    assert!(report.total_chars > 0);
}

#[test]
fn smoke_code_sprint() {
    let report = run_preset(Scenario::code_sprint());
    assert!(report.words_total > 0);
    assert!(report.total_chars > 0);
}

#[test]
fn smoke_stress_test() {
    let report = run_preset(Scenario::stress_test());
    assert!(report.words_total > 0);
    assert!(report.total_chars > 0);
}

#[test]
fn smoke_run_all_presets() {
    let reports = Playground::run_all_presets();
    assert_eq!(reports.len(), 9, "should have 9 preset reports");
    for r in &reports {
        assert!(r.words_total > 0, "{}: should have words", r.scenario_name);
    }
}

#[test]
fn english_prose_has_some_acceptance() {
    let report = run_preset(Scenario::english_prose());
    // With real corpus loaded, we expect at least some predictions to be accepted.
    // Use a very low bar — even 1 accepted word means the engine is producing useful ghosts.
    assert!(
        report.words_predicted > 0,
        "english prose should have measurable acceptance (got {} predicted out of {})",
        report.words_predicted,
        report.words_total
    );
}

#[test]
fn json_output_is_valid() {
    let report = run_preset(Scenario::english_prose());
    let json = report.to_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("should be valid JSON");
    assert!(parsed.get("scenario_name").is_some());
    assert!(parsed.get("total_chars").is_some());
    assert!(parsed.get("accept_rate").is_some());
}
