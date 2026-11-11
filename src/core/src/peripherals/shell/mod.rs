//! Programs on this computer and the terminal registry (`local_shell.py` and
//! the session half of `ssh_bridge.py`).

pub mod local;
mod routes;
pub mod session;

pub use local::{LocalRunner, LocalSession};
pub use routes::register;
pub use session::{Terminal, Terminals};
