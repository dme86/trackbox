//! A streaming FLAC encoder (RFC 9639) for the mix recording: 24-bit stereo,
//! fixed blocks of 4096, stereo decorrelation, fixed predictors, partitioned
//! Rice residuals. No LPC; the MD5 stays zero, which the format reads as unknown.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bit packing narrows on purpose; every field is masked to its width"
)]

use std::io::{self, Seek, SeekFrom, Write};

const BLOCK: usize = 4096;
const BPS: u32 = 24;
const MAX_PARTITION_ORDER: u32 = 8;
const SAMPLE_RANGE: std::ops::RangeInclusive<i32> = -(1 << 23)..=(1 << 23) - 1;

/// Writes a FLAC stream frame by frame; `finish` patches STREAMINFO.
pub struct FlacWriter<W: Write + Seek> {
    out: W,
    /// Where `fLaC` starts, so STREAMINFO can be found again.
    start: u64,
    sample_rate: u32,
    frames: u64,
    frame_number: u64,
    /// Frame sizes in bytes; 0 until a frame is written.
    min_frame: u32,
    max_frame: u32,
    left: Vec<i32>,
    right: Vec<i32>,
    mid: Vec<i32>,
    side: Vec<i32>,
    residual: Vec<i64>,
    bits: BitWriter,
}

impl<W: Write + Seek> FlacWriter<W> {
    /// Starts a stream: `fLaC` and a STREAMINFO with the length still unknown.
    pub fn new(mut out: W, sample_rate: u32) -> io::Result<Self> {
        if !(1..=655_350).contains(&sample_rate) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "FLAC cannot store this sample rate"));
        }
        let start = out.stream_position()?;
        let mut writer = Self {
            out,
            start,
            sample_rate,
            frames: 0,
            frame_number: 0,
            min_frame: 0,
            max_frame: 0,
            left: Vec::with_capacity(BLOCK),
            right: Vec::with_capacity(BLOCK),
            mid: Vec::with_capacity(BLOCK),
            side: Vec::with_capacity(BLOCK),
            residual: Vec::with_capacity(BLOCK),
            bits: BitWriter::default(),
        };
        writer.out.write_all(b"fLaC")?;
        // Last metadata block, type STREAMINFO, 34 bytes.
        writer.out.write_all(&[0x80, 0, 0, 34])?;
        let info = writer.stream_info();
        writer.out.write_all(&info)?;
        Ok(writer)
    }

    /// Interleaved stereo frames, each sample a signed 24-bit value in an `i32`.
    pub fn write(&mut self, interleaved: &[i32]) -> io::Result<()> {
        if interleaved.len() % 2 != 0 || interleaved.iter().any(|s| !SAMPLE_RANGE.contains(s)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FLAC takes whole stereo frames of 24-bit samples",
            ));
        }
        for pair in interleaved.chunks_exact(2) {
            self.left.push(pair[0]);
            self.right.push(pair[1]);
            self.frames += 1;
            if self.left.len() == BLOCK {
                self.encode_block()?;
            }
        }
        Ok(())
    }

    /// Stereo frames taken so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Encodes what is left and writes the final length into STREAMINFO.
    pub fn finish(mut self) -> io::Result<W> {
        self.encode_block()?;
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(self.start + 8))?;
        let info = self.stream_info();
        self.out.write_all(&info)?;
        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn stream_info(&self) -> Vec<u8> {
        let total = if self.frames < 1 << 36 { self.frames } else { 0 };
        let mut info = BitWriter::default();
        info.put(BLOCK as u64, 16);
        info.put(BLOCK as u64, 16);
        info.put(u64::from(self.min_frame), 24);
        info.put(u64::from(self.max_frame), 24);
        info.put(u64::from(self.sample_rate), 20);
        info.put(2 - 1, 3);
        info.put(u64::from(BPS - 1), 5);
        info.put(total, 36);
        info.bytes.resize(34, 0);
        info.bytes
    }

    /// Encodes the buffered frames, a full block or the short last one.
    fn encode_block(&mut self) -> io::Result<()> {
        let n = self.left.len();
        if n == 0 {
            return Ok(());
        }
        self.mid.clear();
        self.side.clear();
        for (&l, &r) in self.left.iter().zip(&self.right) {
            self.mid.push((l + r) >> 1);
            self.side.push(l - r);
        }
        let signals: [(&[i32], u32); 4] = [(&self.left, BPS), (&self.right, BPS), (&self.mid, BPS), (&self.side, BPS + 1)];
        let plans = signals.map(|(x, bps)| plan(x, bps, &mut self.residual));
        // Channel assignment code and the two signals it stores, cheapest wins.
        let (assignment, a, b) = [(0b0001, 0, 1), (0b1000, 0, 3), (0b1001, 3, 1), (0b1010, 2, 3)]
            .into_iter()
            .min_by_key(|&(_, a, b)| plans[a].bits + plans[b].bits)
            .unwrap_or((0b0001, 0, 1));

        self.bits.bytes.clear();
        frame_header(&mut self.bits, self.sample_rate, self.frame_number, n, assignment);
        for i in [a, b] {
            let (x, bps) = signals[i];
            write_subframe(&mut self.bits, x, bps, &plans[i], &mut self.residual);
        }
        self.bits.align();
        let crc = crc16(&self.bits.bytes);
        self.bits.bytes.extend_from_slice(&crc.to_be_bytes());
        self.out.write_all(&self.bits.bytes)?;

        let size = self.bits.bytes.len() as u32;
        self.min_frame = if self.min_frame == 0 { size } else { self.min_frame.min(size) };
        self.max_frame = self.max_frame.max(size);
        self.frame_number += 1;
        self.left.clear();
        self.right.clear();
        Ok(())
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    /// Bits in `acc` not yet in `bytes`, always under 8 between calls.
    pending: u32,
}

