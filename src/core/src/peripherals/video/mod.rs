//! Camera video (`video.py`, `routes/video.py`): RTSP decoded by an ffmpeg
//! child process into JPEG frames the camera window fetches one at a time,
//! WebRTC passed through (only the WHEP signalling runs here, so a camera's
//! password never reaches the browser).

pub mod ffmpeg;
pub mod model;
mod routes;
pub mod service;
pub mod stream;
pub mod url;

pub use routes::register;
pub use service::{Frame, FrameError, VideoService, WhepResult};
pub use stream::Timing;
