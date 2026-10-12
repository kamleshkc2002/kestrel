//! On-demand local interface addresses; nothing is cached or logged.

use std::{
    ffi::CStr,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

pub const MAX_LOCAL_ADDRESSES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAddress {
    pub interface: String,
    pub address: IpAddr,
    pub prefix_length: u8,
}

/// Addresses of interfaces that are up, without loopback, at most
/// [`MAX_LOCAL_ADDRESSES`], sorted by interface then address.
pub fn local_addresses() -> io::Result<Vec<LocalAddress>> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `list` is a valid out-pointer; success hands ownership to `freeifaddrs` below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut addresses = Vec::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: `entry` is a node of the list returned by `getifaddrs`, valid until it is freed.
        let current = unsafe { &*entry };
        entry = current.ifa_next;
        if let Some(address) = convert(current) {
            addresses.push(address);
        }
    }
    // SAFETY: `list` came from a successful `getifaddrs` and is freed exactly once.
    unsafe { libc::freeifaddrs(list) };
    addresses.sort_by(|left, right| {
        (&left.interface, left.address).cmp(&(&right.interface, right.address))
    });
    addresses.dedup();
    addresses.truncate(MAX_LOCAL_ADDRESSES);
    Ok(addresses)
}

fn convert(entry: &libc::ifaddrs) -> Option<LocalAddress> {
    let flags = entry.ifa_flags as libc::c_int;
    if flags & libc::IFF_UP == 0 || flags & libc::IFF_LOOPBACK != 0 {
        return None;
    }
    if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
        return None;
    }
    // SAFETY: `ifa_name` is a NUL-terminated interface name owned by the list.
    let interface = unsafe { CStr::from_ptr(entry.ifa_name) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: `ifa_addr` is non-null and points at a sockaddr whose family selects its layout.
    let family = i32::from(unsafe { (*entry.ifa_addr).sa_family });
    let (address, prefix_length) = match family {
        libc::AF_INET => {
            // SAFETY: AF_INET addresses are `sockaddr_in`.
            let address = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr));
            let prefix = (!entry.ifa_netmask.is_null()).then(|| {
                // SAFETY: the netmask of an AF_INET entry is a `sockaddr_in`.
                let mask = unsafe { &*(entry.ifa_netmask as *const libc::sockaddr_in) };
                u32::from_be(mask.sin_addr.s_addr).count_ones() as u8
            });
            (IpAddr::V4(ip), prefix.unwrap_or(32))
        }
        libc::AF_INET6 => {
            // SAFETY: AF_INET6 addresses are `sockaddr_in6`.
            let address = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
            let ip = Ipv6Addr::from(address.sin6_addr.s6_addr);
            let prefix = (!entry.ifa_netmask.is_null()).then(|| {
                // SAFETY: the netmask of an AF_INET6 entry is a `sockaddr_in6`.
                let mask = unsafe { &*(entry.ifa_netmask as *const libc::sockaddr_in6) };
                mask.sin6_addr
                    .s6_addr
                    .iter()
                    .map(|byte| byte.count_ones())
                    .sum::<u32>() as u8
            });
            (IpAddr::V6(ip), prefix.unwrap_or(128))
        }
        _ => return None,
    };
    Some(LocalAddress {
        interface,
        address,
        prefix_length,
    })
}

#[cfg(test)]
mod tests {
    use super::local_addresses;

    #[test]
    fn addresses_exclude_loopback_and_stay_bounded() {
        let addresses = local_addresses().expect("getifaddrs works in the test environment");
        assert!(addresses.len() <= super::MAX_LOCAL_ADDRESSES);
        assert!(addresses.iter().all(|entry| !entry.address.is_loopback()));
        assert!(
            addresses
                .iter()
                .all(|entry| entry.prefix_length <= if entry.address.is_ipv4() { 32 } else { 128 })
        );
    }
}
