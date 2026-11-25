use super::{verified_receiver_interface, RouteEvidence, TransferResult};
use serde::Serialize;
use std::collections::HashSet;

/// Indexes into a single transfer report. An adapter qualifies only after all its samples pass.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct ComputeRoutes {
    primary_ip: Option<usize>,
    additional_ip: Vec<usize>,
    bluetooth_control: Option<usize>,
}

pub(super) fn recommend(paths: &[TransferResult]) -> ComputeRoutes {
    let mut ip = Vec::new();
    let mut bluetooth_control = None;
    for (index, path) in paths.iter().enumerate() {
        let Some(adapter) = verified_receiver_interface(path) else { continue };
        match path.route_evidence {
            Some(RouteEvidence::Tcp { .. }) => {
                if let Some(median_ms) = path.median_probe_ms {
                    ip.push((median_ms, index, adapter));
                }
            }
            Some(RouteEvidence::BluetoothRfcomm { .. }) if bluetooth_control.is_none() => bluetooth_control = Some(index),
            _ => {}
        }
    }
    ip.sort_unstable();
    let mut adapters = HashSet::new();
    let mut distinct = ip.into_iter().filter_map(|(_, index, adapter)| adapters.insert(adapter).then_some(index));
    ComputeRoutes {
        primary_ip: distinct.next(),
        additional_ip: distinct.collect(),
        bluetooth_control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{transfer_receipt, TRIAL_CHUNKS, TRIAL_CHUNK_BYTES};

    fn path(receiver: RouteEvidence, median_probe_ms: u64) -> TransferResult {
        let payload = "a".repeat(TRIAL_CHUNK_BYTES);
        let receipt = transfer_receipt(&payload, &receiver).unwrap();
        TransferResult {
            source: None,
            endpoint: String::new(),
            route_evidence: Some(receiver),
            receipts: vec![receipt; TRIAL_CHUNKS],
            median_probe_ms: Some(median_probe_ms),
            error: None,
        }
    }

    fn ip(adapter: &str, elapsed_ms: u64) -> TransferResult {
        path(RouteEvidence::Tcp {
            local_addr: "192.0.2.1:8470".parse().unwrap(),
            remote_addr: "192.0.2.2:60000".parse().unwrap(),
            local_interface: Some(adapter.into()),
        }, elapsed_ms)
    }

    #[test]
    fn ranks_verified_ip_routes_and_keeps_the_radio_for_control() {
        let bluetooth = path(RouteEvidence::BluetoothRfcomm {
            local_adapter: "020000000001".into(),
            remote_device: "020000000002".into(),
            service_uuid: crate::bluetooth::SERVICE_UUID.into(),
            channel: 3,
        }, 1);
        let paths = [ip("wire", 8), ip("wireless", 5), ip("wireless", 7), bluetooth];
        assert_eq!(recommend(&paths), ComputeRoutes {
            primary_ip: Some(1), additional_ip: vec![0], bluetooth_control: Some(3),
        });
    }

    #[test]
    fn skips_partial_failed_or_unmapped_paths_without_discarding_a_healthy_one() {
        let mut failed = ip("failed", 1);
        failed.error = Some("timed out".into());
        let mut partial = ip("partial", 2);
        partial.receipts.pop();
        let mut unmapped = ip("unmapped", 3);
        unmapped.receipts[0].route_evidence = None;
        unmapped.receipts[0].receiver_interface = None;
        let paths = [failed, partial, unmapped, ip("healthy", 30)];
        assert_eq!(recommend(&paths), ComputeRoutes {
            primary_ip: Some(3), additional_ip: vec![], bluetooth_control: None,
        });
    }
}
