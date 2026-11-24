//! Local DNS-SD hints. A service record identifies an endpoint for pairing, never a trusted peer.

use mdns_sd::{IfKind, IfPredicate, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const SERVICE: &str = "_consilium._tcp.local.";
const BROWSE_TIME: Duration = Duration::from_millis(1_200);
const MAX_HINTS: usize = 32;

pub(super) struct Discovery {
    daemon: ServiceDaemon,
    scan: tokio::sync::Mutex<()>,
    stopped: CancellationToken,
}

impl Discovery {
    pub(super) fn publish(port: u16) -> mdns_sd::Result<Self> {
        let discovery = Self {
            daemon: ServiceDaemon::new()?,
            scan: tokio::sync::Mutex::new(()),
            stopped: CancellationToken::new(),
        };
        discovery.daemon.disable_interface(IfKind::Predicate(IfPredicate::new(|interface| {
            !private_lan(interface.ip())
        })))?;
        let label = format!("consilium-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let mut service = ServiceInfo::new(SERVICE, &label, &format!("{label}.local."), "", port, None)?
            .enable_addr_auto();
        service.set_interfaces(vec![IfKind::Predicate(IfPredicate::new(|interface| private_lan(interface.ip())))]);
        discovery.daemon.register(service)?;
        Ok(discovery)
    }

    pub(super) fn stop(&self) {
        if self.stopped.is_cancelled() {
            return;
        }
        self.stopped.cancel();
        let _ = self.daemon.shutdown();
    }

    pub(super) async fn browse(&self, port: u16) -> Vec<String> {
        tokio::select! {
            _ = self.stopped.cancelled() => Vec::new(),
            found = self.browse_once(port) => found,
        }
    }

    async fn browse_once(&self, port: u16) -> Vec<String> {
        let _scan = self.scan.lock().await;
        let Ok(interfaces) = if_addrs::get_if_addrs() else { return Vec::new(); };
        let Ok(events) = self.daemon.browse(SERVICE) else { return Vec::new(); };
        let browse = StopBrowse(&self.daemon);
        let known_ips: BTreeSet<_> = interfaces.iter().map(|interface| interface.ip()).collect();
        let deadline = tokio::time::Instant::now() + BROWSE_TIME;
        let mut hints = BTreeSet::new();
        while tokio::time::Instant::now() < deadline && hints.len() < MAX_HINTS {
            let Ok(Ok(event)) = tokio::time::timeout_at(deadline, events.recv_async()).await else { break };
            if let ServiceEvent::ServiceResolved(service) = event {
                if service.get_port() != port {
                    continue;
                }
                for ip in service.get_addresses_v4() {
                    if private_lan(ip.into()) && !known_ips.contains(&ip.into()) {
                        hints.insert(ip);
                    }
                    if hints.len() >= MAX_HINTS {
                        break;
                    }
                }
            }
        }
        drop(browse);
        let candidates = hints.into_iter().flat_map(|ip| {
            interfaces.iter().filter_map(move |interface| {
                let if_addrs::IfAddr::V4(address) = &interface.addr else { return None; };
                on_link(address, ip).then(|| (address.ip, SocketAddrV4::new(ip, port)))
            })
        }).collect();
        super::verify_discovery_candidates(candidates).await
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stop the daemon's query even if the frontend cancels the command mid-scan.
struct StopBrowse<'a>(&'a ServiceDaemon);

impl Drop for StopBrowse<'_> {
    fn drop(&mut self) {
        let _ = self.0.stop_browse(SERVICE);
    }
}

fn private_lan(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if (ip.is_private() || ip.is_link_local()) && super::route_ip_allowed(ip.into()))
}

fn on_link(address: &if_addrs::Ifv4Addr, candidate: Ipv4Addr) -> bool {
    let local = u32::from(address.ip);
    let mask = u32::from(address.netmask);
    let target = u32::from(candidate);
    private_lan(address.ip.into())
        && private_lan(candidate.into())
        && candidate != address.ip
        && target & mask == local & mask
        && (address.prefixlen >= 31 || (target != (local & mask) && target != (local | !mask)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_must_be_private_reachable_hosts_on_the_same_interface() {
        let interface = if_addrs::Ifv4Addr {
            ip: "192.168.4.20".parse().unwrap(),
            netmask: "255.255.255.0".parse().unwrap(),
            broadcast: Some("192.168.4.255".parse().unwrap()),
            prefixlen: 24,
        };
        assert!(on_link(&interface, "192.168.4.21".parse().unwrap()));
        for address in ["192.168.4.20", "192.168.4.0", "192.168.4.255", "192.168.5.21", "8.8.8.8", "127.0.0.1"] {
            assert!(!on_link(&interface, address.parse().unwrap()), "{address}");
        }
    }
}
