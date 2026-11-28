//! An MJPEG-in-AVI writer that keeps real time.
//!
//! The browser sends a frame only when the screen changes, so frames arrive unevenly: none for
//! seconds while a page sits still, dozens in a second while it animates. Written as they come,
//! a video would rush through still moments. This writer plays at a constant rate instead:
//! each frame lands in the slot of its capture time, and a slot with no new frame holds the
//! one before, as a zero-length chunk, AVI's way of repeating a frame. A still page costs
//! almost nothing.

use std::io::{self, Seek, SeekFrom, Write};

/// The frame rate videos play at.
pub const FPS: u32 = 25;

/// Writes an MJPEG AVI to `out` as frames arrive; [`finish`](Self::finish) writes the index.
pub struct AviWriter<W: Write + Seek> {
    out: W,
    /// Each slot written so far: its chunk's offset, relative to the `movi` list, and size
    /// (zero for a repeat).
    slots: Vec<(u32, u32)>,
    /// The latest frame for the slot after the last written, not written until a later slot
    /// begins, so a slot shows its last frame.
    pending: Option<(u32, Vec<u8>)>,
    /// How many distinct frames have been written.
    frames: usize,
    /// The last frame written, to end the video on.
    last: Vec<u8>,
    /// Where the `movi` list's FourCC is, once the headers are written.
    movi: u64,
    /// Where `dwTotalFrames` and the stream's `dwLength` are, to patch at the end.
    total_frames_at: u64,
    length_at: u64,
}

impl<W: Write + Seek> AviWriter<W> {
    /// A writer for `out`, which receives the headers with the first frame.
    pub fn new(out: W) -> Self {
        AviWriter {
            out,
            slots: Vec::new(),
            pending: None,
            frames: 0,
            last: Vec::new(),
            movi: 0,
            total_frames_at: 0,
            length_at: 0,
        }
    }

    /// How many distinct frames have been written.
    #[cfg(test)]
    pub fn stored(&self) -> usize {
        self.frames + usize::from(self.pending.is_some())
    }

    fn slot_of(at: f64) -> u32 {
        (at.max(0.0) * f64::from(FPS)).floor() as u32
    }

    /// Adds a JPEG captured `at` seconds after the first.
    pub fn add(&mut self, jpeg: &[u8], at: f64) -> io::Result<()> {
        if self.movi == 0 {
            let (width, height) = jpeg_size(jpeg).unwrap_or((1280, 800));
            self.headers(width, height)?;
        }
        // A frame never goes back in time: one that arrives late joins the latest slot.
        let floor = self
            .pending
            .as_ref()
            .map_or(self.slots.len() as u32, |(pending, _)| *pending);
        let slot = Self::slot_of(at).max(floor);
        if let Some((pending_slot, _)) = &self.pending
            && *pending_slot < slot
        {
            self.flush_pending()?;
        }
        self.pending = Some((slot, jpeg.to_vec()));
        Ok(())
    }

    /// Writes the pending frame in its slot, after repeats for the slots before it.
    fn flush_pending(&mut self) -> io::Result<()> {
        let Some((slot, jpeg)) = self.pending.take() else {
            return Ok(());
        };
        self.repeat_until(slot)?;
        self.write_frame(&jpeg)?;
        self.frames += 1;
        self.last = jpeg;
        Ok(())
    }

    fn write_frame(&mut self, jpeg: &[u8]) -> io::Result<()> {
        let position = self.out.stream_position()?;
        self.out.write_all(b"00dc")?;
        self.out.write_all(&(jpeg.len() as u32).to_le_bytes())?;
        self.out.write_all(jpeg)?;
        if jpeg.len() % 2 == 1 {
            self.out.write_all(&[0])?;
        }
        self.slots
            .push(((position - self.movi) as u32, jpeg.len() as u32));
        Ok(())
    }

