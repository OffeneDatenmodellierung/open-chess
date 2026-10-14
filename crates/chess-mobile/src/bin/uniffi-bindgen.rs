//! Writes the Swift bindings: `scripts/build-xcframework.sh` runs it.

fn main() {
    uniffi::uniffi_bindgen_main();
}
