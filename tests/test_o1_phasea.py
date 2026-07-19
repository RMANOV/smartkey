"""Pytest surface for the O1 / Phase-A n-gram calibration harness.

Runs every machinery self-test check (GREEN->PASS, injected RED->FAIL, each
FAIL-clause, the schema invariant, sweep/watchdog, the scope firewall, corpus
pin, and the live-tree data guard) plus focused unit assertions. All artifacts
go to a per-session tmp dir via SMARTKEY_PHASEA_DATA; the pinned lab_corpus is
read-only and sha256-verified by the harness itself.
"""

from __future__ import annotations

import os

import pytest


@pytest.fixture(scope="session", autouse=True)
def _isolated_data_dir(tmp_path_factory):
    d = tmp_path_factory.mktemp("o1-phasea-data")
    old = os.environ.get("SMARTKEY_PHASEA_DATA")
    os.environ["SMARTKEY_PHASEA_DATA"] = str(d)
    yield
    if old is not None:
        os.environ["SMARTKEY_PHASEA_DATA"] = old
    else:
        os.environ.pop("SMARTKEY_PHASEA_DATA", None)


from phase_a import selftest  # noqa: E402  (after fixture so env is ready at call time)
from phase_a.constants import PREDICTION_SCOPE  # noqa: E402
from phase_a.freqmodel import FreqModel  # noqa: E402
from phase_a.paths import lab_corpus_files  # noqa: E402


@pytest.mark.parametrize("name,fn", selftest.CHECKS, ids=[c[0] for c in selftest.CHECKS])
def test_selftest_check(name, fn):
    ok, detail = fn()
    assert ok, f"{name}: {detail}"


def test_prediction_scope_is_ngram_component():
    assert PREDICTION_SCOPE == "ngram_component"


def test_freqmodel_top3_is_pure_ngram_ranking():
    """top3(ctx) must be the raw-count-ranked followers — no ensemble anywhere."""
    model = FreqModel.load(lab_corpus_files("json"))
    # find a context with >=4 followers so ranking is observable
    ctx = next(c for c, slot in model.bigrams.items() if len(slot) >= 4)
    top3 = model.top3(ctx)
    followers = model.followers(ctx)
    expected = [w for w, _ in sorted(followers.items(), key=lambda kv: (-kv[1], kv[0]))[:3]]
    assert top3 == expected
    # p_top3 equals the summed conditional mass of exactly those three
    total = model.bigram_totals[ctx]
    manual = sum(followers[w] for w in top3) / total
    assert abs(model.p_top3(ctx, top3) - manual) < 1e-9


def test_no_ensemble_or_pyo3_in_measured_path():
    ok, detail = selftest.check_scope_firewall_static()
    assert ok, detail


def test_corpus_pins_match_expected():
    ok, detail = selftest.check_corpus_pin()
    assert ok, detail
