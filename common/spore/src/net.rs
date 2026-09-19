//! A DHCPv4 client (RFC 2131) and interface setup by ioctl. Kernel `ip=dhcp` is not
//! enough: hcloud leases a /32 whose gateway is outside the subnet, so the gateway
//! needs a host route before the default route.

use std::ffi::CString;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub addr: Ipv4Addr,
    pub mask: Option<Ipv4Addr>,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    /// RFC 3442 option 121: (destination, prefix length, gateway).
    pub routes: Vec<(Ipv4Addr, u8, Ipv4Addr)>,
    server: Option<Ipv4Addr>,
    kind: u8,
}

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;
const COOKIE: [u8; 4] = [99, 130, 83, 99];

fn packet(xid: u32, mac: &[u8; 6], kind: u8, want: Option<(Ipv4Addr, Ipv4Addr)>) -> Vec<u8> {
    let mut p = vec![0u8; 240];
    p[0] = 1;
    p[1] = 1;
    p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[10] = 0x80;
    p[28..34].copy_from_slice(mac);
    p[236..240].copy_from_slice(&COOKIE);
    p.extend_from_slice(&[53, 1, kind]);
    if let Some((addr, server)) = want {
        p.extend_from_slice(&[50, 4]);
        p.extend_from_slice(&addr.octets());
        p.extend_from_slice(&[54, 4]);
        p.extend_from_slice(&server.octets());
    }
    p.extend_from_slice(&[55, 5, 1, 3, 6, 51, 121]);
    p.push(255);
    p.resize(300.max(p.len()), 0);
    p
}

fn ip(b: &[u8]) -> Ipv4Addr {
    Ipv4Addr::new(b[0], b[1], b[2], b[3])
}

/// Parses a BOOTREPLY for `xid`; None if it is not one.
pub fn parse(p: &[u8], xid: u32) -> Option<Lease> {
    if p.len() < 240 || p[0] != 2 || p[4..8] != xid.to_be_bytes() || p[236..240] != COOKIE {
        return None;
    }
    let mut l = Lease {
        addr: ip(&p[16..20]),
        mask: None,
        routers: Vec::new(),
        dns: Vec::new(),
        routes: Vec::new(),
        server: None,
        kind: 0,
    };
    let mut o = &p[240..];
    while let [code, rest @ ..] = o {
        match *code {
            0 => {
                o = rest;
                continue;
            }
            255 => break,
            _ => {}
        }
        let [len, rest @ ..] = rest else { break };
        let len = *len as usize;
        if rest.len() < len {
            break;
        }
        let v = &rest[..len];
        match *code {
            1 if len == 4 => l.mask = Some(ip(v)),
            3 => l.routers = v.chunks_exact(4).map(ip).collect(),
            6 => l.dns = v.chunks_exact(4).map(ip).collect(),
            53 if len == 1 => l.kind = v[0],
            54 if len == 4 => l.server = Some(ip(v)),
            121 => {
                let mut r = v;
                while let [w, rest @ ..] = r {
                    let n = (*w as usize).div_ceil(8);
                    if *w > 32 || rest.len() < n + 4 {
                        break;
                    }
                    let mut d = [0u8; 4];
                    d[..n].copy_from_slice(&rest[..n]);
                    l.routes.push((Ipv4Addr::from(d), *w, ip(&rest[n..n + 4])));
                    r = &rest[n + 4..];
                }
            }
            _ => {}
        }
        o = &rest[len..];
    }
    Some(l)
}

fn ifreq(name: &str) -> libc::ifreq {
    let mut r: libc::ifreq = unsafe { std::mem::zeroed() };
    for (d, s) in r
        .ifr_name
        .iter_mut()
        .zip(name.bytes().take(libc::IFNAMSIZ - 1))
    {
        *d = s as libc::c_char;
    }
    r
}

fn sockaddr(a: Ipv4Addr) -> libc::sockaddr {
    let sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(a).to_be(),
        },
        sin_zero: [0; 8],
    };
    unsafe { std::mem::transmute(sin) }
}

