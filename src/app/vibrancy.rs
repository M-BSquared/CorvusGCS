//! The blur behind a window of its own. egui cannot blur the desktop, so on
//! macOS the window gets an `NSVisualEffectView` under its content, as
//! `corvus/native_window.py` (`MacBlur`) did for the web interface: the
//! Popover material, which lets the most colour through, in the light or dark
//! appearance of the theme, rounded like the frame. The window itself is
//! transparent (`show_titlebar`), so the view shows wherever egui paints with
//! alpha. The view goes into the window's frame view, below the content
//! view, because the content view draws in its own layer and a subview of it
//! would cover the page.

/// Whether a window of its own can be blurred here.
pub fn blurs() -> bool {
    cfg!(target_os = "macos")
}

/// Blurs the window titled `title`, and keeps its look in step with the theme.
#[cfg(target_os = "macos")]
pub fn apply(title: &str, dark: bool, radius: f64) {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol};
    use objc2::{msg_send, ClassType, MainThreadMarker};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
        NSAutoresizingMaskOptions, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindowOrderingMode,
    };

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    for win in app.windows().iter() {
        if win.title().to_string() != title {
            continue;
        }
        let Some(content) = win.contentView() else {
            continue;
        };
        let Some(frame) = (unsafe { content.superview() }) else {
            continue;
        };
        let name = unsafe {
            if dark {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            }
        };
        let existing = frame
            .subviews()
            .iter()
            .find(|v| v.isKindOfClass(NSVisualEffectView::class()));
        let view: Retained<NSVisualEffectView> = match existing {
            Some(v) => {
                let view: Retained<NSVisualEffectView> = unsafe { Retained::cast_unchecked(v) };
                view.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
                continue;
            }
            None => NSVisualEffectView::initWithFrame(mtm.alloc(), frame.bounds()),
        };
        view.setMaterial(NSVisualEffectMaterial::Popover);
        view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        view.setState(NSVisualEffectState::Active);
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        view.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
        view.setWantsLayer(true);
        unsafe {
            let layer: Option<Retained<AnyObject>> = msg_send![&*view, layer];
            if let Some(layer) = layer {
                let _: () = msg_send![&*layer, setCornerRadius: radius];
                let _: () = msg_send![&*layer, setMasksToBounds: true];
            }
            frame.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, Some(&content));
        }
        win.setOpaque(false);
        win.invalidateShadow();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply(_title: &str, _dark: bool, _radius: f64) {}
