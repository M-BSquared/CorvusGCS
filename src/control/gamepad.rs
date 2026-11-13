//! Reading a USB or Bluetooth gamepad (`joystick.js` read `navigator.getGamepads`).
//!
//! The rules that turn four axes into a manual control frame are in
//! `model.rs` and tested with fed axes; this is only the device side: the
//! first connected pad, its two sticks, polled once a frame on the interface
//! thread. Nothing is opened until the operator turns the gamepad on in
//! Settings, and dropping the reader releases the device.

use gilrs::{Axis, Gilrs};

/// How often the frame loop comes back to read the pad while it is on; the
/// browser read it every animation frame.
pub const POLL_MS: u64 = 16;

/// The four axes the model reads, `[left x, left y, right x, right y]`. A
/// gamepad reports up as positive; the model follows the Gamepad API of the
/// web, where the axes grow down like the screen, so the vertical ones turn.
pub fn axes_of(left_x: f32, left_y: f32, right_x: f32, right_y: f32) -> [f64; 4] {
    let clean = |v: f32| {
        if v.is_finite() {
            f64::from(v).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    [clean(left_x), -clean(left_y), clean(right_x), -clean(right_y)]
}

/// The pads the system offers.
pub struct Pads {
    gilrs: Gilrs,
}

impl Pads {
    /// Opens the system's gamepad service; None when there is none (no
    /// permission, no udev), which reads as "no gamepad".
    pub fn open() -> Option<Pads> {
        Gilrs::new().ok().map(|gilrs| Pads { gilrs })
    }

    /// The sticks of the first connected pad, after taking in everything that
    /// happened since the last frame; None when none is plugged in.
    pub fn read(&mut self) -> Option<[f64; 4]> {
        while self.gilrs.next_event().is_some() {}
        let pad = self.gilrs.gamepads().map(|(_, g)| g).find(|g| g.is_connected())?;
        Some(axes_of(
            pad.value(Axis::LeftStickX),
            pad.value(Axis::LeftStickY),
            pad.value(Axis::RightStickX),
            pad.value(Axis::RightStickY),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_on_the_pad_is_up_in_the_model_and_a_broken_axis_is_centred() {
        assert_eq!(axes_of(0.5, 1.0, -0.25, -1.0), [0.5, -1.0, -0.25, 1.0]);
        assert_eq!(axes_of(f32::NAN, 2.0, f32::INFINITY, 0.0), [0.0, -1.0, 0.0, 0.0]);
        assert_eq!(axes_of(0.0, 0.0, 0.0, 0.0), [0.0, 0.0, 0.0, 0.0]);
    }
}
