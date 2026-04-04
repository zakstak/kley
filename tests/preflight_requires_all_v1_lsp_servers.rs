use kley::preflight_test_support::{
    command_for_lsp_requirement, lsp_requirements, run_required_lsp_checks_with_runner,
    CommandOutput, FakeRunner,
};

#[test]
fn preflight_requires_all_v1_lsp_servers() {
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

    assert_eq!(results.len(), lsp_requirements().len());
    assert!(results.iter().all(|result| !result.success));
}
