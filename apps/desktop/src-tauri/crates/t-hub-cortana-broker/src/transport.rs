use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use t_hub_cortana_broker_protocol::{BrokerResponse, CortanaBrokerRequest, ObservedPeer};

const MAX_PACKET_BYTES: usize = 64 * 1024;
const REQUIRED_HANDLES: usize = 4;

pub struct ReceivedBrokerPacket {
    pub request: CortanaBrokerRequest,
    pub peer: ObservedPeer,
    pub handles: [OwnedFd; REQUIRED_HANDLES],
}

pub fn receive_request(socket: RawFd) -> io::Result<ReceivedBrokerPacket> {
    let peer = peer_identity(socket)?;
    let mut payload = vec![0_u8; MAX_PACKET_BYTES];
    let mut control = vec![0_u8; cmsg_space(REQUIRED_HANDLES * size_of::<RawFd>())];
    let mut iovec = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();
    let received = unsafe { libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC) };
    if received <= 0 {
        return Err(io::Error::last_os_error());
    }
    if received as usize == MAX_PACKET_BYTES
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker request exceeded its bound",
        ));
    }
    payload.truncate(received as usize);
    let request = serde_json::from_slice::<CortanaBrokerRequest>(&payload)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "broker request is malformed"))?;
    let descriptors = received_rights(&message)?;
    let handles: [OwnedFd; REQUIRED_HANDLES] = descriptors.try_into().map_err(|descriptors: Vec<_>| {
        drop(descriptors);
        io::Error::new(
            io::ErrorKind::InvalidData,
            "broker request must carry exactly four handles",
        )
    })?;
    Ok(ReceivedBrokerPacket {
        request,
        peer,
        handles,
    })
}

pub fn send_response(socket: RawFd, response: &BrokerResponse) -> io::Result<()> {
    let payload = serde_json::to_vec(response)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "broker response is malformed"))?;
    if payload.len() > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker response exceeded its bound",
        ));
    }
    let sent = unsafe {
        libc::send(
            socket,
            payload.as_ptr().cast(),
            payload.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    if sent < 0 || sent as usize != payload.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn request(
    socket_path: &Path,
    request: &CortanaBrokerRequest,
    handles: [&OwnedFd; REQUIRED_HANDLES],
) -> io::Result<BrokerResponse> {
    let socket = seqpacket_socket()?;
    connect(socket.as_raw_fd(), socket_path)?;
    set_timeout(socket.as_raw_fd())?;
    let payload = serde_json::to_vec(request)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "broker request is malformed"))?;
    if payload.len() > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker request exceeded its bound",
        ));
    }
    send_with_rights(socket.as_raw_fd(), &payload, handles)?;
    let mut response = vec![0_u8; MAX_PACKET_BYTES];
    let received = unsafe {
        libc::recv(
            socket.as_raw_fd(),
            response.as_mut_ptr().cast(),
            response.len(),
            0,
        )
    };
    if received <= 0 || received as usize == response.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "broker response was unavailable or oversized",
        ));
    }
    response.truncate(received as usize);
    serde_json::from_slice(&response)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "broker response is malformed"))
}

fn peer_identity(socket: RawFd) -> io::Result<ObservedPeer> {
    let mut credential: libc::ucred = unsafe { zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            socket,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credential as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
        || length as usize != size_of::<libc::ucred>()
        || credential.pid <= 0
        || credential.uid == u32::MAX
    {
        return Err(io::Error::last_os_error());
    }
    let pid = credential.pid as u32;
    let metadata = std::fs::metadata(format!("/proc/{pid}/exe"))?;
    use std::os::unix::fs::MetadataExt;
    Ok(ObservedPeer {
        uid: credential.uid,
        pid,
        process_start_ticks: process_start_ticks(pid)?,
        executable_device: metadata.dev(),
        executable_inode: metadata.ino(),
    })
}

fn process_start_ticks(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end = stat
        .rfind(')')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "peer stat is malformed"))?;
    stat[end + 2..]
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "peer start time is malformed"))
}

