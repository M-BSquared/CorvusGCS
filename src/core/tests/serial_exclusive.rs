//! A vehicle link takes its serial port alone: a second ground station on the
//! same USB cable is refused with a reason instead of each program reading an
//! arbitrary half of the stream. Played on a pseudo terminal, no hardware.
#![cfg(unix)]

use corvus_link::plain::connection::{open, ConnectError, Options};

fn pty() -> (i32, String) {
    let (mut master, mut slave) = (-1, -1);
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: openpty fills the descriptors and the name buffer it is given.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            name.as_mut_ptr(),
            std::ptr::null_mut::<libc::termios>() as _,
            std::ptr::null_mut::<libc::winsize>() as _,
        )
    };
    assert_eq!(rc, 0);
    // SAFETY: openpty wrote a NUL terminated name.
    let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: the slave is reopened by path below.
    unsafe { libc::close(slave) };
    (master, path)
}

fn exclusive() -> Options {
    Options {
        baud: 57600,
        exclusive: true,
        ..Options::default()
    }
}

#[test]
fn a_second_program_on_the_same_port_is_refused_and_the_port_is_free_again_after_close() {
    let (master, path) = pty();
    let first = open(&path, &exclusive()).expect("the first open takes the port");
    match open(&path, &exclusive()) {
        Err(ConnectError::Serial { errno, message }) => {
            assert_eq!(errno, Some(libc::EWOULDBLOCK));
            assert!(message.contains("in use by another program"), "{message}");
            assert!(message.contains(&path), "{message}");
        }
        Err(other) => panic!("wrong refusal: {other}"),
        Ok(_) => panic!("two programs must not share one vehicle link"),
    }
    first.close();
    drop(first);
    open(&path, &exclusive()).expect("a closed port can be taken again");
    // SAFETY: the master descriptor is ours.
    unsafe { libc::close(master) };
}

#[test]
fn a_port_opened_the_old_way_is_not_locked() {
    let (master, path) = pty();
    let plain = Options {
        baud: 57600,
        ..Options::default()
    };
    let _a = open(&path, &plain).expect("first");
    let _b = open(&path, &plain).expect("sharing stays possible for services that ask for it");
    // SAFETY: the master descriptor is ours.
    unsafe { libc::close(master) };
}
