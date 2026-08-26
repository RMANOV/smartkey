pub mod bg_morphology;
pub mod bpe;
pub mod cache;
pub mod calibration;
pub mod caps;
pub mod collocation;
pub mod context_sampler;
pub mod corpus;
pub mod correction_memory;
pub mod cvm;
pub mod dual_buffer;
pub mod ensemble;
pub mod eval;
pub mod ffi_protocol;
pub mod frustration;
pub mod hedge;
pub mod input;
pub mod keymap;
pub mod kneser_ney;
pub mod lang_cvm;
pub mod lang_detect;
pub mod lang_model;
pub mod light_profile;
pub mod markov;
pub mod master_loop;
pub mod ngram;
pub mod o1_shim;
pub mod paths;
pub mod personal;
pub mod ppm;
pub mod rejection_memory;
pub mod reranker;
pub mod session_cache;
pub mod tech_vocab;
pub mod typing_regime;

// F4 Phase R (ruling 221af6d0a1c8): lane RED-test modules live in separate
// files so parallel test authors never edit the same file.
#[cfg(test)]
mod f4_lane_ab_tests;
#[cfg(test)]
mod f4_lane_cde_tests;

pub use ensemble::SmartKeyEngine;
pub use input::InputMethodCore;
pub use master_loop::MasterLoop;
