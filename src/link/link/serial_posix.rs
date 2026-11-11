//! A serial port the way pyserial opens one on POSIX: raw termios, the
//! standard rates through `Bxxx`, any other rate through `termios2`
//! (Linux) or `IOSSIOSPEED` (macOS), DTR and RTS raised, the input buffer
//! flushed. Unlike the `serialport` crate this works on a pseudo-terminal,
//! which is what the simulated vehicle and the tests use.

use std::io;
use std::os::unix::io::RawFd;

use crate::link::transport::wait_writable;

fn check(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

fn standard_rate(baud: u32) -> Option<libc::speed_t> {
    Some(match baud {
        0 => libc::B0,
        50 => libc::B50,
        75 => libc::B75,
        110 => libc::B110,
        134 => libc::B134,
        150 => libc::B150,
        200 => libc::B200,
        300 => libc::B300,
        600 => libc::B600,
        1200 => libc::B1200,
        1800 => libc::B1800,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        230400 => libc::B230400,
        #[cfg(target_os = "linux")]
        460800 => libc::B460800,
        #[cfg(target_os = "linux")]
        500000 => libc::B500000,
        #[cfg(target_os = "linux")]
        576000 => libc::B576000,
        #[cfg(target_os = "linux")]
        921600 => libc::B921600,
        #[cfg(target_os = "linux")]
        1000000 => libc::B1000000,
        #[cfg(target_os = "linux")]
        1152000 => libc::B1152000,
        #[cfg(target_os = "linux")]
        1500000 => libc::B1500000,
        #[cfg(target_os = "linux")]
        2000000 => libc::B2000000,
        #[cfg(target_os = "linux")]
        2500000 => libc::B2500000,
        #[cfg(target_os = "linux")]
        3000000 => libc::B3000000,
        #[cfg(target_os = "linux")]
        3500000 => libc::B3500000,
        #[cfg(target_os = "linux")]
        4000000 => libc::B4000000,
        _ => return None,
    })
}

pub fn open(device: &str) -> io::Result<RawFd> {
    let path = std::ffi::CString::new(device).map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
    let fd = check(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    })?;
    Ok(fd)
}

/// `_reconfigure_port`: 8N1, raw, no flow control, VMIN = VTIME = 0.
pub fn configure(fd: RawFd, baud: u32, rtscts: bool) -> io::Result<()> {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    check(unsafe { libc::tcgetattr(fd, &mut t) })
        .map_err(|e| io::Error::new(e.kind(), format!("Could not configure port: {e}")))?;
    t.c_cflag |= libc::CLOCAL | libc::CREAD;
    t.c_lflag &= !(libc::ICANON
        | libc::ECHO
        | libc::ECHOE
        | libc::ECHOK
        | libc::ECHONL
        | libc::ISIG
        | libc::IEXTEN
        | libc::ECHOCTL
        | libc::ECHOKE);
    t.c_oflag &= !(libc::OPOST | libc::ONLCR | libc::OCRNL);
    t.c_iflag &= !(libc::INLCR | libc::IGNCR | libc::ICRNL | libc::IGNBRK | libc::PARMRK);
    #[cfg(target_os = "linux")]
    {
        t.c_iflag &= !libc::IUCLC;
    }
    let custom = standard_rate(baud).is_none();
    let speed = standard_rate(baud).unwrap_or(libc::B38400);
    t.c_cflag &= !libc::CSIZE;
    t.c_cflag |= libc::CS8;
    t.c_cflag &= !libc::CSTOPB;
    t.c_iflag &= !(libc::INPCK | libc::ISTRIP);
    t.c_cflag &= !(libc::PARENB | libc::PARODD);
    #[cfg(target_os = "linux")]
    {
        t.c_cflag &= !libc::CMSPAR;
    }
    t.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
    if rtscts {
        t.c_cflag |= libc::CRTSCTS;
    } else {
        t.c_cflag &= !libc::CRTSCTS;
    }
    t.c_cc[libc::VMIN] = 0;
    t.c_cc[libc::VTIME] = 0;
    unsafe {
        libc::cfsetispeed(&mut t, speed);
        libc::cfsetospeed(&mut t, speed);
    }
    check(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) })?;
    if custom {
        set_special_baudrate(fd, baud)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_special_baudrate(fd: RawFd, baud: u32) -> io::Result<()> {
    let mut t2: libc::termios2 = unsafe { std::mem::zeroed() };
    let result = (|| {
        check(unsafe { libc::ioctl(fd, libc::TCGETS2, &mut t2) })?;
        t2.c_cflag &= !libc::CBAUD;
        t2.c_cflag |= libc::BOTHER;
        t2.c_ispeed = baud;
        t2.c_ospeed = baud;
        check(unsafe { libc::ioctl(fd, libc::TCSETS2, &t2) })
    })();
    result.map(|_| ()).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Failed to set custom baud rate ({baud}): {e}"),
        )
    })
}

#[cfg(target_os = "macos")]
fn set_special_baudrate(fd: RawFd, baud: u32) -> io::Result<()> {
    const IOSSIOSPEED: libc::c_ulong = 0x80045402;
    let speed = baud as libc::speed_t;
    check(unsafe { libc::ioctl(fd, IOSSIOSPEED, &speed) }).map(|_| ())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn set_special_baudrate(_fd: RawFd, baud: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Invalid baud rate: {baud}"),
    ))
}

/// Raise DTR and RTS, ignoring a device that has no modem lines (EINVAL,
/// ENOTTY), as pyserial does.
pub fn raise_modem_lines(fd: RawFd, rts: bool) -> io::Result<()> {
    let mut lines: libc::c_int = libc::TIOCM_DTR;
    if rts {
        lines |= libc::TIOCM_RTS;
    }
    match check(unsafe { libc::ioctl(fd, libc::TIOCMBIS as _, &lines) }) {
        Ok(_) => Ok(()),
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTTY)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Set DTR and RTS to the given states, ignoring a device that has no modem
/// lines. Used when the operator asked for a line to stay released.
pub fn set_modem_lines(fd: RawFd, dtr: bool, rts: bool) -> io::Result<()> {
    for (bit, on) in [(libc::TIOCM_DTR, dtr), (libc::TIOCM_RTS, rts)] {
        let request = if on { libc::TIOCMBIS } else { libc::TIOCMBIC };
        match check(unsafe { libc::ioctl(fd, request as _, &bit) }) {
            Ok(_) => {}
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTTY)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn flush_input(fd: RawFd) {
    unsafe {
        libc::tcflush(fd, libc::TCIFLUSH);
    }
}

/// pyserial's blocking `write`: all of it, waiting whenever the driver's
/// buffer is full.
pub fn write_all(fd: RawFd, mut buf: &[u8]) -> io::Result<usize> {
    let total = buf.len();
    while !buf.is_empty() {
        let rc = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    wait_writable(fd, -1.0);
                    continue;
                }
                _ => return Err(err),
            }
        }
        buf = &buf[rc as usize..];
    }
    Ok(total)
}

pub fn set_rtscts(fd: RawFd, enable: bool) -> io::Result<()> {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    check(unsafe { libc::tcgetattr(fd, &mut t) })?;
    if enable {
        t.c_cflag |= libc::CRTSCTS;
    } else {
        t.c_cflag &= !libc::CRTSCTS;
    }
    check(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) }).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rates_mavlink_radios_use_are_standard() {
        for baud in [9600, 19200, 38400, 57600, 115200, 230400] {
            assert!(standard_rate(baud).is_some(), "{baud}");
        }
        assert!(
            standard_rate(250_000).is_none(),
            "a custom rate goes through the special path"
        );
    }
}