impl BitWriter {
    /// The low `bits` of `value`, most significant first; `bits` at most 56.
    fn put(&mut self, value: u64, bits: u32) {
        self.acc = (self.acc << bits) | (value & ((1 << bits) - 1));
        self.pending += bits;
        while self.pending >= 8 {
            self.pending -= 8;
            self.bytes.push((self.acc >> self.pending) as u8);
        }
    }

    fn put_signed(&mut self, value: i32, bits: u32) {
        self.put(i64::from(value) as u64, bits);
    }

    /// Unary quotient (zeros closed by a one), then the low `k` bits.
    fn put_rice(&mut self, value: u64, k: u32) {
        let mut q = value >> k;
        while q >= 32 {
            self.put(0, 32);
            q -= 32;
        }
        self.put(1, q as u32 + 1);
        self.put(value, k);
    }

    fn align(&mut self) {
        if self.pending > 0 {
            self.put(0, 8 - self.pending);
        }
    }
}

fn frame_header(bits: &mut BitWriter, sample_rate: u32, frame_number: u64, n: usize, assignment: u64) {
    let (size_code, size_extra) = match n {
        BLOCK => (0b1100, None),
        1..=256 => (0b0110, Some(8)),
        _ => (0b0111, Some(16)),
    };
    let rate_code = match sample_rate {
        88_200 => 0b0001,
        176_400 => 0b0010,
        192_000 => 0b0011,
        8_000 => 0b0100,
        16_000 => 0b0101,
        22_050 => 0b0110,
        24_000 => 0b0111,
        32_000 => 0b1000,
        44_100 => 0b1001,
        48_000 => 0b1010,
        96_000 => 0b1011,
        _ => 0b0000,
    };
    // Sync code, reserved bit, fixed blocking.
    bits.put(0b1111_1111_1111_1000, 16);
    bits.put(size_code, 4);
    bits.put(rate_code, 4);
    bits.put(assignment, 4);
    bits.put(0b110, 3);
    bits.put(0, 1);
    put_coded_number(bits, frame_number);
    if let Some(width) = size_extra {
        bits.put(n as u64 - 1, width);
    }
    let crc = crc8(&bits.bytes);
    bits.put(u64::from(crc), 8);
}

