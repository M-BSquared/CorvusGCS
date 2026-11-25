//! The questions a plugin asks the program and waits for ([`wire::Query`]).

use crate::wire;

#[link(wasm_import_module = "corvus")]
extern "C" {
    /// Takes a [`wire::Query`] and answers with a [`wire::Told`] written into a
    /// buffer the program got from `corvus_alloc`, or 0 for nothing to say.
    #[link_name = "host"]
    fn corvus_host(ptr: u32, len: u32) -> u64;
}

pub fn ask(query: &wire::Query) -> wire::Told {
    let Ok(bytes) = wire::encode(query) else {
        return wire::Told::default();
    };
    // SAFETY: the program reads `bytes` during the call and keeps nothing of them.
    let at = unsafe { corvus_host(bytes.as_ptr() as u32, bytes.len() as u32) };
    if at == 0 {
        return wire::Told::default();
    }
    // SAFETY: a non-zero answer is a buffer the program took from `corvus_alloc` with
    // exactly this length, filled, and handed over to us.
    let told = unsafe { super::take_buffer((at >> 32) as u32, at as u32) };
    wire::decode(&told).unwrap_or_default()
}

pub fn log(level: &str, text: &str) {
    ask(&wire::Query::Log(wire::Line {
        level: level.into(),
        text: text.into(),
    }));
}

/// A panic ends the module with a trap, which says nothing: its words go to the
/// program first.
pub fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let payload = info.payload();
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic".into());
            let place = info
                .location()
                .map_or_else(String::new, |l| format!(" ({}:{})", l.file(), l.line()));
            ask(&wire::Query::Panic(wire::Line {
                level: "error".into(),
                text: format!("{what}{place}"),
            }));
        }));
    });
}
