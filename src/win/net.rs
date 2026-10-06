//! Addresses of the local network interfaces.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_FRIENDLY_NAME,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
};

/// Unicast addresses of the interfaces that are up.
pub fn interface_addresses() -> Vec<IpAddr> {
    let flags = GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER
        | GAA_FLAG_SKIP_FRIENDLY_NAME;
    // u64 elements keep the buffer aligned for the structures inside.
    let mut buffer: Vec<u64> = vec![0; 2048];
    let mut status = ERROR_BUFFER_OVERFLOW;
    for _ in 0..3 {
        let mut size = (buffer.len() * 8) as u32;
        // SAFETY: the buffer holds `size` bytes.
        status = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                flags,
                std::ptr::null(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_BUFFER_OVERFLOW {
            break;
        }
        buffer.resize(size as usize / 8 + 1, 0);
    }
    if status != NO_ERROR {
        return Vec::new();
    }
    let mut addresses = Vec::new();
    let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // SAFETY: GetAdaptersAddresses filled the buffer with linked lists that
    // stay inside it.
    unsafe {
        while let Some(current) = adapter.as_ref() {
            if current.OperStatus == IfOperStatusUp {
                let mut unicast = current.FirstUnicastAddress;
                while let Some(entry) = unicast.as_ref() {
                    if let Some(ip) = ip_of(entry.Address.lpSockaddr) {
                        addresses.push(ip);
                    }
                    unicast = entry.Next;
                }
            }
            adapter = current.Next;
        }
    }
    addresses
}

/// # Safety
/// `address` must be null or point to a valid socket address.
unsafe fn ip_of(address: *const SOCKADDR) -> Option<IpAddr> {
    // SAFETY: guaranteed by the caller; the family says which struct it is.
    unsafe {
        match address.as_ref()?.sa_family {
            AF_INET => {
                let v4 = &*address.cast::<SOCKADDR_IN>();
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                    v4.sin_addr.S_un.S_addr,
                ))))
            }
            AF_INET6 => {
                let v6 = &*address.cast::<SOCKADDR_IN6>();
                let ip = Ipv6Addr::from(v6.sin6_addr.u.Byte);
                // Link-local IPv6 needs a zone id, which browsers do not accept.
                (!ip.is_unicast_link_local()).then_some(IpAddr::V6(ip))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_listed() {
        let addresses = interface_addresses();
        assert!(addresses.iter().any(IpAddr::is_loopback), "{addresses:?}");
    }
}