    /// Fills the slots before `slot` with repeats of the frame before them.
    fn repeat_until(&mut self, slot: u32) -> io::Result<()> {
        while (self.slots.len() as u32) < slot {
            let position = self.out.stream_position()?;
            self.out.write_all(b"00dc")?;
            self.out.write_all(&0u32.to_le_bytes())?;
            self.slots.push(((position - self.movi) as u32, 0));
        }
        Ok(())
    }

    /// Writes the last frame, holds it until `end` seconds, writes the index and patches the
    /// headers. Returns the stream, and how many frames the video plays.
    pub fn finish(mut self, end: f64) -> io::Result<(W, u32)> {
        if self.movi == 0 {
            self.headers(1, 1)?;
        } else {
            self.flush_pending()?;
            let last = Self::slot_of(end).max(self.slots.len() as u32 - 1);
            // The final slot holds a real copy of the last frame, not a repeat: a converter
            // gives the last frame one frame's time, and a repeat at the end would be lost
            // with everything held since.
            if last >= self.slots.len() as u32 {
                self.repeat_until(last)?;
                let jpeg = std::mem::take(&mut self.last);
                self.write_frame(&jpeg)?;
            }
        }
        let index_at = self.out.stream_position()?;
        let slots = self.slots.len() as u32;
        self.out.write_all(b"idx1")?;
        self.out.write_all(&(slots * 16).to_le_bytes())?;
        for &(offset, size) in &self.slots {
            self.out.write_all(b"00dc")?;
            // A repeat is not a key frame: it shows the one before.
            let flags: u32 = if size > 0 { 0x10 } else { 0 };
            self.out.write_all(&flags.to_le_bytes())?;
            self.out.write_all(&offset.to_le_bytes())?;
            self.out.write_all(&size.to_le_bytes())?;
        }
        let file_end = self.out.stream_position()?;
        // RIFF size, the movi list's size, and the frame counts.
        self.patch(4, (file_end - 8) as u32)?;
        self.patch(self.movi - 4, (index_at - self.movi) as u32)?;
        self.patch(self.total_frames_at, slots)?;
        self.patch(self.length_at, slots)?;
        self.out.seek(SeekFrom::Start(file_end))?;
        self.out.flush()?;
        Ok((self.out, slots))
    }

    fn patch(&mut self, at: u64, value: u32) -> io::Result<()> {
        self.out.seek(SeekFrom::Start(at))?;
        self.out.write_all(&value.to_le_bytes())
    }

