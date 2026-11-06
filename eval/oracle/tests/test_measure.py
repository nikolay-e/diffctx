import measure


def test_a_round_fails_only_when_a_selected_language_measured_no_commit():
    assert measure.unmeasured_languages({"python": 7, "typescript": 3}) == []
    assert measure.unmeasured_languages({"python": 7, "typescript": 0}) == ["typescript"]
    assert measure.unmeasured_languages({"typescript": 0, "python": 0}) == ["python", "typescript"]
    assert measure.unmeasured_languages({}) == []
