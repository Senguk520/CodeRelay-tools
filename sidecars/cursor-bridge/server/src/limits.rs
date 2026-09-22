//! Upper bounds on Cursor-facing request bodies.
//!
//! The Cursor protocol endpoints buffer whole requests before decoding them
//! (Connect unary framing makes streaming impossible for those routes), so an
//! unbounded read is an unbounded allocation. The two limits below bound it from
//! opposite sides:
//!
//! - [`MAX_COMPRESSED_BODY_BYTES`] caps what arrives **on the wire**, before
//!   `Content-Encoding` is undone. Without it a request could stream gigabytes
//!   into the process even though the decoded result would be rejected.
//! - [`MAX_REQUEST_BODY_BYTES`] caps the **decompressed** body a handler will
//!   buffer. This is the one that bounds a decompression bomb: a few KB of gzip
//!   can expand far past the compressed limit, so the compressed cap alone is
//!   not enough.
//!
//! The values are deliberately tens of megabytes rather than the megabyte
//! range: a Cursor Agent turn carries the conversation, the tool definitions and
//! any attached context in one body, and a legitimate large turn was observed
//! well past 2 MB (which is why the default extractor limit was raised in the
//! first place). These caps exist to stop a pathological request, not to police
//! normal ones.

/// Largest **decompressed** body a handler will buffer.
pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Largest **compressed** (on-the-wire) body accepted.
///
/// Smaller than the decompressed cap on purpose: legitimate payloads here are
/// mostly text (so they compress well), and a small wire cap is what keeps a
/// hostile client from spending the process's bandwidth before the decoded-size
/// check can reject it.
pub const MAX_COMPRESSED_BODY_BYTES: usize = 16 * 1024 * 1024;
