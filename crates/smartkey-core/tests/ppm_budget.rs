//! An expert gated to zero must not change another expert's ranking budget.
//!
//! This exercises the public ensemble directly: Core's PPM-dependent minimum
//! prefix length and client presentation policy are outside this contract.

use smartkey_core::ensemble::Prediction;
use smartkey_core::input::InputConfig;
use smartkey_core::lang_detect::LangId;
use smartkey_core::SmartKeyEngine;

fn null_ppm_pool(use_ppm: bool, prefix: &str) -> Vec<Prediction> {
    let config = InputConfig {
        use_ppm,
        cvm_decay_lambda: 0.0, // Same clock-free CVM observation in both arms.
        ..InputConfig::default()
    };
    let mut engine = SmartKeyEngine::from_config(&config);
    engine.learn_oov_word("axa", LangId::En);
    engine.learn_oov_word("axb", LangId::En);
    engine.learn("axa");
    engine.learn_online_markov(Some("ctx"), None, "axb");

    // These public producers leave the candidates unconfirmed and train only
    // the personal CVM/Markov layers. They do not train the corpus PPM model.
    let profile = engine.export_personal_profile();
    assert!(profile.confirmed_words.is_empty());
    assert_eq!(profile.cvm.words, vec![String::from("axa")]);
    assert_eq!(
        profile.markov_bigrams,
        vec![(String::from("ctx"), String::from("axb"), 1_u32)]
    );
    assert!(profile.markov_trigrams.is_empty());

    engine.predict(prefix, &["ctx"], 2, Some(LangId::En))
}

#[test]
fn ppm_gated_zero_pool_does_not_change_other_experts_budget() {
    for prefix in ["a", "ax"] {
        let off = null_ppm_pool(false, prefix);
        let on = null_ppm_pool(true, prefix);
        for (label, arm) in [("PPM off", &off), ("PPM on", &on)] {
            assert_eq!(arm.len(), 2, "{label}: preserve the whole candidate pool");
            let mut words: Vec<&str> = arm.iter().map(|p| p.word.as_str()).collect();
            words.sort_unstable();
            assert_eq!(words, ["axa", "axb"], "{label}: same OOV candidates");
            assert!(arm
                .iter()
                .all(|p| { p.score.is_finite() && p.score > 0.0 && p.confidence.is_finite() }));
            eprintln!("prefix={prefix}, {label}: {arm:?}");
        }

        // The hand-derived default Markov/CVM fixture prefers the recorded
        // bigram. An inactive expert must preserve scores as well as ordering.
        assert_eq!(off[0].word, "axb", "baseline Markov preference");
        for (with_ppm, without_ppm) in on.iter().zip(&off) {
            assert_eq!(
                with_ppm.word, without_ppm.word,
                "an expert normalized to zero for the unchanged pool must not borrow Markov weight"
            );
            assert_eq!(with_ppm.score.to_bits(), without_ppm.score.to_bits());
            assert_eq!(
                with_ppm.confidence.to_bits(),
                without_ppm.confidence.to_bits()
            );
        }
    }
}

#[test]
fn ppm_trained_corpus_still_contributes_to_an_unchanged_pool() {
    let predict = |use_ppm| {
        let config = InputConfig {
            use_ppm,
            cvm_decay_lambda: 0.0,
            ..InputConfig::default()
        };
        let mut engine = SmartKeyEngine::from_config(&config);
        // Equal corpus frequencies leave the baseline candidates tied. The
        // character corpus makes the repeated-a continuation more predictable.
        engine.load_word_lang("aaaa", 100, LangId::En);
        engine.load_word_lang("abbb", 100, LangId::En);
        engine.predict("a", &[], 2, Some(LangId::En))
    };
    let off = predict(false);
    let on = predict(true);
    for arm in [&off, &on] {
        assert_eq!(arm.len(), 2);
        let mut words: Vec<&str> = arm.iter().map(|p| p.word.as_str()).collect();
        words.sort_unstable();
        assert_eq!(words, ["aaaa", "abbb"]);
    }
    eprintln!("trained PPM off: {off:?}");
    eprintln!("trained PPM on: {on:?}");
    assert_eq!(off[0].score.to_bits(), off[1].score.to_bits());
    assert_eq!(on[0].word, "aaaa");
    assert!(on[0].score > on[1].score);
    assert!(on[0].confidence > on[1].confidence);
    assert!(on.iter().any(|candidate| {
        let baseline = off.iter().find(|p| p.word == candidate.word).unwrap();
        candidate.score.to_bits() != baseline.score.to_bits()
    }));
}
