use std::path::PathBuf;

use clap::Parser;
use smartkey_playground::report::{confidence_histogram, print_histogram, print_summary_table};
use smartkey_playground::{Playground, Scenario};

#[derive(Parser)]
#[command(
    name = "smartkey-eval",
    about = "SmartKey prediction quality evaluator"
)]
struct Cli {
    /// Run only this preset (english-prose, bulgarian-chat, mixed-language, code-sprint, stress-test, capitalization-prose, all-caps-typing, auto-language-detection, high-precision-english)
    #[arg(long)]
    preset: Option<String>,

    /// Print full event timeline
    #[arg(long)]
    verbose: bool,

    /// Print confidence histogram (calibration view)
    #[arg(long)]
    histogram: bool,

    /// Output JSON instead of human-readable text
    #[arg(long)]
    json: bool,

    /// Acceptance confidence threshold (default: 0.2)
    #[arg(long, default_value_t = 0.2)]
    threshold: f64,

    /// Path to corpus directory
    #[arg(long)]
    corpus_dir: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();

    let scenarios: Vec<Scenario> = if let Some(ref name) = cli.preset {
        match name.as_str() {
            "english-prose" => vec![Scenario::english_prose()],
            "bulgarian-chat" => vec![Scenario::bulgarian_chat()],
            "mixed-language" => vec![Scenario::mixed_language()],
            "code-sprint" => vec![Scenario::code_sprint()],
            "stress-test" => vec![Scenario::stress_test()],
            "capitalization-prose" => vec![Scenario::capitalization_prose()],
            "all-caps-typing" => vec![Scenario::all_caps_typing()],
            "auto-language-detection" => vec![Scenario::auto_language_detection()],
            "high-precision-english" => vec![Scenario::high_precision_english()],
            other => {
                log::error!(
                    "Unknown preset: {:?}. Available: english-prose, bulgarian-chat, \
                     mixed-language, code-sprint, stress-test, capitalization-prose, \
                     all-caps-typing, auto-language-detection, high-precision-english",
                    other
                );
                std::process::exit(1);
            }
        }
    } else {
        Scenario::all_presets()
    };

    let mut builder = Playground::new().accept_threshold(cli.threshold);

    if let Some(ref dir) = cli.corpus_dir {
        builder = builder.corpus_dir(dir);
    }

    for s in &scenarios {
        builder = builder.scenario(s.clone());
    }

    let reports = builder.run();

    if cli.json {
        let json = serde_json::to_string_pretty(&reports).unwrap();
        println!("{}", json);
    } else {
        if reports.len() > 1 {
            print_summary_table(&reports);
        }

        for report in &reports {
            report.print_summary();
            if cli.histogram {
                let hist = confidence_histogram(&report.timeline);
                print_histogram(&report.scenario_name, &hist);
            }
            if cli.verbose {
                report.print_timeline();
            }
        }
    }
}
