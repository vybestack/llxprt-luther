//! Components: concrete step executor implementations.
//!
//! Split by dependency direction rather than by subject matter. `generic`
//! holds components that carry no knowledge of software change, version
//! control, or any external service, and depend on nothing beyond the
//! executor contract in `engine::error` and `engine::executor`.
//!
//! They previously lived under `engine::executors`, which made them part of
//! the engine by construction: anything depending on the engine got them, and
//! any dependency they acquired became an engine dependency.

pub mod generic;
pub mod software_change;
