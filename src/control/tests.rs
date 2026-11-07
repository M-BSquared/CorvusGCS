use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

/// A stand-in backend that counts the frames it is sent.
fn backend() -> (Server, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = got.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut text = String::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let want = head
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() >= want {
                        break;
                    }
                }
            }
            if let Some(body) = text.split("\r\n\r\n").nth(1) {
                seen.relock().push(body.to_string());
            }
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}");
        }
    });
    (Server::local(port), got)
}

#[test]
fn a_pad_with_no_surface_streams_nothing() {
    let (server, got) = backend();
    let mut c = Control::new(server, Config::default());
    let ctx = egui::Context::default();
    c.update(&ctx, &json!({"connected": true}), &Default::default(), true, 0.0);
    std::thread::sleep(Duration::from_millis(500));
    c.shut_down();
    assert!(got.relock().is_empty());
}

#[test]
fn the_stream_sends_neutral_frames_while_the_link_is_up_and_stops_on_shutdown() {
    let (server, got) = backend();
    let cfg = Config {
        sticks: true,
        ..Config::default()
    };
    let mut c = Control::new(server, cfg);
    let ctx = egui::Context::default();
    c.update(&ctx, &json!({"connected": true}), &Default::default(), true, 0.0);
    std::thread::sleep(Duration::from_millis(700));
    c.shut_down();
    let frames = got.relock().clone();
    assert!(frames.len() >= 2, "{frames:?}");
    assert!(frames.iter().all(|b| {
        let v: Value = serde_json::from_str(b).expect("json");
        v == json!({"x": 0.0, "y": 0.0, "z": 0.5, "r": 0.0})
    }));
    // Nothing more once it is down.
    let n = got.relock().len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(got.relock().len(), n);
}

#[test]
fn no_link_sends_nothing() {
    let (server, got) = backend();
    let mut c = Control::new(
        server,
        Config {
            sticks: true,
            ..Config::default()
        },
    );
    let ctx = egui::Context::default();
    c.update(&ctx, &json!({"connected": false}), &Default::default(), true, 0.0);
    std::thread::sleep(Duration::from_millis(500));
    c.shut_down();
    assert!(got.relock().is_empty());
}

#[test]
fn a_gamepad_pushed_forward_flies_and_the_arm_gesture_asks_once() {
    let (server, got) = backend();
    let mut c = Control::new(
        server,
        Config {
            gamepad: true,
            ..Config::default()
        },
    );
    let ctx = egui::Context::default();
    let state = json!({"connected": true, "armed": true});
    // Left stick down on the raw axes is thrust up.
    c.feed_gamepad(Some([0.0, -1.0, 0.0, 0.0]));
    c.update(&ctx, &state, &Default::default(), true, 0.0);
    assert_eq!(c.frame.z, 1.0);
    c.shut_down();
    let _ = got;
    // Disarmed, both sticks inward for a second: one arm request.
    let (server, got) = backend();
    let mut c = Control::new(
        server,
        Config {
            gamepad: true,
            ..Config::default()
        },
    );
    let parked = json!({"connected": true, "armed": false});
    c.feed_gamepad(Some([1.0, 0.0, -1.0, 0.0]));
    c.update(&ctx, &parked, &Default::default(), true, 0.0);
    assert!(c.frame.is_neutral(), "silent while the gesture is held");
    c.update(&ctx, &parked, &Default::default(), true, 1000.0);
    assert!(c.gesture.sent && c.stick_mode_after_arm);
    std::thread::sleep(Duration::from_millis(400));
    c.shut_down();
    let bodies = got.relock().clone();
    assert!(bodies.iter().any(|b| b.contains("\"arm\":true")), "{bodies:?}");
}

fn key(key: egui::Key, pressed: bool, shift: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers {
            shift,
            ..Default::default()
        },
    }
}

/// One frame of the window with these events, then the pad's step.
fn frame_with(c: &mut Control, ctx: &egui::Context, events: Vec<egui::Event>, now: f64) {
    let shift = events
        .iter()
        .any(|e| matches!(e, egui::Event::Key { modifiers, .. } if modifiers.shift));
    let mut all = vec![egui::Event::ModifiersChanged(egui::Modifiers {
        shift,
        ..Default::default()
    })];
    all.extend(events);
    let input = egui::RawInput {
        events: all,
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        c.update(
            ui.ctx(),
            &json!({"connected": true, "armed": false}),
            &Default::default(),
            true,
            now,
        );
    });
    out.textures_delta.clear();
}

#[test]
fn the_keyboard_is_locked_until_shift_w_twice_and_escape_locks_it_again() {
    let (server, _) = backend();
    let mut c = Control::new(
        server,
        Config {
            keys: true,
            wasd: true,
            ..Config::default()
        },
    );
    let ctx = egui::Context::default();
    // Locked: an arrow key flies nothing.
    frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, true, false)], 0.0);
    assert!(c.frame.is_neutral());
    frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, false, false)], 10.0);
    // Shift + W, released, Shift + W again inside the window.
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 100.0);
    assert!(c.lock.locked);
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, false, true)], 200.0);
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 500.0);
    assert!(!c.lock.locked);
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, false, true)], 600.0);
    // Unlocked: the arrow flies half a stick, Shift doubles it.
    frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, true, false)], 700.0);
    assert_eq!(c.frame.x, 0.5);
    frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, true, true)], 750.0);
    assert_eq!(c.frame.x, 1.0);
    frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, false, false)], 800.0);
    frame_with(&mut c, &ctx, vec![key(egui::Key::D, true, false)], 850.0);
    assert_eq!(c.frame.r, 0.5);
    frame_with(&mut c, &ctx, vec![key(egui::Key::D, false, false)], 900.0);
    // Escape locks it again and lets go of everything.
    frame_with(&mut c, &ctx, vec![key(egui::Key::Escape, true, false)], 950.0);
    assert!(c.lock.locked);
    assert!(c.frame.is_neutral());
    c.shut_down();
}

#[test]
fn a_second_press_too_late_does_not_unlock() {
    let (server, _) = backend();
    let mut c = Control::new(
        server,
        Config {
            wasd: true,
            ..Config::default()
        },
    );
    let ctx = egui::Context::default();
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 0.0);
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, false, true)], 100.0);
    frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 1500.0);
    assert!(c.lock.locked);
    c.shut_down();
}

#[test]
fn a_new_config_locks_the_keyboard_and_lets_go_of_everything() {
    let (server, _) = backend();
    let mut c = Control::new(
        server,
        Config {
            keys: true,
            ..Config::default()
        },
    );
    c.lock.set(false);
    c.kbd.up = true;
    c.set_config(Config {
        keys: true,
        wasd: true,
        ..Config::default()
    });
    assert!(c.lock.locked);
    assert_eq!(c.kbd, Held::default());
    c.shut_down();
}
