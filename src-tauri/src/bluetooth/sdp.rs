//! Windows service discovery runs in an owned process because Winsock can
//! block in WSALookupServiceBeginW before it returns a cancellable handle.
use super::{worker_process::OwnedChild, SERVICE_UUID};
use std::io::{Read, Write};
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::Bluetooth::{AF_BTH, BTHPROTO_RFCOMM, NS_BTH, SOCKADDR_BTH};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Networking::WinSock::{
    WSAAddressToStringW, WSACleanup, WSAGetLastError, WSALookupServiceBeginW, WSALookupServiceEnd,
    WSALookupServiceNextW, WSAStartup, CSADDR_INFO, LUP_FLUSHCACHE, LUP_RETURN_ADDR,
    SOCKET_ADDRESS, SOCK_STREAM, WSADATA, WSAENOMORE, WSAQUERYSETW, WSASERVICE_NOT_FOUND,
    WSA_E_NO_MORE,
};

const MAX_RECORDS: usize = 8;
const MAX_ADDRESSES: usize = 8;
const BUFFER_BYTES: usize = 64 * 1024;
const MAX_RADIO: u64 = 0xffff_ffff_ffff;
const NOT_FOUND: i32 = -1;
const INVALID_RESULT: i32 = -2;
const AMBIGUOUS: i32 = -3;
const INVALID_REQUEST: i32 = -4;