    fn headers(&mut self, width: u32, height: u32) -> io::Result<()> {
        let mut h = Vec::with_capacity(224);
        let u32le = |h: &mut Vec<u8>, v: u32| h.extend_from_slice(&v.to_le_bytes());
        let u16le = |h: &mut Vec<u8>, v: u16| h.extend_from_slice(&v.to_le_bytes());
        h.extend_from_slice(b"RIFF");
        u32le(&mut h, 0); // patched
        h.extend_from_slice(b"AVI LIST");
        u32le(&mut h, 192); // hdrl: "hdrl" + avih (8 + 56) + LIST strl (12 + 64 + 48)
        h.extend_from_slice(b"hdrlavih");
        u32le(&mut h, 56);
        u32le(&mut h, 1_000_000 / FPS); // dwMicroSecPerFrame
        u32le(&mut h, 0); // dwMaxBytesPerSec
        u32le(&mut h, 0); // dwPaddingGranularity
        u32le(&mut h, 0x10); // dwFlags: AVIF_HASINDEX
        let total_frames_at = h.len() as u64;
        u32le(&mut h, 0); // dwTotalFrames, patched
        u32le(&mut h, 0); // dwInitialFrames
        u32le(&mut h, 1); // dwStreams
        u32le(&mut h, 0); // dwSuggestedBufferSize
        u32le(&mut h, width);
        u32le(&mut h, height);
        h.extend_from_slice(&[0; 16]); // dwReserved
        h.extend_from_slice(b"LIST");
        u32le(&mut h, 116); // "strl" + strh (8 + 56) + strf (8 + 40)
        h.extend_from_slice(b"strlstrh");
        u32le(&mut h, 56);
        h.extend_from_slice(b"vidsMJPG");
        u32le(&mut h, 0); // dwFlags
        u16le(&mut h, 0); // wPriority
        u16le(&mut h, 0); // wLanguage
        u32le(&mut h, 0); // dwInitialFrames
        u32le(&mut h, 1); // dwScale
        u32le(&mut h, FPS); // dwRate: FPS frames per second
        u32le(&mut h, 0); // dwStart
        let length_at = h.len() as u64;
        u32le(&mut h, 0); // dwLength, patched
        u32le(&mut h, 0); // dwSuggestedBufferSize
        u32le(&mut h, u32::MAX); // dwQuality: the default
        u32le(&mut h, 0); // dwSampleSize
        u16le(&mut h, 0);
        u16le(&mut h, 0);
        u16le(&mut h, width as u16);
        u16le(&mut h, height as u16);
        h.extend_from_slice(b"strf");
        u32le(&mut h, 40);
        u32le(&mut h, 40); // biSize
        u32le(&mut h, width);
        u32le(&mut h, height);
        u16le(&mut h, 1); // biPlanes
        u16le(&mut h, 24); // biBitCount
        h.extend_from_slice(b"MJPG");
        u32le(&mut h, width * height * 3); // biSizeImage
        h.extend_from_slice(&[0; 16]);
        h.extend_from_slice(b"LIST");
        u32le(&mut h, 0); // movi size, patched
        let movi = h.len() as u64;
        h.extend_from_slice(b"movi");

        let start = self.out.stream_position()?;
        self.out.write_all(&h)?;
        self.movi = start + movi;
        self.total_frames_at = start + total_frames_at;
        self.length_at = start + length_at;
        Ok(())
    }
}

