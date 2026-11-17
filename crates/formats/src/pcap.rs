//! Shared saved-capture and protocol inspection. File admission and evidence policy remain local.
pub use craft_pcap::{
    Capture, Count, Decoded, Dns, DnsAnswer, DnsQuestion, Flow, FlowKey, FlowTable, Frame, Http, Interface, IpInfo, MAX_FLOWS, MAX_INVENTORY, MAX_PACKETS,
    Summary, TlsHello, Transport, decode, endpoint, ip_protocol_name, is_capture, iso8601, link_name, qtype_name, strip_userinfo,
};

impl From<craft_pcap::PcapError> for crate::FormatError {
    fn from(error: craft_pcap::PcapError) -> Self {
        match error {
            craft_pcap::PcapError::Truncated(reason) => Self::Truncated(reason),
            craft_pcap::PcapError::Malformed(reason) => Self::Malformed(reason),
            craft_pcap::PcapError::Unsupported(reason) => Self::Unsupported(reason),
        }
    }
}

pub fn summarize(data: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Summary, crate::FormatError> {
    craft_pcap::summarize(data, cancelled).map_err(Into::into)
}

#[cfg(test)]
#[path = "pcap_legacy.rs"]
mod legacy;

#[cfg(test)]
mod parity_tests {
    use super::*;
    #[test]
    fn frames_protocols_summaries_and_errors_match() {
        let frames = craft_anything_testkit::pcap::sample_frames();
        for data in [craft_anything_testkit::pcap::pcap(&frames), craft_anything_testkit::pcap::pcapng(&frames), vec![], b"not a capture".to_vec()] {
            // Keep the retained admission oracle exercised, including truncated magic bytes.
            for end in 0..=data.len() {
                assert_eq!(legacy::is_capture(&data[..end]), is_capture(&data[..end]));
            }
            assert_eq!(format!("{:?}", legacy::summarize(&data, &|| false)), format!("{:?}", summarize(&data, &|| false)));
            assert_eq!(format!("{:?}", legacy::summarize(&data, &|| true)), format!("{:?}", summarize(&data, &|| true)));
            match (legacy::Capture::new(&data), Capture::new(&data)) {
                (Ok(old), Ok(new)) => {
                    let a: Vec<_> = old.collect();
                    let b: Vec<_> = new.collect();
                    assert_eq!(format!("{a:?}"), format!("{b:?}"));
                }
                (Err(a), Err(b)) => assert_eq!(a, crate::FormatError::from(b)),
                _ => panic!("capture parity mismatch"),
            }
        }
        for frame in frames {
            assert_eq!(serde_json::to_value(legacy::decode(1, &frame)).unwrap(), serde_json::to_value(decode(1, &frame)).unwrap());
        }
    }
}
