//! Source timing schema assertions without provider or database I/O.
use ai_platform_gateway::migration_agent::model_timing::*;
#[test]
fn fake_clock_components_are_additive_and_first_write_wins() {
    let mut timing = ModelPlaneTiming::start(1000.);
    timing.note_dispatch(1000.25);
    timing.note_first_frame(1001.75);
    timing.note_first_visible(1001.83);
    for ((_, value), expected) in timing
        .components()
        .into_iter()
        .zip([0.25, 1.5, 0.08, 0.33, 1.83])
    {
        assert!((value.unwrap() - expected).abs() <= 1e-9);
    }
    assert!(timing.identity_residual().unwrap() <= 1e-9);
    timing.note_dispatch(9999.);
    timing.note_first_frame(9999.);
    timing.note_first_visible(9999.);
    assert_eq!(timing.dispatched, Some(1000.25));
    assert_eq!(timing.first_frame, Some(1001.75));
    assert_eq!(timing.first_visible, Some(1001.83));
}
#[test]
fn missing_components_are_absent_rather_than_zero() {
    let mut timing = ModelPlaneTiming::start(10.);
    timing.note_dispatch(10.5);
    let c = timing.components();
    assert_eq!(c[0].1, Some(0.5));
    for (_, value) in &c[1..] {
        assert_eq!(*value, None)
    }
    assert_eq!(timing.identity_residual(), None);
}
#[test]
fn source_client_reconciliation_boundary_matrix() {
    for (server, client, expected) in [
        (None, Some(3.), false),
        (Some(3.), None, false),
        (Some(3.), Some(3.05), true),
        (Some(3.), Some(3.19), true),
        (Some(3.), Some(3.30), false),
        (Some(3.), Some(2.99), true),
        (Some(3.), Some(2.98), false),
        (Some(3.), Some(2.90), false),
        (Some(4.5), Some(4.70), true),
        (Some(4.5), Some(4.80), false),
    ] {
        assert_eq!(
            client_residual_within_tolerance(server, client),
            expected,
            "{server:?} {client:?}"
        );
    }
}
#[test]
fn real_monotonic_clock_identity_preserves_five_millisecond_bound() {
    let clock = std::time::Instant::now();
    let mut timing = ModelPlaneTiming::start(clock.elapsed().as_secs_f64());
    timing.note_dispatch(clock.elapsed().as_secs_f64());
    timing.note_first_frame(clock.elapsed().as_secs_f64());
    timing.note_first_visible(clock.elapsed().as_secs_f64());
    assert!(timing.identity_residual().unwrap() <= REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS);
}