pub(super) fn resolve_channel(
    address: u64,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<u32, String> {
    let remaining = || {
        if cancelled() {
            return Err("Bluetooth connection cancelled.".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "Bluetooth connection timed out while resolving the selected service.".to_string(),
            );
        }
        Ok(remaining)
    };
    remaining()?;
    let worker_error = || "Windows could not run its Bluetooth service lookup worker.".to_string();
    let mut worker = OwnedChild::spawn("--bluetooth-sdp-worker").map_err(|_| worker_error())?;
    let mut request = worker.child.stdin.take().ok_or_else(worker_error)?;
    request
        .write_all(&address.to_le_bytes())
        .map_err(|_| worker_error())?;
    drop(request);

    loop {
        let wait = remaining()?.min(Duration::from_millis(25));
        if let Some(status) = worker.child.try_wait().map_err(|_| worker_error())? {
            if !status.success() {
                return Err(worker_error());
            }
            break;
        }
        std::thread::sleep(wait);
    }
    remaining()?;
    let mut reply = [0; 8];
    worker
        .child
        .stdout
        .take()
        .ok_or_else(worker_error)?
        .read_exact(&mut reply)
        .map_err(|_| worker_error())?;
    let status = i32::from_le_bytes(reply[..4].try_into().unwrap());
    let channel = u32::from_le_bytes(reply[4..].try_into().unwrap());
    match (status, channel) {
        (0, 1..=30) => Ok(channel),
        (NOT_FOUND | WSASERVICE_NOT_FOUND, 0) => Err("The selected computer is not advertising Consilium's Bluetooth service. Keep Consilium open there and check its Bluetooth setting.".into()),
        (AMBIGUOUS, 0) => Err("Windows found multiple RFCOMM channels for the selected Consilium Bluetooth service.".into()),
        (INVALID_RESULT, 0) => Err("Windows returned invalid address information for the selected Consilium Bluetooth service.".into()),
        (code, 0) if code > 0 => Err(format!("Windows could not discover the selected Consilium Bluetooth service ({code}).")),
        _ => Err(worker_error()),
    }
}

pub(super) fn run_worker() -> i32 {
    let mut request = [0; 8];
    if std::io::stdin().read_exact(&mut request).is_err() {
        return 1;
    }
    let address = u64::from_le_bytes(request);
    let (status, channel) = match lookup(address) {
        Ok(channel) => (0, channel),
        Err(status) => (status, 0),
    };
    let mut response = [0; 8];
    response[..4].copy_from_slice(&status.to_le_bytes());
    response[4..].copy_from_slice(&channel.to_le_bytes());
    let mut output = std::io::stdout().lock();
    i32::from(
        output
            .write_all(&response)
            .and_then(|()| output.flush())
            .is_err(),
    )
}

struct Winsock;
impl Drop for Winsock {
    fn drop(&mut self) {
        // SAFETY: Only created after a successful WSAStartup in this process.
        unsafe {
            WSACleanup();
        }
    }
}

struct Lookup(HANDLE);
impl Drop for Lookup {
    fn drop(&mut self) {
        // SAFETY: This owns the live service query handle.
        unsafe {
            WSALookupServiceEnd(self.0);
        }
    }
}

#[repr(align(16))]
struct LookupBuffer([u8; BUFFER_BYTES]);

fn last_error() -> i32 {
    // SAFETY: Retrieves the Winsock error on this thread.
    unsafe { WSAGetLastError() }
}

fn lookup(address: u64) -> Result<u32, i32> {
    if !(1..MAX_RADIO).contains(&address) {
        return Err(INVALID_REQUEST);
    }
    let mut sockets = WSADATA::default();
    // SAFETY: WSADATA is writable, and WSAStartup receives a supported version.
    let status = unsafe { WSAStartup(0x0202, &mut sockets) };
    if status != 0 {
        return Err(status);
    }
    let _winsock = Winsock;
    let remote = SOCKADDR_BTH {
        addressFamily: AF_BTH,
        btAddr: address,
        ..Default::default()
    };
    let mut context = [0u16; 256];
    let mut context_len = context.len() as u32;
    // SAFETY: The packed address and output UTF-16 buffer are valid for their given lengths.
    let status = unsafe {
        WSAAddressToStringW(
            (&remote as *const SOCKADDR_BTH).cast(),
            size_of::<SOCKADDR_BTH>() as u32,
            null(),
            context.as_mut_ptr(),
            &mut context_len,
        )
    };
    if status != 0 {
        return Err(last_error());
    }
    if context_len as usize > context.len() || !context.contains(&0) {
        return Err(INVALID_RESULT);
    }
    let mut service = GUID::from_u128(
        uuid::Uuid::parse_str(SERVICE_UUID)
            .map_err(|_| INVALID_REQUEST)?
            .as_u128(),
    );
    let query = WSAQUERYSETW {
        dwSize: size_of::<WSAQUERYSETW>() as u32,
        lpServiceClassId: &mut service,
        dwNameSpace: NS_BTH,
        lpszContext: context.as_mut_ptr(),
        ..Default::default()
    };
    let mut handle = null_mut();
    // SAFETY: query, UUID, and address context stay live across the synchronous call.
    let status =
        unsafe { WSALookupServiceBeginW(&query, LUP_FLUSHCACHE | LUP_RETURN_ADDR, &mut handle) };
    if status != 0 {
        return Err(last_error());
    }
    if handle.is_null() {
        return Err(INVALID_RESULT);
    }
    let handle = Lookup(handle);
    let mut buffer = LookupBuffer([0; BUFFER_BYTES]);
    let mut channel = None;
    for record in 0..=MAX_RECORDS {
        let mut length = BUFFER_BYTES as u32;
        buffer.0.fill(0);
        buffer.0[..4].copy_from_slice(&(size_of::<WSAQUERYSETW>() as u32).to_ne_bytes());
        let result = buffer.0.as_mut_ptr().cast::<WSAQUERYSETW>();
        // SAFETY: The aligned buffer has room for the query result and nested records.
        let status =
            unsafe { WSALookupServiceNextW(handle.0, LUP_RETURN_ADDR, &mut length, result) };
        if status != 0 {
            return match last_error() {
                WSAENOMORE | WSA_E_NO_MORE => channel.ok_or(NOT_FOUND),
                status => Err(status),
            };
        }
        let used = buffer.0.get(..length as usize).ok_or(INVALID_RESULT)?;
        if record == MAX_RECORDS || used.len() < size_of::<WSAQUERYSETW>() {
            return Err(INVALID_RESULT);
        }
        // SAFETY: The provider succeeded and the fixed result header fits in the buffer.
        add_results(
            unsafe { used.as_ptr().cast::<WSAQUERYSETW>().read() },
            used,
            address,
            &mut channel,
        )?;
    }
    Err(INVALID_RESULT)
}

fn contained_offset(buffer: &[u8], pointer: usize, length: usize) -> Result<usize, i32> {
    let offset = pointer
        .checked_sub(buffer.as_ptr() as usize)
        .ok_or(INVALID_RESULT)?;
    if length > buffer.len().saturating_sub(offset) || offset > buffer.len() {
        return Err(INVALID_RESULT);
    }
    Ok(offset)
}

fn add_results(
    result: WSAQUERYSETW,
    buffer: &[u8],
    address: u64,
    found: &mut Option<u32>,
) -> Result<(), i32> {
    let count = result.dwNumberOfCsAddrs as usize;
    if result.dwSize as usize != size_of::<WSAQUERYSETW>() || count > MAX_ADDRESSES {
        return Err(INVALID_RESULT);
    }
    if count == 0 {
        return Ok(());
    }
    let size = size_of::<CSADDR_INFO>();
    let offset = contained_offset(buffer, result.lpcsaBuffer as usize, count * size)?;
    for index in 0..count {
        // SAFETY: This fixed-layout SDK record fits within the checked array range.
        let entry = unsafe {
            buffer
                .as_ptr()
                .add(offset + index * size)
                .cast::<CSADDR_INFO>()
                .read_unaligned()
        };
        // Some providers leave the optional protocol metadata unset. Reject a
        // different supplied protocol, then verify family, radio, and channel.
        if !matches!(entry.iSocketType, 0 | SOCK_STREAM)
            || (entry.iProtocol != 0 && entry.iProtocol != BTHPROTO_RFCOMM as i32)
        {
            continue;
        }
        // The provider can place the connectable address in either field.
        for socket_addr in [entry.LocalAddr, entry.RemoteAddr] {
            if let Some(channel) = read_channel(buffer, socket_addr, address)? {
                if found.is_some_and(|previous| previous != channel) {
                    return Err(AMBIGUOUS);
                }
                *found = Some(channel);
            }
        }
    }
    Ok(())
}

fn read_channel(
    buffer: &[u8],
    socket_addr: SOCKET_ADDRESS,
    address: u64,
) -> Result<Option<u32>, i32> {
    if socket_addr.lpSockaddr.is_null() && socket_addr.iSockaddrLength == 0 {
        return Ok(None);
    }
    let len = usize::try_from(socket_addr.iSockaddrLength).map_err(|_| INVALID_RESULT)?;
    let offset = contained_offset(buffer, socket_addr.lpSockaddr as usize, len)?;
    if len < size_of::<u16>() {
        return Err(INVALID_RESULT);
    }
    let family = u16::from_ne_bytes([buffer[offset], buffer[offset + 1]]);
    if family != AF_BTH {
        return Ok(None);
    }
    if len < size_of::<SOCKADDR_BTH>() {
        return Err(INVALID_RESULT);
    }
    // SAFETY: The complete fixed-layout Bluetooth address fits within the checked range.
    let remote = unsafe {
        buffer
            .as_ptr()
            .add(offset)
            .cast::<SOCKADDR_BTH>()
            .read_unaligned()
    };
    let port = remote.port;
    Ok((remote.btAddr == address && (1..=30).contains(&port)).then_some(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RADIO: u64 = 0x1234_5678_9abc;

    fn provider_address(buffer: &mut [u8], offset: usize, radio: u64, port: u32) -> SOCKET_ADDRESS {
        assert!(offset + size_of::<SOCKADDR_BTH>() <= buffer.len());
        let record = SOCKADDR_BTH {
            addressFamily: AF_BTH,
            btAddr: radio,
            port,
            ..Default::default()
        };
        // SAFETY: The complete packed address fits within the checked fixture.
        let record_ptr = unsafe { buffer.as_mut_ptr().add(offset).cast::<SOCKADDR_BTH>() };
        unsafe {
            record_ptr.write_unaligned(record);
        }
        SOCKET_ADDRESS {
            lpSockaddr: record_ptr.cast(),
            iSockaddrLength: size_of::<SOCKADDR_BTH>() as i32,
        }
    }

    fn provider_query(
        buffer: &mut [u8],
        local: SOCKET_ADDRESS,
        remote: SOCKET_ADDRESS,
    ) -> WSAQUERYSETW {
        assert!(size_of::<CSADDR_INFO>() <= 64 && buffer.len() >= 64);
        let entry = buffer.as_mut_ptr().cast::<CSADDR_INFO>();
        // SAFETY: The fixture has room for one complete record. Address fields
        // are written separately so the buffer's zeroed padding stays initialized.
        unsafe {
            std::ptr::addr_of_mut!((*entry).LocalAddr.lpSockaddr).write_unaligned(local.lpSockaddr);
            std::ptr::addr_of_mut!((*entry).LocalAddr.iSockaddrLength)
                .write_unaligned(local.iSockaddrLength);
            std::ptr::addr_of_mut!((*entry).RemoteAddr.lpSockaddr)
                .write_unaligned(remote.lpSockaddr);
            std::ptr::addr_of_mut!((*entry).RemoteAddr.iSockaddrLength)
                .write_unaligned(remote.iSockaddrLength);
        }
        WSAQUERYSETW {
            dwSize: size_of::<WSAQUERYSETW>() as u32,
            dwNumberOfCsAddrs: 1,
            lpcsaBuffer: entry,
            ..Default::default()
        }
    }

    #[test]
    fn uses_matching_port_from_either_field_and_accepts_duplicates() {
        let mut buffer = [0; 256];
        let wanted = provider_address(&mut buffer, 64, RADIO, 7);
        let other = provider_address(&mut buffer, 128, RADIO + 1, 8);
        for (local, remote) in [(wanted, other), (other, wanted), (wanted, wanted)] {
            let query = provider_query(&mut buffer, local, remote);
            let mut channel = None;
            assert_eq!(add_results(query, &buffer, RADIO, &mut channel), Ok(()));
            assert_eq!(channel, Some(7));
        }
    }

    #[test]
    fn rejects_ambiguous_or_malformed_provider_results() {
        let mut buffer = [0; 256];
        let local = provider_address(&mut buffer, 64, RADIO, 7);
        let remote = provider_address(&mut buffer, 128, RADIO, 8);
        let query = provider_query(&mut buffer, local, remote);
        assert_eq!(
            add_results(query, &buffer, RADIO, &mut None),
            Err(AMBIGUOUS)
        );
        let mut short = local;
        short.iSockaddrLength -= 1;
        let query = provider_query(&mut buffer, remote, short);
        assert_eq!(
            add_results(query, &buffer, RADIO, &mut None),
            Err(INVALID_RESULT)
        );
        let query = provider_query(&mut buffer, local, remote);
        assert_eq!(
            add_results(query, &buffer[..96], RADIO, &mut None),
            Err(INVALID_RESULT)
        );
        let query = WSAQUERYSETW {
            dwNumberOfCsAddrs: (MAX_ADDRESSES + 1) as u32,
            ..query
        };
        assert_eq!(
            add_results(query, &buffer, RADIO, &mut None),
            Err(INVALID_RESULT)
        );
    }

    #[test]
    fn ignores_unusable_channels_and_foreign_radio() {
        let mut buffer = [0; 256];
        let local = provider_address(&mut buffer, 64, RADIO, 0);
        let remote = provider_address(&mut buffer, 128, RADIO, 31);
        let query = provider_query(&mut buffer, local, remote);
        let mut channel = None;
        assert_eq!(add_results(query, &buffer, RADIO, &mut channel), Ok(()));
        assert_eq!(channel, None);
        let remote = provider_address(&mut buffer, 128, RADIO + 1, 1);
        let query = provider_query(&mut buffer, local, remote);
        assert_eq!(add_results(query, &buffer, RADIO, &mut channel), Ok(()));
        assert_eq!(channel, None);
    }
}
