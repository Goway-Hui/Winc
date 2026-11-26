//! Debug Adapter Protocol client over a loopback TCP transport.
//!
//! The protocol boundary uses UTF-16 positions; editor text remains char-offset based.

mod framing;
mod jsonrpc;
pub mod position;
mod protocol;
mod tcp;

pub use framing::{DecodeError, FramedReader, FramedWriter};
pub use jsonrpc::{
    Error, Id, JsonRpcError, Message, Notification, Request, RequestError, Response,
};
pub use position::Position;
pub use position::{char_position, char_to_utf16, utf16_position, utf16_to_char};
pub use protocol::{
    Breakpoint, Configuration, InitializeRequest, InitializeResult, LaunchRequest, OutputEvent,
    ProtocolMessage, Scope, ScopesResponse, SetBreakpointsRequest, SetBreakpointsResponse, Source,
    SourceBreakpoint, StackFrame, StackTraceResponse, StoppedEvent, Thread, ThreadsResponse,
    Variable, VariablesResponse,
};
pub use tcp::{Client, Connect, Event, TcpConfig};
