use std::{
    mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

const CLIENT_PEER_SUFFIX: &str = "007V";
const CLIENT_INTERFACE_CAPACITY: usize = 16;

fn format_peer_id(mac: [u8; 6]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{CLIENT_PEER_SUFFIX}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Match the installed Linux client's device identity without reading its
/// profile or calling its UI. ThunderHelper selects the last interface from
/// SIOCGIFCONF and appends its fixed product suffix to the uppercase MAC.
pub(super) fn local_client_peer_id() -> Option<String> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return None;
    }
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut interfaces: [libc::ifreq; CLIENT_INTERFACE_CAPACITY] = unsafe { mem::zeroed() };
    let mut config = libc::ifconf {
        ifc_len: mem::size_of_val(&interfaces) as libc::c_int,
        ifc_ifcu: libc::__c_anonymous_ifc_ifcu {
            ifcu_req: interfaces.as_mut_ptr(),
        },
    };
    if unsafe { libc::ioctl(socket.as_raw_fd(), libc::SIOCGIFCONF, &mut config) } < 0 {
        return None;
    }
    let count = usize::try_from(config.ifc_len).ok()? / mem::size_of::<libc::ifreq>();
    if count == 0 || count > interfaces.len() {
        return None;
    }
    let name = interfaces[count - 1].ifr_name;
    let mut request: libc::ifreq = unsafe { mem::zeroed() };
    request.ifr_name = name;
    if unsafe { libc::ioctl(socket.as_raw_fd(), libc::SIOCGIFFLAGS, &mut request) } < 0 {
        return None;
    }
    request.ifr_name = name;
    if unsafe { libc::ioctl(socket.as_raw_fd(), libc::SIOCGIFHWADDR, &mut request) } < 0 {
        return None;
    }
    let hardware = unsafe { request.ifr_ifru.ifru_hwaddr.sa_data };
    let mac = std::array::from_fn(|index| hardware[index] as u8);
    Some(format_peer_id(mac))
}

#[cfg(test)]
mod tests {
    use super::{format_peer_id, local_client_peer_id};
    use crate::xunlei_native::linux::runtime::discover_best_runtime;
    use std::process::Command;

    #[test]
    fn formats_linux_client_peer_id_from_mac_address() {
        assert_eq!(
            format_peer_id([0x02, 0x10, 0xAB, 0xCD, 0xEF, 0x77]),
            "0210ABCDEF77007V"
        );
    }

    #[test]
    fn local_linux_client_peer_id_has_the_native_shape() {
        let peer_id = local_client_peer_id().expect("network interface for native Xunlei");
        assert_eq!(peer_id.len(), 16);
        assert!(peer_id[..12].bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(peer_id.ends_with("007V"));
    }

    #[test]
    #[ignore = "compares against the installed Linux client helper"]
    fn local_peer_id_matches_installed_client() {
        let runtime = discover_best_runtime().expect("installed Linux Xunlei runtime");
        let output = Command::new(runtime.root.join("thunder"))
            .env("ELECTRON_RUN_AS_NODE", "1")
            .args([
                "-e",
                "const h=require(process.argv[1]); console.log(h.getPeerID())",
                runtime
                    .root
                    .join("resources/bin/ThunderHelper.node")
                    .to_str()
                    .expect("helper path"),
            ])
            .output()
            .expect("invoke installed client helper");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("helper output");
        let official = stdout.lines().last().expect("official peer ID");
        assert!(
            local_client_peer_id().as_deref() == Some(official),
            "Rust device Peer ID differs from the installed Linux client"
        );
    }
}
