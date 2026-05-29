
use futuresdr::prelude::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Command;

const IFF_TAP: i16 = 0x0002;
const IFF_NO_PI: i16 = 0x1000;
// _IOW('T', 202, int) on Linux. Stable kernel ABI.
const TUNSETIFF: libc::c_ulong = 0x400454CA;

#[derive(Block)]
#[message_inputs(rx)]
pub struct TapNic {
    iface: String,
    mac: [u8; 6],
    fd: Option<OwnedFd>,
}

impl TapNic {
    pub fn new(cfg: String) -> Self {
        let (iface, mac) = parse_cfg(&cfg).unwrap_or_else(|e| {
            panic!("tap_nic_plugin: bad config {cfg:?}: {e}");
        });
        Self {
            iface,
            mac,
            fd: None,
        }
    }

    async fn rx(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Blob(blob) => {
                if let Some(fd) = self.fd.as_ref() {
                    let raw = fd.as_raw_fd();
                    // Write the payload (a bare RFtap frame) verbatim — no
                    // Ethernet wrapping. A capture therefore starts with the
                    // "RFta" magic, which Wireshark's RFtap dissector recognizes.
                    let n = unsafe { libc::write(raw, blob.as_ptr() as *const _, blob.len()) };
                    if n < 0 {
                        let err = std::io::Error::last_os_error();
                        // EAGAIN on a full kernel buffer is expected under heavy
                        // bursts; drop the frame rather than stalling the runtime.
                        if err.raw_os_error() != Some(libc::EAGAIN) {
                            warn!("tap_nic: write({}B) failed: {}", blob.len(), err);
                        }
                    }
                }
            }
            Pmt::Finished => {
                io.finished = true;
            }
            _ => {}
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for TapNic {
    async fn init(&mut self, _mio: &mut MessageOutputs, _b: &mut BlockMeta) -> Result<()> {
        let fd = open_tap(&self.iface)?;
        set_nonblocking(fd.as_raw_fd())?;
        configure_iface(&self.iface, &self.mac);

        // Spawn a TX-side drain thread on a dup'd fd. Today it just reads and
        // discards (logs at debug); TODO: route into a radio-egress path.
        let dup_fd = unsafe { libc::dup(fd.as_raw_fd()) };
        if dup_fd < 0 {
            warn!(
                "tap_nic: dup() for TX drain failed: {}",
                std::io::Error::last_os_error()
            );
        } else {
            let iface = self.iface.clone();
            std::thread::spawn(move || tx_drain(dup_fd, iface));
        }

        info!(
            "tap_nic: {} up, mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.iface,
            self.mac[0],
            self.mac[1],
            self.mac[2],
            self.mac[3],
            self.mac[4],
            self.mac[5],
        );
        self.fd = Some(fd);
        Ok(())
    }
}

fn parse_cfg(cfg: &str) -> std::result::Result<(String, [u8; 6]), String> {
    // Format: "<iface>:<aa:bb:cc:dd:ee:ff>"
    // splitn(2, ':') to keep the colon-separated MAC intact.
    let (iface, mac_str) = cfg
        .split_once(':')
        .ok_or_else(|| "expected 'iface:mac' format".to_string())?;
    if iface.is_empty() {
        return Err("empty iface name".into());
    }
    if iface.len() >= 16 {
        return Err(format!("iface name too long: {iface:?} (max 15)"));
    }
    let parts: Vec<&str> = mac_str.split(':').collect();
    if parts.len() != 6 {
        return Err(format!("MAC must be 6 hex octets, got {:?}", mac_str));
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16).map_err(|e| format!("MAC octet {p:?}: {e}"))?;
    }
    Ok((iface.to_string(), mac))
}

fn open_tap(iface: &str) -> Result<OwnedFd> {
    // open(O_RDWR) on /dev/net/tun
    let path = b"/dev/net/tun\0";
    let raw = unsafe {
        libc::open(
            path.as_ptr() as *const _,
            libc::O_RDWR | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(Error::RuntimeError(format!(
            "open /dev/net/tun: {}",
            std::io::Error::last_os_error()
        ))
        .into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };

    // ifreq layout: char name[16] | short flags | padding to 40 bytes.
    let mut ifr = [0u8; 40];
    for (i, b) in iface.bytes().enumerate() {
        ifr[i] = b;
    }
    let flags = IFF_TAP | IFF_NO_PI;
    ifr[16..18].copy_from_slice(&flags.to_le_bytes());

    let r = unsafe { libc::ioctl(fd.as_raw_fd(), TUNSETIFF, ifr.as_mut_ptr()) };
    if r < 0 {
        return Err(Error::RuntimeError(format!(
            "TUNSETIFF {iface:?}: {} (need CAP_NET_ADMIN — run as root?)",
            std::io::Error::last_os_error()
        ))
        .into());
    }
    Ok(fd)
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(Error::RuntimeError(format!(
            "F_GETFL: {}",
            std::io::Error::last_os_error()
        ))
        .into());
    }
    let r = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if r < 0 {
        return Err(Error::RuntimeError(format!(
            "F_SETFL O_NONBLOCK: {}",
            std::io::Error::last_os_error()
        ))
        .into());
    }
    Ok(())
}

fn configure_iface(iface: &str, mac: &[u8; 6]) {
    // Best-effort iface config from inside the plugin. The supported deploy
    // model is to pre-create the iface as persistent + owned by the user:
    //   sudo ip tuntap add dev <iface> mode tap user $USER
    //   sudo ip link set <iface> address <mac>
    //   sudo ip link set <iface> up
    // In that flow these commands fail (no privileges) but the iface is
    // already correctly configured, so log at debug and continue.
    let mac_str = format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    let r = Command::new("ip")
        .args(["link", "set", "dev", iface, "address", &mac_str])
        .status();
    if !matches!(r, Ok(s) if s.success()) {
        debug!(
            "tap_nic: ip link set address on {iface} did not succeed ({r:?}) — assuming pre-configured"
        );
    }
    let r = Command::new("ip")
        .args(["link", "set", "dev", iface, "up"])
        .status();
    if !matches!(r, Ok(s) if s.success()) {
        debug!(
            "tap_nic: ip link set up on {iface} did not succeed ({r:?}) — assuming pre-configured"
        );
    }
}

fn tx_drain(fd: RawFd, iface: String) {
    // The dup'd fd inherits the non-blocking flag set on the original; switch
    // back to blocking for this thread so we don't burn CPU on EAGAIN.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // EBADF means the iface fd was closed (plugin shutdown); exit cleanly.
            debug!("tap_nic[{iface}] tx_drain read error: {err} — exiting");
            unsafe { libc::close(fd) };
            return;
        }
        if n == 0 {
            // EOF (iface gone)
            debug!("tap_nic[{iface}] tx_drain EOF — exiting");
            unsafe { libc::close(fd) };
            return;
        }
        // TODO(tx): wire this into a radio-egress block. For now we discard;
        // the trace lets us see Linux→SDR traffic during testing.
        debug!("tap_nic[{iface}] TX drop: {} bytes (TX not yet wired)", n);
    }
}

plugin_api::export_plugin! {
    name: "TapNic",
    description: "Linux TAP NIC: writes each incoming blob (a bare RFtap frame) verbatim to the TAP device — no Ethernet wrapping (RX only; TX stubbed).",
    config: String,
    create: |cfg, _id| {
        TapNic::new(cfg)
    }
}
