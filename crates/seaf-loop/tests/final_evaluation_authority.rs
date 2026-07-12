use seaf_loop::load_verified_final_evaluation_authority;
use seaf_loop::recovery::{
    EvaluationRecoveryAttemptV2, EvaluationRecoveryReportDisposition, EvaluationRecoverySourceRunV2,
};

#[test]
fn final_authority_loader_is_an_explicit_inert_api() {
    let _loader = load_verified_final_evaluation_authority;
}

#[test]
fn evaluation_recovery_v2_authority_types_are_available_to_fixture_builders() {
    assert!(std::mem::size_of::<EvaluationRecoverySourceRunV2>() > 0);
    assert!(std::mem::size_of::<EvaluationRecoveryAttemptV2>() > 0);
    assert!(std::mem::size_of::<EvaluationRecoveryReportDisposition>() > 0);
}
