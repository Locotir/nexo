//! Streams cut anywhere, payloads mangled anywhere: the SSE layer must give the same events however
//! the bytes arrive, and no parser may panic on a payload a provider (or a proxy) could send.
//! Seeds are fixed; every case is reproducible from the names in the failure message.

mod common;
mod parser_mutation;
mod sse_split;