fn received_rights(message: &libc::msghdr) -> io::Result<Vec<OwnedFd>> {
    let mut descriptors = Vec::new();
    let mut header = unsafe { libc::CMSG_FIRSTHDR(message) };
    while !header.is_null() {
        let header_ref = unsafe { &*header };
        if header_ref.cmsg_level != libc::SOL_SOCKET || header_ref.cmsg_type != libc::SCM_RIGHTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker request carried an unsupported control message",
            ));
        }
        let data_bytes = header_ref.cmsg_len as usize - cmsg_len(0);
        if data_bytes == 0 || data_bytes % size_of::<RawFd>() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker descriptor message is malformed",
            ));
        }
        let count = data_bytes / size_of::<RawFd>();
        let data = unsafe { libc::CMSG_DATA(header).cast::<RawFd>() };
        for index in 0..count {
            let descriptor = unsafe { *data.add(index) };
            descriptors.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
        }
        header = unsafe { libc::CMSG_NXTHDR(message, header) };
    }
    Ok(descriptors)
}

fn send_with_rights(
    socket: RawFd,
    payload: &[u8],
    handles: [&OwnedFd; REQUIRED_HANDLES],
) -> io::Result<()> {
    let descriptors = handles.map(AsRawFd::as_raw_fd);
    let mut control = vec![0_u8; cmsg_space(size_of_val(&descriptors))];
    let mut iovec = libc::iovec {
        iov_base: payload.as_ptr().cast_mut().cast(),
        iov_len: payload.len(),
    };
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if header.is_null() {
        return Err(io::Error::other("broker descriptor buffer is unavailable"));
    }
    unsafe {
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = cmsg_len(size_of_val(&descriptors)) as _;
        std::ptr::copy_nonoverlapping(
            descriptors.as_ptr().cast::<u8>(),
            libc::CMSG_DATA(header),
            size_of_val(&descriptors),
        );
        message.msg_controllen = (*header).cmsg_len as usize;
    }
    let sent = unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL) };
    if sent < 0 || sent as usize != payload.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn seqpacket_socket() -> io::Result<OwnedFd> {
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
}

fn connect(socket: RawFd, path: &Path) -> io::Result<()> {
    let bytes = path
        .as_os_str()
        .as_encoded_bytes();
    let mut address: libc::sockaddr_un = unsafe { zeroed() };
    if !path.is_absolute() || bytes.is_empty() || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker socket path is invalid",
        ));
    }
    address.sun_family = libc::AF_UNIX as _;
    for (target, source) in address.sun_path.iter_mut().zip(bytes) {
        *target = *source as libc::c_char;
    }
    let length = (size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
    if unsafe { libc::connect(socket, (&address as *const libc::sockaddr_un).cast(), length) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_timeout(socket: RawFd) -> io::Result<()> {
    let timeout = libc::timeval {
        tv_sec: 5,
        tv_usec: 0,
    };
    for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        if unsafe {
            libc::setsockopt(
                socket,
                libc::SOL_SOCKET,
                option,
                (&timeout as *const libc::timeval).cast(),
                size_of::<libc::timeval>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

const fn align(value: usize) -> usize {
    let alignment = size_of::<usize>();
    (value + alignment - 1) & !(alignment - 1)
}

const fn cmsg_len(data: usize) -> usize {
    align(size_of::<libc::cmsghdr>()) + data
}

const fn cmsg_space(data: usize) -> usize {
    align(size_of::<libc::cmsghdr>()) + align(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn transport_rejects_stream_without_required_handles() {
        let (client, server) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let _ = unsafe {
                libc::send(
                    client.as_raw_fd(),
                    b"{}".as_ptr().cast(),
                    2,
                    libc::MSG_NOSIGNAL,
                )
            };
        });
        assert!(receive_request(server.as_fd().as_raw_fd()).is_err());
    }
}