/// A JPEG's width and height, from its start-of-frame marker.
pub fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2; // past the start-of-image marker
    while at + 9 < jpeg.len() {
        if jpeg[at] != 0xFF {
            return None;
        }
        let marker = jpeg[at + 1];
        let length = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        // SOF0 to SOF15, but for DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([jpeg[at + 5], jpeg[at + 6]]);
            let width = u16::from_be_bytes([jpeg[at + 7], jpeg[at + 8]]);
            return Some((u32::from(width), u32::from(height)));
        }
        at += 2 + length;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// The smallest JPEG-shaped bytes: SOI, a SOF0 for `w`x`h`, EOI.
    fn jpeg(w: u16, h: u16, fill: u8) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8];
        j.extend_from_slice(&h.to_be_bytes());
        j.extend_from_slice(&w.to_be_bytes());
        j.extend_from_slice(&[1, 1, 0x11, 0, fill, 0xFF, 0xD9]);
        j
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn frames_keep_real_time_and_are_stored_once() {
        let mut avi = AviWriter::new(Cursor::new(Vec::new()));
        avi.add(&jpeg(640, 360, 1), 0.0).unwrap();
        // Nothing changes for a second, then two frames in quick succession.
        avi.add(&jpeg(640, 360, 2), 1.0).unwrap();
        avi.add(&jpeg(640, 360, 3), 1.04).unwrap();
        assert_eq!(avi.stored(), 3);
        let (out, slots) = avi.finish(2.0).unwrap();
        let bytes = out.into_inner();
        // Two seconds at 25 fps, the last frame held to the end.
        assert_eq!(slots, 51);

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8);
        assert_eq!(&bytes[8..12], b"AVI ");
        assert_eq!(u32_at(&bytes, 0x40), 640, "width from the JPEG");
        assert_eq!(u32_at(&bytes, 0x44), 360);
        assert_eq!(u32_at(&bytes, 0x30), 51, "dwTotalFrames");
        let movi = bytes.windows(4).position(|w| w == b"movi").unwrap();
        let idx = bytes.windows(4).rposition(|w| w == b"idx1").unwrap();
        assert_eq!(
            u32_at(&bytes, movi - 4) as usize,
            idx - movi,
            "movi list size"
        );
        assert_eq!(u32_at(&bytes, idx + 4), 51 * 16);
        // Each slot's entry: a frame where one was captured, a zero-length repeat elsewhere.
        let entries: Vec<(u32, u32, u32)> = (0..51)
            .map(|n| {
                let at = idx + 8 + n * 16;
                (
                    u32_at(&bytes, at + 4),
                    u32_at(&bytes, at + 8),
                    u32_at(&bytes, at + 12),
                )
            })
            .collect();
        let frames: Vec<usize> = (0..51).filter(|n| entries[*n].2 > 0).collect();
        assert_eq!(
            frames,
            [0, 25, 26, 50],
            "frames at 0 s, 1 s and 1.04 s, and the last again at the end"
        );
        assert_eq!(
            entries[50].2, entries[26].2,
            "the end repeats the last frame"
        );
        for (n, &(flags, offset, size)) in entries.iter().enumerate() {
            let chunk = movi + offset as usize;
            assert_eq!(&bytes[chunk..chunk + 4], b"00dc", "slot {n}");
            assert_eq!(u32_at(&bytes, chunk + 4), size, "slot {n}");
            assert_eq!(
                flags,
                if size > 0 { 0x10 } else { 0 },
                "only frames are key frames"
            );
        }
        // Chunks follow one another in time order.
        assert!(entries.windows(2).all(|w| w[0].1 < w[1].1));
    }

    #[test]
    fn a_slot_shows_its_latest_frame_and_late_frames_do_not_go_back() {
        let mut avi = AviWriter::new(Cursor::new(Vec::new()));
        avi.add(&jpeg(10, 10, 1), 0.0).unwrap();
        avi.add(&jpeg(10, 10, 2), 0.01).unwrap(); // same slot: replaces frame 1
        avi.add(&jpeg(10, 10, 3), 0.5).unwrap();
        avi.add(&jpeg(10, 10, 4), 0.2).unwrap(); // late: joins slot 12, replacing frame 3
        let (out, slots) = avi.finish(0.5).unwrap();
        let bytes = out.into_inner();
        assert_eq!(slots, 13);
        let movi = bytes.windows(4).position(|w| w == b"movi").unwrap();
        let idx = bytes.windows(4).rposition(|w| w == b"idx1").unwrap();
        let fill = |n: usize| {
            let offset = u32_at(&bytes, idx + 8 + n * 16 + 8) as usize;
            // The fill byte is byte 15 of the test JPEG, after the 8-byte chunk header.
            bytes[movi + offset + 8 + 15]
        };
        assert_eq!(fill(0), 2, "slot 0 shows the later of its two frames");
        assert_eq!(fill(12), 4, "the late frame took the slot");
    }

    #[test]
    fn an_empty_video_is_still_a_valid_file() {
        let (out, slots) = AviWriter::new(Cursor::new(Vec::new())).finish(1.0).unwrap();
        let bytes = out.into_inner();
        assert_eq!(slots, 0);
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8);
    }

    #[test]
    fn sizes_come_from_the_start_of_frame_marker() {
        assert_eq!(jpeg_size(&jpeg(1280, 720, 0)), Some((1280, 720)));
        // A DHT segment before the SOF is skipped.
        let mut with_table = vec![0xFF, 0xD8, 0xFF, 0xC4, 0, 4, 0, 0];
        with_table.extend_from_slice(&jpeg(320, 200, 0)[2..]);
        assert_eq!(jpeg_size(&with_table), Some((320, 200)));
        assert_eq!(jpeg_size(b"not a jpeg"), None);
    }
}
