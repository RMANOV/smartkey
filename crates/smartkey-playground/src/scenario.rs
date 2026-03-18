use serde::Serialize;

/// A scheduled event that fires at a specific character offset during simulation.
#[derive(Debug, Clone, Serialize)]
pub enum ScheduledEvent {
    /// Simulate language switch: fires focus_lost + focus_gained on the engine.
    SwitchLanguage { char_offset: usize, lang: String },
    /// Type a wrong character, then backspace, then correct character.
    InjectTypo {
        char_offset: usize,
        wrong_char: char,
    },
    /// Pause: fires focus_lost + focus_gained to reset engine context.
    Pause { char_offset: usize },
}

impl ScheduledEvent {
    pub fn char_offset(&self) -> usize {
        match self {
            Self::SwitchLanguage { char_offset, .. } => *char_offset,
            Self::InjectTypo { char_offset, .. } => *char_offset,
            Self::Pause { char_offset, .. } => *char_offset,
        }
    }
}

/// A typing scenario: target text + optional scheduled events.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub text: String,
    pub events: Vec<ScheduledEvent>,
}

impl Scenario {
    /// Common English sentences — tests basic prediction quality.
    pub fn english_prose() -> Self {
        Self {
            name: "English Prose".into(),
            text: concat!(
                "The quick brown fox jumps over the lazy dog. ",
                "She sells seashells by the seashore. ",
                "How much wood would a woodchuck chuck. ",
                "The weather is beautiful today and the sun is shining. ",
                "I have been working on this project for several months. ",
                "Please let me know if you have any questions about this. ",
                "We should schedule a meeting to discuss the next steps. ",
                "The report will be ready by the end of the week.",
            )
            .into(),
            events: vec![],
        }
    }

    /// Conversational Bulgarian — direct Cyrillic characters.
    pub fn bulgarian_chat() -> Self {
        Self {
            name: "Bulgarian Chat".into(),
            text: concat!(
                "Здравей как си днес. ",
                "Много хубаво време е навън. ",
                "Искам да отида на разходка в парка. ",
                "Какво ще правиш тази вечер. ",
                "Трябва да свърша работата до петък. ",
                "Благодаря за помощта ти.",
            )
            .into(),
            events: vec![],
        }
    }

    /// Alternating English/Bulgarian with language switch events.
    pub fn mixed_language() -> Self {
        let text = concat!(
            "Hello how are you doing today. ",
            "Здравей много добре съм. ",
            "That is great to hear from you. ",
            "Да наистина е хубаво. ",
            "Let us meet tomorrow morning. ",
            "Добре ще се видим утре.",
        );

        // Switch at approximate word boundaries between language segments
        let en1_end = "Hello how are you doing today. ".chars().count();
        let bg1_end = en1_end + "Здравей много добре съм. ".chars().count();
        let en2_end = bg1_end + "That is great to hear from you. ".chars().count();
        let bg2_end = en2_end + "Да наистина е хубаво. ".chars().count();
        let en3_end = bg2_end + "Let us meet tomorrow morning. ".chars().count();

        Self {
            name: "Mixed Language".into(),
            text: text.into(),
            events: vec![
                ScheduledEvent::SwitchLanguage {
                    char_offset: en1_end,
                    lang: "bg".into(),
                },
                ScheduledEvent::SwitchLanguage {
                    char_offset: bg1_end,
                    lang: "en".into(),
                },
                ScheduledEvent::SwitchLanguage {
                    char_offset: en2_end,
                    lang: "bg".into(),
                },
                ScheduledEvent::SwitchLanguage {
                    char_offset: bg2_end,
                    lang: "en".into(),
                },
                ScheduledEvent::SwitchLanguage {
                    char_offset: en3_end,
                    lang: "bg".into(),
                },
            ],
        }
    }

