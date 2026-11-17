//! Shared read-only EVM inspection; file loading and evidence policy remain local.
pub use craft_evm::{Encoding, EvmFunction, EvmInterface, EvmMetadata, MAX_CODE, mnemonic};
impl From<craft_evm::EvmError> for crate::FormatError {
    fn from(error: craft_evm::EvmError) -> Self {
        match error {
            craft_evm::EvmError::Truncated(s) => Self::Truncated(s),
            craft_evm::EvmError::Malformed(s) => Self::Malformed(s),
            craft_evm::EvmError::Unsupported(s) => Self::Unsupported(s),
            craft_evm::EvmError::TooLarge(s) => Self::TooLarge(s),
        }
    }
}
pub fn decode_input(input: &[u8], enc: Encoding) -> Result<(Vec<u8>, &'static str), crate::FormatError> {
    craft_evm::decode_input(input, enc).map_err(Into::into)
}
pub fn analyze(code: &[u8]) -> Result<EvmInterface, crate::FormatError> {
    craft_evm::analyze(code).map_err(Into::into)
}
#[cfg(test)]
#[path = "evm_legacy.rs"]
mod legacy;
#[cfg(test)]
mod parity {
    use super::*;
    #[test]
    fn original_reports_errors_hex_and_truncations_match() {
        let (code, _) = craft_anything_testkit::formats::evm_runtime();
        let mut cases = craft_anything_testkit::mutations(&code);
        cases.extend((0..code.len()).map(|n| code[..n].to_vec()));
        cases.extend([vec![], vec![0xef, 0, 1], vec![0x7f; 40]]);
        for bytes in cases {
            assert_eq!(legacy::analyze(&bytes).map(|r| serde_json::to_value(r).unwrap()), analyze(&bytes).map(|r| serde_json::to_value(r).unwrap()));
            for (old, new) in [(legacy::Encoding::Auto, Encoding::Auto), (legacy::Encoding::Raw, Encoding::Raw), (legacy::Encoding::Hex, Encoding::Hex)] {
                assert_eq!(legacy::decode_input(&bytes, old), decode_input(&bytes, new));
            }
        }
        for bytes in [b"0x123".as_slice(), b"0xzz", b" 0x0012 ", b"0Xff", b""] {
            assert_eq!(legacy::decode_input(bytes, legacy::Encoding::Auto), decode_input(bytes, Encoding::Auto));
        }
        for op in 0..=255 {
            assert_eq!(legacy::mnemonic(op), mnemonic(op));
        }
    }
}
