// Shared test helpers. Included via `mod support;` in integration test files;
// not compiled as its own test binary (it lives in a subdirectory).
// These are shared fixtures: a given test binary uses only part of the surface
// it pulls in, so unused-item warnings here are expected rather than a defect.
#[allow(dead_code)]
pub mod event_layer_seeder;
#[allow(dead_code)]
pub mod stub_peer;
