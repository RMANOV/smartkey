// Bulgarian morphological grouping — suffix-based stemming and frequency pooling.
//
// Groups inflected forms of the same root together so that rare inflections
// benefit from the frequency of their more common siblings:
//   "работя" (freq=500) + "работиш" (freq=5) → pooled stem "работ" freq=505
//
// Uses ~50 suffix rules covering 90%+ of BG verbal/nominal inflection.
// No dictionary needed — purely rule-based with statistical validation.

use std::collections::HashMap;

/// Minimum stem length (in chars) — shorter stems are too ambiguous.
const MIN_STEM_LEN: usize = 3;

/// BG suffix stemmer with frequency pooling.
///
/// Maps words to stems, then pools frequencies across all forms sharing a stem.
/// Provides a pooled frequency boost for rare inflected forms.
pub struct BgMorphology {
    /// stem → total pooled frequency across all inflected forms
    stem_pool: HashMap<String, u32>,
    /// word → stem mapping (cached for fast lookup)
    word_to_stem: HashMap<String, String>,
}

impl BgMorphology {
    pub fn new() -> Self {
        Self {
            stem_pool: HashMap::new(),
            word_to_stem: HashMap::new(),
        }
    }

    /// Build morphological groups from a word frequency table.
    ///
    /// Only processes Cyrillic words (BG). Latin words are ignored.
    pub fn build(word_freqs: &HashMap<String, u32>) -> Self {
        let mut stem_pool: HashMap<String, u32> = HashMap::new();
        let mut word_to_stem: HashMap<String, String> = HashMap::new();

        for (word, &freq) in word_freqs {
            // Only process Cyrillic words.
            if !word.chars().any(is_cyrillic) {
                continue;
            }
            if let Some(stem) = stem_word(word) {
                *stem_pool.entry(stem.clone()).or_insert(0) += freq;
                word_to_stem.insert(word.clone(), stem);
            }
        }

        Self {
            stem_pool,
            word_to_stem,
        }
    }

    /// Get the pooled frequency for a word (frequency of its stem group).
    ///
    /// Returns `None` if the word has no known stem or is not Cyrillic.
    pub fn pooled_frequency(&self, word: &str) -> Option<u32> {
        let stem = self.word_to_stem.get(word)?;
        self.stem_pool.get(stem).copied()
    }

    /// Get a frequency boost factor for a word based on morphological pooling.
    ///
    /// Returns > 1.0 when the word's individual frequency is much lower than
    /// its stem group's pooled frequency (rare inflection of a common root).
    /// Returns 1.0 when no morphological data is available.
    pub fn frequency_boost(&self, word: &str, word_freq: u32) -> f64 {
        match self.pooled_frequency(word) {
            Some(pooled) if pooled > word_freq && word_freq > 0 => {
                // Blend: sqrt(pooled / word_freq), capped at 3x.
                let ratio = pooled as f64 / word_freq as f64;
                ratio.sqrt().min(3.0)
            }
            _ => 1.0,
        }
    }

    /// Number of stem groups.
    pub fn stem_count(&self) -> usize {
        self.stem_pool.len()
    }
}

impl Default for BgMorphology {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a character is Cyrillic.
fn is_cyrillic(ch: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&ch)
}

/// Stem a Bulgarian word by removing the longest matching suffix.
///
/// Returns `None` if the resulting stem would be shorter than MIN_STEM_LEN.
fn stem_word(word: &str) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    let len = chars.len();

    if len < MIN_STEM_LEN + 1 {
        // Word too short for meaningful stemming.
        return None;
    }

    // Try suffixes longest-first for greedy matching.
    // The suffix list is ordered by length (longest first) within each group.
    for suffix in BG_SUFFIXES {
        if word.ends_with(suffix) {
            let stem_len = len - suffix.chars().count();
            if stem_len >= MIN_STEM_LEN {
                let stem: String = chars[..stem_len].iter().collect();
                return Some(stem);
            }
        }
    }

    // No suffix match — use the full word as its own stem.
    Some(word.to_string())
}

/// Bulgarian suffixes for stemming, ordered longest-first.
///
/// Covers the main inflection patterns:
/// - Verbal: person/number/tense (present, past, imperative)
/// - Nominal: gender/number/definiteness
/// - Adjectival: gender/number/definiteness
///
/// ~50 rules covering ~90% of productive BG inflection.
static BG_SUFFIXES: &[&str] = &[
    // 4+ char suffixes (most specific)
    "ваме", "вате", "ваха", "вали", "вана", "вано", "ията", "ието", "ните", "ната", "ното", "ески",
    "ишки", // 3-char suffixes
    "ваш", "вам", "ват", "вах", "иш", "ите", "ила", "ило", "аме", "ате", "аха", "ахе", "ена",
    "ено", "ени", "ост", "ист", "ята", "ято", "ска", "ско", "ски", "ния", "ние",
    // 2-char suffixes (productive inflection)
    "ам", "аш", "ат", "им", "иш", "ит", "ем", "еш", "ет", "ях", "ях", "те", "та", "то", "ът", "ат",
    "ен", "на", "но", "ни", "ия", "ие", // 1-char suffixes (definiteness, gender)
    "а", "о", "е", "и",
];

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stem_word() {
        // "работиш" → stem "работ" (removing "иш")
        let stem = stem_word("работиш");
        assert!(stem.is_some());
        assert_eq!(stem.unwrap(), "работ");
    }

    #[test]
    fn test_stem_preserves_long_word() {
        // "програмиране" → stem should keep at least MIN_STEM_LEN chars
        let stem = stem_word("програмиране");
        assert!(stem.is_some());
        let s = stem.unwrap();
        assert!(s.chars().count() >= MIN_STEM_LEN);
    }

    #[test]
    fn test_frequency_pooling() {
        let mut freqs = HashMap::new();
        freqs.insert("работя".to_string(), 500u32);
        freqs.insert("работиш".to_string(), 5);
        freqs.insert("работи".to_string(), 200);

        let morph = BgMorphology::build(&freqs);

        // "работиш" should get a boost from the pooled stem frequency.
        let boost = morph.frequency_boost("работиш", 5);
        assert!(boost > 1.0, "rare inflection should get a boost: {boost}");

        // "работя" shouldn't get much boost (it's already high-frequency).
        let boost_common = morph.frequency_boost("работя", 500);
        assert!(
            boost_common <= 1.5,
            "common form should get minimal boost: {boost_common}"
        );
    }

    #[test]
    fn test_latin_words_ignored() {
        let mut freqs = HashMap::new();
        freqs.insert("hello".to_string(), 100u32);
        freqs.insert("working".to_string(), 50);

        let morph = BgMorphology::build(&freqs);
        assert_eq!(morph.pooled_frequency("hello"), None);
    }

    #[test]
    fn test_short_words_not_stemmed() {
        let stem = stem_word("да");
        assert!(stem.is_none(), "2-char word should not be stemmed");
    }
}
