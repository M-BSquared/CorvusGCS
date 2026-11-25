//! The plugin's side of [`crate::wire`]: what `export_plugin!` calls into. It
//! keeps the plugin, an egui of its own and a copy of what the plugin may look
//! at, and turns each call of the program into the calls of the
//! [`Plugin`] trait.
//!
//! Everything here runs on the one thread a module has. Its `static`s are its
//! own: the theme, the language and the rest the widgets read are handed over
//! by the program ([`wire::Env`]).

pub mod files;
pub mod host;
pub mod mail;
mod runtime;

use std::cell::RefCell;

use crate::plugin::Plugin;
use crate::wire::{self, Maybe};

thread_local! {
    static RUNTIME: RefCell<Option<runtime::Runtime>> = const { RefCell::new(None) };
    /// The reply the program is reading: kept until the next call.
    static REPLY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// `len` bytes the program may write into, given back by [`call`] or by the
/// answer of a question.
pub fn alloc(len: u32) -> u32 {
    let buffer = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(buffer) as *mut u8 as u32
}

/// Takes back what [`alloc`] gave out.
///
/// # Safety
/// `ptr` and `len` are those of one [`alloc`] that has not been taken back.
unsafe fn take_buffer(ptr: u32, len: u32) -> Box<[u8]> {
    // SAFETY: as the function's contract says, this is the boxed slice `alloc` leaked.
    unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize)) }
}

/// Where a slice lies, as one number: its address above, its length below.
fn pack(bytes: &[u8]) -> u64 {
    (u64::from(bytes.as_ptr() as u32) << 32) | bytes.len() as u64
}

/// One call of the program: `ptr` and `len` are the [`wire::Call`] it wrote
/// into a buffer of [`alloc`]. Answers where the [`wire::Reply`] lies.
pub fn call(ptr: u32, len: u32, make: fn() -> Box<dyn Plugin>, lang: &'static [(&'static str, &'static str)]) -> u64 {
    host::install_panic_hook();
    // SAFETY: the program got `ptr` from `corvus_alloc(len)` and passes it here once.
    let bytes = unsafe { take_buffer(ptr, len) };
    let reply = match wire::decode::<wire::Call>(&bytes) {
        Ok(call) => RUNTIME.with(|r| {
            let mut slot = r.borrow_mut();
            slot.get_or_insert_with(|| runtime::Runtime::new(make(), lang))
                .handle(call)
        }),
        Err(e) => {
            host::log("error", &format!("a call of the program could not be read: {e}"));
            wire::Reply {
                ret: Some(Maybe::Known(wire::Ret::Unsupported(wire::Empty {}))),
                ..wire::Reply::default()
            }
        }
    };
    let encoded = wire::encode(&reply).unwrap_or_default();
    REPLY.with(|r| {
        let mut r = r.borrow_mut();
        *r = encoded;
        pack(&r)
    })
}
