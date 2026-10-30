//! Local AI models: which ones exist ([`registry`]), how images become their tensors and their
//! outputs become masks ([`tensor`]), and, with the `runtime` feature, running them
//! ([`runtime`], pure-Rust ONNX inference on the CPU).
//!
//! Nothing here downloads or stores a model: the app shell does that (IndexedDB in the browser,
//! the config directory on desktop) and hands the bytes over. Models are never committed and
//! never built in; see `docs/ai-models.md` and `docs/decisions.md`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod registry;
#[cfg(feature = "runtime")]
pub mod runtime;
pub mod tensor;

pub use registry::{DataFile, MODELS, Model, Norm, Purpose, find};
pub use tensor::Tensor;

/// What can go wrong with a model. Every message is meant to be shown to the user as it is.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MlError {
    #[error("unknown AI model \"{0}\"")]
    UnknownModel(String),
    #[error("the {0} model is not installed")]
    NotInstalled(&'static str),
    #[error("the {name} model file is damaged or is not the expected file (its checksum differs); download it again")]
    Checksum { name: &'static str },
    #[error("the {name} model could not be loaded: {reason}")]
    Load { name: &'static str, reason: String },
    #[error("the {name} model failed to run: {reason}")]
    Run { name: &'static str, reason: String },
    #[error("the image has the wrong size for the {name} model ({got} values, {want} expected)")]
    Shape { name: &'static str, got: usize, want: usize },
    #[error("this build of PhotoCraft cannot run AI models itself")]
    NoRuntime,
}
