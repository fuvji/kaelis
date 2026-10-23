//! Buffered byte copying for archive and file installs.
//!
//! `std::io::copy` uses an 8 KiB stack buffer. Every install path here copies
//! zip entries, so a larger buffer removes most of the read/write system calls
//! without changing memory use in any meaningful way.

use std::io::{Read, Write};

/// 128 KiB: the largest size that still fits comfortably in cache while making
/// an 8 KiB loop's syscall count sixteen times smaller.
pub const COPY_BUFFER: usize = 128 * 1024;

/// Copy `reader` into `writer`, returning the number of bytes written.
pub fn copy_stream<R, W>(reader: &mut R, writer: &mut W) -> std::io::Result<u64>
where
    R: Read + ?Sized,
    W: Write + ?Sized,
{
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(total);
        }
        writer.write_all(&buffer[..read])?;
        total += read as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::{COPY_BUFFER, copy_stream};

    #[test]
    fn copies_every_byte_including_a_multi_buffer_payload() {
        let payload = (0..COPY_BUFFER * 2 + 17)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut source = std::io::Cursor::new(payload.clone());
        let mut target = Vec::new();
        let written = copy_stream(&mut source, &mut target).unwrap();
        assert_eq!(written, payload.len() as u64);
        assert_eq!(target, payload);
    }

    #[test]
    fn copies_nothing_from_an_empty_reader() {
        let mut source = std::io::Cursor::new(Vec::new());
        let mut target = Vec::new();
        assert_eq!(copy_stream(&mut source, &mut target).unwrap(), 0);
        assert!(target.is_empty());
    }
}