    /// Programming keywords + technical vocabulary.
    pub fn code_sprint() -> Self {
        Self {
            name: "Code Sprint".into(),
            text: concat!(
                "function async await promise callback. ",
                "struct impl trait derive clone. ",
                "select from where join group order. ",
                "docker kubernetes container deployment. ",
                "algorithm binary search tree graph. ",
                "interface abstract override virtual. ",
                "compile build test debug release.",
            )
            .into(),
            events: vec![],
        }
    }

    /// Fast typing with typos and pauses — robustness test.
    pub fn stress_test() -> Self {
        Self {
            name: "Stress Test".into(),
            text: concat!(
                "The implementation of the new feature requires careful testing. ",
                "Performance optimization should be measured with benchmarks. ",
                "Error handling must cover all edge cases gracefully. ",
                "Documentation should be clear and up to date always.",
            )
            .into(),
            events: vec![
                ScheduledEvent::InjectTypo {
                    char_offset: 8,
                    wrong_char: 'x',
                },
                ScheduledEvent::Pause { char_offset: 30 },
                ScheduledEvent::InjectTypo {
                    char_offset: 55,
                    wrong_char: 'q',
                },
                ScheduledEvent::Pause { char_offset: 80 },
                ScheduledEvent::InjectTypo {
                    char_offset: 120,
                    wrong_char: 'z',
                },
                ScheduledEvent::Pause { char_offset: 150 },
            ],
        }
    }

    /// English text with proper nouns and sentence-start capitals.
    pub fn capitalization_prose() -> Self {
        Self {
            name: "Capitalization Prose".into(),
            text: concat!(
                "London is a beautiful city in the United Kingdom. ",
                "Sarah and Michael went to the park yesterday. ",
                "The United States declared independence in July. ",
                "Professor Smith teaches at Oxford University. ",
                "Amazon and Google are major technology companies. ",
                "The Eiffel Tower is located in Paris France.",
            )
            .into(),
            events: vec![],
        }
    }

    /// All-caps technical text.
    pub fn all_caps_typing() -> Self {
        Self {
            name: "All Caps Typing".into(),
            text: concat!(
                "HTTP REQUEST FAILED WITH ERROR CODE. ",
                "WARNING DO NOT DELETE THIS FILE. ",
                "API KEY MUST BE SET BEFORE RUNNING. ",
                "DNS LOOKUP TIMEOUT AFTER RETRY. ",
                "SSL CERTIFICATE EXPIRED ON SERVER.",
            )
            .into(),
            events: vec![],
        }
    }

    /// Mixed EN/BG text WITHOUT explicit SwitchLanguage events.
    /// Tests auto language detection from character input.
    pub fn auto_language_detection() -> Self {
        Self {
            name: "Auto Language Detection".into(),
            text: concat!(
                "Hello how are you doing today. ",
                "Здравей много добре съм благодаря. ",
                "The weather is really nice outside. ",
                "Искам да отида на разходка в парка. ",
                "Let us meet tomorrow for coffee. ",
                "Добре ще се видим утре сутринта.",
            )
            .into(),
            events: vec![], // No language switch events — purely auto-detected
        }
    }

    /// Long-form common English sentences for high precision measurement.
    pub fn high_precision_english() -> Self {
        Self {
            name: "High Precision English".into(),
            text: concat!(
                "I have been working on this project for several months now. ",
                "The results show a significant improvement in performance. ",
                "We need to schedule a meeting to discuss the next steps. ",
                "Please review the document and provide your feedback. ",
                "The team has made excellent progress this quarter. ",
                "Our goal is to deliver the final version by next month. ",
                "Thank you for your continued support and dedication. ",
                "The new feature will be available in the next release.",
            )
            .into(),
            events: vec![],
        }
    }

    /// All preset scenarios.
    pub fn all_presets() -> Vec<Self> {
        vec![
            Self::english_prose(),
            Self::bulgarian_chat(),
            Self::mixed_language(),
            Self::code_sprint(),
            Self::stress_test(),
            Self::capitalization_prose(),
            Self::all_caps_typing(),
            Self::auto_language_detection(),
            Self::high_precision_english(),
        ]
    }
}
