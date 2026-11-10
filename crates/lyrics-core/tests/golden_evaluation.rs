use lyrics_core::golden::{
    build_golden_set, evaluate_golden_set, format_evaluation_report, GoldenCategory,
    GoldenThresholds,
};

#[test]
fn test_golden_set_regression_gate() {
    let tracks = build_golden_set();
    assert_eq!(tracks.len(), 33, "Golden set must contain exactly 33 tracks");

    let thresholds = GoldenThresholds::default();
    let report = evaluate_golden_set(&tracks, &thresholds);

    let markdown = format_evaluation_report(&report);
    eprintln!("\n{}\n", markdown);

    assert!(
        report.passed_ci_gate,
        "CI Regression Gate FAILED! Thresholds regressed:\n{:#?}",
        report.regression_failures
    );
}

#[test]
fn test_golden_set_category_coverage() {
    let tracks = build_golden_set();

    let all_categories = [
        GoldenCategory::CorrectTags,
        GoldenCategory::WrongTags,
        GoldenCategory::SwappedArtistTitle,
        GoldenCategory::NoisyTitle,
        GoldenCategory::CorrectLrc,
        GoldenCategory::WrongSongLrc,
        GoldenCategory::OffsetPlus15,
        GoldenCategory::OffsetMinus15,
        GoldenCategory::OffsetPlus30,
        GoldenCategory::OffsetMinus30,
        GoldenCategory::PlainOnly,
        GoldenCategory::Instrumental,
    ];

    for cat in &all_categories {
        let count = tracks
            .iter()
            .filter(|t| t.categories.contains(cat))
            .count();
        assert!(
            count >= 2,
            "Category {:?} has insufficient representation: found {}",
            cat,
            count
        );
    }
}
