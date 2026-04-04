use kley::preflight_test_support::{
    command_for_lsp_requirement, lsp_requirements, run_required_lsp_checks_with_runner,
    CommandOutput, FakeRunner,
};

#[test]
fn preflight_reports_each_missing_lsp_binary_by_name() {
    let entries = lsp_requirements()
        .iter()
        .map(|requirement| {
            (
                command_for_lsp_requirement(requirement),
                CommandOutput::failure(),
            )
        })
        .collect();
    let runner = FakeRunner::new(entries);
    let results = run_required_lsp_checks_with_runner(&runner);

    let reported_ids: Vec<&str> = results.iter().map(|result| result.id).collect();
    let expected_ids: Vec<&str> = lsp_requirements()
        .iter()
        .map(|requirement| requirement.id)
        .collect();

    assert_eq!(reported_ids, expected_ids);
}
