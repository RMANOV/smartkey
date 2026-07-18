# O1 / PHASE-A RECEIPT 5 — EFFECTIVE SAFE FEATURE-FLAG / CONFIG MANIFEST

generated: 2026-07-18T21:50:41+03:00   worktree HEAD: 47acd8a12ec16b9e5aeb60a63f4bf43550f04405
Shows the effective config/flags the harness runs with — nothing points at live data.

## Environment (effective)
```
SMARTKEY_PHASEA_DATA = /tmp/claude-1000/-home-rmanov-smartkey-phase-a-lab/179f182b-c6aa-4af8-b4ef-df37d19ebd44/scratchpad/o1-phasea-data   <- scratch dir (NOT ~/smartkey, NOT ~/.config)
PYTHONPATH           = /home/rmanov/smartkey-o1-lab   <- the o1-lab worktree only
SMARTKEY_PHASEA_DB   = <unset -> data_dir()/events.db under scratch>
SMARTKEY_PHASEA_PMODEL     = <unset -> bigram+unigram-backoff>
SMARTKEY_PHASEA_MAX_BIGRAMS= <unset -> no cap>
SMARTKEY_CORPUS_DIR        = <unset; IGNORED — harness reads pinned lab_corpus/ only>
interpreter          = /usr/bin/python3  (Python 3.14.4)   <- system python, NOT the live venv
```

## Frozen constants the gate runs with (phase_a/constants.py)
```
PREDICTION_SCOPE           = 'ngram_component'
RESOLVER                   = 'script:next_token_in_top3'
EVENT_CLASS                = 'machine'
ALLOWED_RESOLVER_PREFIXES  = ('script:', 'sql:')
LATENCY_BUDGET_US          = 20000
GATE_PASS_GAP              = 0.05
GATE_FAIL_GAP              = 0.1
GATE_FAIL_BUCKETS          = 3
MIN_RESOLUTIONS            = 5000
MAX_UNRESOLVED_FRAC        = 0.05
MAX_OVER_BUDGET_FRAC       = 0.01
N_QUANTILE_BUCKETS         = 5
MIN_BUCKET_RESOLUTIONS     = 30
MIN_SURVIVING_BUCKETS      = 2
FIT_FRACTION               = 0.5
WATCHDOG_MAX_AGE_H         = 48
VOLUME_WINDOW_DAYS         = 14
HARNESS_VERSION            = 'o1-phase-a-harness/1.0.0'
SPEC_SHA256                = '48251199ca6fcad07cbda2cbc1d4db2ab7bf7077512641bc6a49290053f7b319'
```

## Corpus source (pinned; the ONLY inputs)
```
08b1440a6d542d68ff1a1e80d9478292e240531272e8632522bf40a6fc3fcb1c  corpus_bg.bin
087136b9e3b5cb6e7750b31e8837e32b2ea7e92b88ffbed227329f2b82700fb3  corpus_bg.json
609c90439257f68794a28224da53999ce63d82589b25800b24b494dcb7afccde  corpus_en.bin
5a59e842c5d58e9797790c4ad2921233c45679dccab0cbfbfa9256e4365f7445  corpus_en.json
77b57593eab7f3c884fd3c3f332fbd7add9bb8e3e131fdcb8e44631b940925b6  corpus_tech.bin
5af923c90a9e8dfb5011bcaa4c1a0b11c7e58e7770d6a496474d328b24fb5845  corpus_tech.json
```

## Isolation posture
```
git remote -v (push disabled — isolation is plumbing, not will):
origin	git@github.com:RMANOV/smartkey.git (fetch)
origin	DISABLED (push)

measured-path modules importing ensemble/PyO3 (import/from lines only): NONE
  scanned: constants, paths, freqmodel, harness, analyze, sweep, corpus_replay
  (docstrings NAME the excluded stages to document the firewall; no code imports them.)
  verified structurally by phase_a.selftest check 9 and tests/test_o1_phasea.py.
```
