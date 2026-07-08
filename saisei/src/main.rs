//! saisei launcher binary — thin wrapper over the `saisei` library crate so the
//! command logic (and its helpers) stay unit-testable from integration tests.

fn main() {
    saisei::run();
}