fn ioctl<T>(fd: i32, req: libc::c_ulong, arg: &mut T) -> io::Result<()> {
    if unsafe { libc::ioctl(fd, req as _, arg as *mut T) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn control() -> io::Result<UdpSocket> {
    UdpSocket::bind("0.0.0.0:0")
}

pub fn link_up(name: &str) -> io::Result<()> {
    let s = control()?;
    let mut r = ifreq(name);
    ioctl(s.as_raw_fd(), libc::SIOCGIFFLAGS, &mut r)?;
    unsafe { r.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short };
    ioctl(s.as_raw_fd(), libc::SIOCSIFFLAGS, &mut r)
}

fn add_route(
    fd: i32,
    dev: &CString,
    dst: Ipv4Addr,
    prefix: u8,
    gw: Option<Ipv4Addr>,
) -> io::Result<()> {
    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };
    rt.rt_dst = sockaddr(dst);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    rt.rt_genmask = sockaddr(Ipv4Addr::from(mask));
    rt.rt_flags = libc::RTF_UP;
    if prefix == 32 {
        rt.rt_flags |= libc::RTF_HOST;
    }
    if let Some(g) = gw.filter(|g| !g.is_unspecified()) {
        rt.rt_gateway = sockaddr(g);
        rt.rt_flags |= libc::RTF_GATEWAY;
    }
    rt.rt_dev = dev.as_ptr() as *mut libc::c_char;
    match ioctl(fd, libc::SIOCADDRT, &mut rt) {
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Ok(()),
        r => r,
    }
}

fn configure(iface: &str, l: &Lease) -> io::Result<()> {
    let s = control()?;
    let fd = s.as_raw_fd();
    let mut r = ifreq(iface);
    r.ifr_ifru.ifru_addr = sockaddr(l.addr);
    ioctl(fd, libc::SIOCSIFADDR, &mut r)?;
    let mask = l.mask.unwrap_or(Ipv4Addr::BROADCAST);
    r.ifr_ifru.ifru_netmask = sockaddr(mask);
    ioctl(fd, libc::SIOCSIFNETMASK, &mut r)?;
    let dev = CString::new(iface).unwrap();
    let net = u32::from(l.addr) & u32::from(mask);
    let on_link = |g: Ipv4Addr| u32::from(g) & u32::from(mask) == net;
    let routes: Vec<(Ipv4Addr, u8, Ipv4Addr)> = if l.routes.is_empty() {
        l.routers
            .first()
            .map(|&g| (Ipv4Addr::UNSPECIFIED, 0, g))
            .into_iter()
            .collect()
    } else {
        l.routes.clone()
    };
    for &(_, _, g) in &routes {
        if !g.is_unspecified() && !on_link(g) {
            add_route(fd, &dev, g, 32, None)?;
        }
    }
    for &(d, p, g) in &routes {
        add_route(fd, &dev, d, p, Some(g))?;
    }
    Ok(())
}

fn mac(iface: &str) -> Result<[u8; 6], String> {
    let s =
        fs::read_to_string(format!("/sys/class/net/{iface}/address")).map_err(|e| e.to_string())?;
    let v: Vec<u8> = s
        .trim()
        .split(':')
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .collect();
    v.try_into()
        .map_err(|_| format!("bad MAC address '{}'", s.trim()))
}

fn exchange(
    sock: &UdpSocket,
    xid: u32,
    msg: &[u8],
    want: u8,
    deadline: Instant,
) -> Result<Option<Lease>, String> {
    let dst = SocketAddrV4::new(Ipv4Addr::BROADCAST, 67);
    let mut buf = [0u8; 1500];
    let mut next_send = Instant::now();
    let mut wait = Duration::from_millis(250);
    while Instant::now() < deadline {
        if Instant::now() >= next_send {
            sock.send_to(msg, dst)
                .map_err(|e| format!("DHCP send: {e}"))?;
            next_send = Instant::now() + wait;
            wait = (wait * 2).min(Duration::from_secs(2));
        }
        match sock.recv(&mut buf) {
            Ok(n) => match parse(&buf[..n], xid) {
                Some(l) if l.kind == want => return Ok(Some(l)),
                Some(l) if l.kind == NAK => return Ok(None),
                _ => {}
            },
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(format!("DHCP receive: {e}")),
        }
    }
    Err("no DHCP answer".into())
}

fn dhcp(iface: &str, timeout: Duration) -> Result<Lease, String> {
    let mac = mac(iface)?;
    let sock = UdpSocket::bind("0.0.0.0:68").map_err(|e| format!("bind :68: {e}"))?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|e| e.to_string())?;
    let name = CString::new(iface).unwrap();
    let r = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            name.as_ptr().cast(),
            name.as_bytes_with_nul().len() as libc::socklen_t,
        )
    };
    if r != 0 {
        return Err(format!(
            "SO_BINDTODEVICE {iface}: {}",
            io::Error::last_os_error()
        ));
    }
    let deadline = Instant::now() + timeout;
    loop {
        let mut x = [0u8; 4];
        unsafe { libc::getrandom(x.as_mut_ptr().cast(), 4, 0) };
        let xid = u32::from_ne_bytes(x);
        let Some(offer) = exchange(
            &sock,
            xid,
            &packet(xid, &mac, DISCOVER, None),
            OFFER,
            deadline,
        )?
        else {
            continue;
        };
        let server = offer.server.ok_or("DHCP offer without server id")?;
        let req = packet(xid, &mac, REQUEST, Some((offer.addr, server)));
        if let Some(ack) = exchange(&sock, xid, &req, ACK, deadline)? {
            return Ok(ack);
        }
    }
}

