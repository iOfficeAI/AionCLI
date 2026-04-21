pub mod error;
pub mod shell;
pub mod stt;
pub mod stt_deepgram;
pub mod stt_openai;

pub use error::{ShellError, SttError};
pub use shell::ShellService;
pub use stt::SttService;