/// The frame number in FLAC's UTF-8-like coding (up to 36 bits).
fn put_coded_number(bits: &mut BitWriter, value: u64) {
    if value < 0x80 {
        bits.put(value, 8);
        return;
    }
    // n bytes carry 5n + 1 bits.
    let significant = 64 - value.leading_zeros();
    let n = (significant - 1).div_ceil(5).max(2);
    bits.put(((0xFF00 >> n) & 0xFF) | (value >> (6 * (n - 1))), 8);
    for i in (0..n - 1).rev() {
        bits.put(0x80 | ((value >> (6 * i)) & 0x3F), 8);
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Constant,
    Verbatim,
    Fixed { order: usize, partition_order: u32, rice2: bool },
}

/// How one channel's subframe is coded, and what it costs in bits.
#[derive(Clone, Copy)]
struct Plan {
    bits: u64,
    kind: Kind,
    params: [u8; 1 << MAX_PARTITION_ORDER],
}

fn plan(x: &[i32], bps: u32, residual: &mut Vec<i64>) -> Plan {
    const HEADER: u64 = 8;
    let mut params = [0; 1 << MAX_PARTITION_ORDER];
    if x.iter().all(|&s| s == x[0]) {
        return Plan { bits: HEADER + u64::from(bps), kind: Kind::Constant, params };
    }
    let verbatim = Plan { bits: HEADER + x.len() as u64 * u64::from(bps), kind: Kind::Verbatim, params };

    // At least two samples here, or the block would be constant.
    let max_order = 4.min(x.len() - 1);
    let order = (0..=max_order)
        .min_by_key(|&o| (max_order..x.len()).map(|i| residual_at(x, o, i).unsigned_abs()).sum::<u64>())
        .unwrap_or(0);
    fixed_residual(x, order, residual);

    let n = x.len();
    let mut best: Option<(u64, u32, bool)> = None;
    for partition_order in 0..=MAX_PARTITION_ORDER {
        let len = n >> partition_order;
        if len << partition_order != n || len <= order {
            break;
        }
        let mut level = [0; 1 << MAX_PARTITION_ORDER];
        let mut bits = 2 + 4;
        let mut start = 0;
        for (j, param) in level.iter_mut().enumerate().take(1 << partition_order) {
            let end = (j + 1) * len - order;
            let (k, cost) = rice_param(&residual[start..end]);
            *param = k;
            bits += cost;
            start = end;
        }
        let rice2 = level.iter().any(|&k| k > 14);
        bits += (1 << partition_order) * if rice2 { 5 } else { 4 };
        if best.is_none_or(|(b, ..)| bits < b) {
            best = Some((bits, partition_order, rice2));
            params = level;
        }
    }
    let Some((residual_bits, partition_order, rice2)) = best else { return verbatim };
    let bits = HEADER + order as u64 * u64::from(bps) + residual_bits;
    if bits < verbatim.bits {
        Plan { bits, kind: Kind::Fixed { order, partition_order, rice2 }, params }
    } else {
        verbatim
    }
}

/// The fixed predictor's error at sample `i` (`i >= order`).
fn residual_at(x: &[i32], order: usize, i: usize) -> i64 {
    let s = |back: usize| i64::from(x[i - back]);
    match order {
        0 => s(0),
        1 => s(0) - s(1),
        2 => s(0) - 2 * s(1) + s(2),
        3 => s(0) - 3 * s(1) + 3 * s(2) - s(3),
        _ => s(0) - 4 * s(1) + 6 * s(2) - 4 * s(3) + s(4),
    }
}

fn fixed_residual(x: &[i32], order: usize, residual: &mut Vec<i64>) {
    residual.clear();
    residual.extend((order..x.len()).map(|i| residual_at(x, order, i)));
}

fn zigzag(r: i64) -> u64 {
    ((r << 1) ^ (r >> 63)) as u64
}

/// The cheapest Rice parameter for a partition near its mean's log2, with its exact cost.
fn rice_param(part: &[i64]) -> (u8, u64) {
    let len = part.len() as u64;
    let mean = part.iter().map(|&r| zigzag(r)).sum::<u64>() / len;
    let guess = mean.max(1).ilog2().min(30);
    let cost = |k: u32| part.iter().map(|&r| zigzag(r) >> k).sum::<u64>() + len * u64::from(1 + k);
    (guess.saturating_sub(1)..=(guess + 1).min(30))
        .map(|k| (k as u8, cost(k)))
        .min_by_key(|&(_, c)| c)
        .unwrap_or((30, u64::MAX))
}

fn write_subframe(bits: &mut BitWriter, x: &[i32], bps: u32, plan: &Plan, residual: &mut Vec<i64>) {
    // Each header: zero pad bit, 6-bit type, wasted-bits flag 0.
    match plan.kind {
        Kind::Constant => {
            bits.put(0b0000_0000, 8);
            bits.put_signed(x[0], bps);
        }
        Kind::Verbatim => {
            bits.put(0b0000_0010, 8);
            for &s in x {
                bits.put_signed(s, bps);
            }
        }
        Kind::Fixed { order, partition_order, rice2 } => {
            bits.put((0b1000 | order as u64) << 1, 8);
            for &s in &x[..order] {
                bits.put_signed(s, bps);
            }
            fixed_residual(x, order, residual);
            bits.put(u64::from(rice2), 2);
            bits.put(u64::from(partition_order), 4);
            let width = if rice2 { 5 } else { 4 };
            let len = x.len() >> partition_order;
            let mut start = 0;
            for (j, &k) in plan.params.iter().enumerate().take(1 << partition_order) {
                let end = (j + 1) * len - order;
                bits.put(u64::from(k), width);
                for &r in &residual[start..end] {
                    bits.put_rice(zigzag(r), u32::from(k));
                }
                start = end;
            }
        }
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::cast_precision_loss)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    const FULL: f64 = 8_388_607.0;

    struct Decoded {
        sample_rate: Option<u32>,
        bits: Option<u32>,
        n_frames: Option<u64>,
        samples: Vec<i32>,
    }

    fn decode(bytes: Vec<u8>) -> Decoded {
        let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes)), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("flac");
        let mut format = symphonia::default::get_probe()
            .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
            .unwrap()
            .format;
        let params = format.default_track().unwrap().codec_params.clone();
        let mut decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).unwrap();
        let mut samples = Vec::new();
        while let Ok(packet) = format.next_packet() {
            let audio = decoder.decode(&packet).unwrap();
            let mut buffer = SampleBuffer::<i32>::new(audio.capacity() as u64, *audio.spec());
            buffer.copy_interleaved_ref(audio);
            // symphonia widens 24-bit FLAC to the top of an i32.
            samples.extend(buffer.samples().iter().map(|s| s >> 8));
        }
        Decoded { sample_rate: params.sample_rate, bits: params.bits_per_sample, n_frames: params.n_frames, samples }
    }

    /// Writes in uneven chunks so blocks straddle calls and the last one is short.
    fn encode(samples: &[i32], sample_rate: u32) -> Vec<u8> {
        let mut writer = FlacWriter::new(Cursor::new(Vec::new()), sample_rate).unwrap();
        let mut rest = samples;
        for frames in [1, 777, 4096, 10_000].into_iter().cycle() {
            if rest.is_empty() {
                break;
            }
            let (chunk, tail) = rest.split_at((frames * 2).min(rest.len()));
            writer.write(chunk).unwrap();
            rest = tail;
        }
        assert_eq!(writer.frames(), samples.len() as u64 / 2);
        writer.finish().unwrap().into_inner()
    }

    /// STREAMINFO's 36-bit total sample count.
    fn total_samples(bytes: &[u8]) -> u64 {
        u64::from_be_bytes(bytes[18..26].try_into().unwrap()) & ((1 << 36) - 1)
    }

    /// Set `TRACKBOX_FLAC_DUMP=<dir>` to keep a file for `flac -t`.
    fn dump(name: &str, bytes: &[u8]) {
        if let Some(dir) = std::env::var_os("TRACKBOX_FLAC_DUMP") {
            std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
        }
    }

    fn lcg(state: &mut u64) -> i32 {
        *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (*state >> 40) as i32 - (1 << 23)
    }

    fn sine(i: usize, hz: f64, rate: f64, amp: f64) -> i32 {
        (amp * FULL * (std::f64::consts::TAU * hz * i as f64 / rate).sin()).round() as i32
    }

    fn assert_round_trip(samples: &[i32], sample_rate: u32, name: &str) -> usize {
        let bytes = encode(samples, sample_rate);
        dump(name, &bytes);
        let frames = samples.len() as u64 / 2;
        assert_eq!(total_samples(&bytes), frames);
        let len = bytes.len();
        let decoded = decode(bytes);
        assert_eq!(decoded.sample_rate, Some(sample_rate));
        assert_eq!(decoded.bits, Some(24));
        assert_eq!(decoded.n_frames, Some(frames));
        assert_eq!(decoded.samples.len(), samples.len());
        if let Some(i) = decoded.samples.iter().zip(samples).position(|(a, b)| a != b) {
            panic!("sample {i} differs: {} != {}", decoded.samples[i], samples[i]);
        }
        len
    }

    fn music(seconds: f64, rate: u32) -> Vec<i32> {
        let r = f64::from(rate);
        let frames = (seconds * r) as usize;
        let mut state = 1;
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f64 / r;
            let (l, rr) = if (1.0..1.5).contains(&t) {
                (0, 0)
            } else if (1.5..2.3).contains(&t) {
                let s = sine(i, 220.0, r, 0.7);
                (s, s)
            } else {
                (sine(i, 440.0, r, 0.5), (sine(i, 1234.5, r, 0.3) + lcg(&mut state) / 128).clamp(-(1 << 23), (1 << 23) - 1))
            };
            out.extend([l, rr]);
        }
        out
    }

    #[test]
    fn music_silence_and_mono_round_trip_bit_exact() {
        let samples = music(3.3, 44_100);
        assert_ne!((samples.len() / 2) % BLOCK, 0, "the last block must be short");
        let len = assert_round_trip(&samples, 44_100, "music.flac");
        eprintln!("music: {len} bytes, {:.1}% of raw 24-bit", 100.0 * len as f64 / (samples.len() * 3) as f64);
    }

    #[test]
    fn full_scale_extremes_round_trip_at_48k() {
        let rate = 48_000;
        let mut state = 7;
        let mut samples = Vec::new();
        for i in 0..40_000 {
            let edge = if i % 2 == 0 { -(1 << 23) } else { (1 << 23) - 1 };
            let (l, r) = match i / 10_000 {
                0 => (edge, edge),
                1 => (edge, -edge - 1),
                2 => (edge, (1 << 23) - 1),
                _ => (lcg(&mut state), edge),
            };
            samples.extend([l, r]);
        }
        // A loud sine with ±2^20 noise: residuals near 2^21 need Rice parameters above 14.
        let loud: Vec<i32> = (0..BLOCK)
            .map(|i| (sine(i, 1000.0, 48_000.0, 0.85) + (lcg(&mut state) >> 3)).clamp(-(1 << 23), (1 << 23) - 1))
            .collect();
        let mut residual = Vec::new();
        assert!(matches!(plan(&loud, BPS, &mut residual).kind, Kind::Fixed { rice2: true, .. }));
        for (i, &l) in loud.iter().enumerate() {
            samples.extend([l, -loud[BLOCK - 1 - i] - 1]);
        }
        assert_round_trip(&samples, rate, "extremes.flac");
    }

    #[test]
    fn silence_and_sine_compress() {
        let silence = vec![0; 441_000 * 2];
        let raw = silence.len() * 3;
        let len = assert_round_trip(&silence, 44_100, "silence.flac");
        eprintln!("silence: {len} bytes, {:.3}% of raw", 100.0 * len as f64 / raw as f64);
        assert!(len * 50 < raw);

        let tone: Vec<i32> = (0..441_000).flat_map(|i| [sine(i, 440.0, 44_100.0, 0.5), sine(i, 660.0, 44_100.0, 0.5)]).collect();
        let len = assert_round_trip(&tone, 44_100, "sine.flac");
        eprintln!("sine: {len} bytes, {:.1}% of raw", 100.0 * len as f64 / raw as f64);
        assert!(len * 4 < raw * 3);
    }

    #[test]
    fn an_unfinished_stream_decodes_up_to_its_last_frame() {
        let samples = music(3.3, 44_100);
        let mut writer = FlacWriter::new(Cursor::new(Vec::new()), 44_100).unwrap();
        for chunk in samples.chunks(2 * 1000) {
            writer.write(chunk).unwrap();
        }
        let bytes = writer.out.into_inner();
        assert_eq!(total_samples(&bytes), 0);
        let decoded = decode(bytes);
        assert_eq!(decoded.n_frames, None);
        let whole = samples.len() / 2 / BLOCK * BLOCK * 2;
        assert_eq!(decoded.samples.len(), whole);
        assert_eq!(decoded.samples, samples[..whole]);
    }

    #[test]
    fn coded_numbers_match_the_spec() {
        for (value, expected) in [
            (0x7F, vec![0x7F]),
            (0x80, vec![0xC2, 0x80]),
            (0x7FF, vec![0xDF, 0xBF]),
            (0x800, vec![0xE0, 0xA0, 0x80]),
            ((1 << 36) - 1, vec![0xFE, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF]),
        ] {
            let mut bits = BitWriter::default();
            put_coded_number(&mut bits, value);
            assert_eq!(bits.bytes, expected, "{value:#x}");
        }
    }

    #[test]
    fn rejects_samples_outside_24_bits() {
        let mut writer = FlacWriter::new(Cursor::new(Vec::new()), 44_100).unwrap();
        assert!(writer.write(&[1 << 23, 0]).is_err());
        assert!(writer.write(&[0]).is_err());
        assert_eq!(writer.frames(), 0);
    }
}
