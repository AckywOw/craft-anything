//! Shared WebAssembly inspection; this module preserves the app error contract.
use crate::FormatError;
pub use craft_wasm::{
    GlueScan, MAX_INPUT, MAX_ITEMS, ProducerField, ProducerValue, WasmCustom, WasmExport, WasmFunction, WasmGlobal, WasmImport, WasmMemory, WasmReport,
    WasmSection, WasmTable, WasmTag, WasmValidation, is_wasm, scan_glue, to_wat, validate,
};

pub fn inspect(data: &[u8]) -> Result<WasmReport, FormatError> {
    craft_wasm::inspect(data).map_err(|error| match error {
        craft_wasm::WasmError::Malformed(message) => FormatError::Malformed(message),
        craft_wasm::WasmError::TooLarge(message) => FormatError::TooLarge(message),
    })
}

#[cfg(test)]
#[path = "wasm_legacy.rs"]
mod legacy;

#[cfg(test)]
mod parity {
    use super::*;
    #[test]
    fn shared_reports_errors_and_text_match() {
        let sample = craft_anything_testkit::formats::wasm_module();
        let mut samples = craft_anything_testkit::mutations(&sample);
        samples.push(sample);
        samples.push(b"not wasm".to_vec());
        samples.push(b"\0asm\x0d\0\x01\0".to_vec());
        for data in samples {
            assert_eq!(is_wasm(&data), legacy::is_wasm(&data));
            assert_eq!(serde_json::to_value(validate(&data)).unwrap(), serde_json::to_value(legacy::validate(&data)).unwrap());
            match (legacy::inspect(&data), inspect(&data)) {
                (Ok(old), Ok(new)) => {
                    assert_eq!(serde_json::to_value(&old).unwrap(), serde_json::to_value(&new).unwrap());
                    let glue = craft_anything_testkit::formats::WASM_GLUE;
                    assert_eq!(
                        serde_json::to_value(legacy::scan_glue(glue, &old.imports)).unwrap(),
                        serde_json::to_value(scan_glue(glue, &new.imports)).unwrap()
                    );
                    for cap in [0, 1, 40, 4096] {
                        assert_eq!(legacy::to_wat(&data, cap), to_wat(&data, cap));
                    }
                }
                (Err(old), Err(new)) => assert_eq!(old, new),
                _ => panic!("WASM parity mismatch"),
            }
        }
    }
}