fn interfaces() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir("/sys/class/net")
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "lo")
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Brings up lo and the first other interface with DHCP, and writes /etc/resolv.conf.
/// Returns the interface name.
pub fn up() -> Result<String, String> {
    link_up("lo").map_err(|e| format!("lo: {e}"))?;
    let t = Instant::now();
    let iface = loop {
        if let Some(i) = interfaces().into_iter().next() {
            break i;
        }
        if t.elapsed() > Duration::from_secs(10) {
            return Err("no network interface".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    link_up(&iface).map_err(|e| format!("{iface}: {e}"))?;
    let lease = dhcp(&iface, Duration::from_secs(30)).map_err(|e| format!("{iface}: {e}"))?;
    configure(&iface, &lease).map_err(|e| format!("{iface}: {e}"))?;
    let conf: String = lease
        .dns
        .iter()
        .map(|d| format!("nameserver {d}\n"))
        .collect();
    fs::write("/etc/resolv.conf", conf).map_err(|e| format!("/etc/resolv.conf: {e}"))?;
    Ok(iface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_layout() {
        let p = packet(0x1234_5678, &[1, 2, 3, 4, 5, 6], DISCOVER, None);
        assert_eq!(p.len(), 300);
        assert_eq!(&p[..4], &[1, 1, 6, 0]);
        assert_eq!(&p[4..8], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(p[10], 0x80);
        assert_eq!(&p[28..34], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&p[240..243], &[53, 1, DISCOVER]);
    }

    #[test]
    fn parse_hcloud_style_ack() {
        let mut p = packet(7, &[0; 6], ACK, None);
        p[0] = 2;
        p[16..20].copy_from_slice(&[95, 216, 1, 2]);
        p.truncate(240);
        p.extend_from_slice(&[53, 1, ACK, 1, 4, 255, 255, 255, 255, 3, 4, 172, 31, 1, 1]);
        p.extend_from_slice(&[6, 8, 185, 12, 64, 1, 185, 12, 64, 2, 54, 4, 172, 31, 1, 1]);
        p.extend_from_slice(&[
            121, 14, 32, 172, 31, 1, 1, 0, 0, 0, 0, 0, 172, 31, 1, 1, 255,
        ]);
        let l = parse(&p, 7).unwrap();
        assert_eq!(l.kind, ACK);
        assert_eq!(l.addr, Ipv4Addr::new(95, 216, 1, 2));
        assert_eq!(l.mask, Some(Ipv4Addr::BROADCAST));
        assert_eq!(l.routers, vec![Ipv4Addr::new(172, 31, 1, 1)]);
        assert_eq!(l.dns.len(), 2);
        assert_eq!(
            l.routes,
            vec![
                (Ipv4Addr::new(172, 31, 1, 1), 32, Ipv4Addr::UNSPECIFIED),
                (Ipv4Addr::UNSPECIFIED, 0, Ipv4Addr::new(172, 31, 1, 1)),
            ]
        );
        assert!(parse(&p, 8).is_none());
    }
}
