//! Terminal image protocol encoders for the viewport strip: Kitty Graphics
//! Protocol (PNG payload + Unicode placeholders), DEC Sixel (popularity-palette
//! quantizer) and iTerm2 `OSC 1337` inline images. All encoders append into
//! caller-owned buffers so steady-state frames do not allocate.

use std::io::Write;

use rustc_hash::FxHashMap;

use super::painter::Rgb;

/// Kitty Unicode placeholder base character (`U+10EEEE`).
pub const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// Kitty `gen/rowcolumn-diacritics.txt`: the combining mark at index `n`
/// encodes row / column `n`.
pub const KITTY_DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// Largest row / column count a placeholder grid can address.
pub const KITTY_MAX_CELLS: u16 = KITTY_DIACRITICS.len() as u16;

/// Base64 payload bytes per Kitty APC chunk (protocol limit 4096).
const KITTY_CHUNK: usize = 4096;

/// Appends standard (padded) base64 of `bytes` to `out`.
pub fn base64_into(bytes: &[u8], out: &mut Vec<u8>) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    out.reserve(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize]);
            } else {
                out.push(b'=');
            }
        }
    }
}

/// Encodes opaque RGBA rows as a fast-compressed PNG into `out` (cleared first).
pub fn encode_png(rgba: &[u8], width: u32, height: u32, out: &mut Vec<u8>) -> bool {
    out.clear();
    let mut encoder = png::Encoder::new(&mut *out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let Ok(mut writer) = encoder.write_header() else {
        return false;
    };
    writer.write_image_data(rgba).is_ok() && writer.finish().is_ok()
}

/// Writes the placeholder grapheme for image cell `(row, col)` into `buf`:
/// `U+10EEEE` + row diacritic + column diacritic (both always explicit so partial
/// redraws by the ratatui diff stay unambiguous). No heap allocation.
pub fn kitty_placeholder_into(buf: &mut [u8; 16], row: u16, col: u16) -> &str {
    let mut len = KITTY_PLACEHOLDER.encode_utf8(&mut buf[..]).len();
    for n in [row, col] {
        let cp = KITTY_DIACRITICS[usize::from(n).min(KITTY_DIACRITICS.len() - 1)];
        let c = char::from_u32(cp).expect("valid diacritic code point");
        len += c.encode_utf8(&mut buf[len..]).len();
    }
    std::str::from_utf8(&buf[..len]).expect("utf-8 placeholder")
}

/// Transmits a PNG as image `id` with a virtual (`U=1`) placement spanning
/// `cols x rows` cells; responses are suppressed (`q=2`) so nothing leaks into
/// the input stream.
pub fn kitty_transmit(
    png: &[u8],
    id: u32,
    cols: u16,
    rows: u16,
    b64: &mut Vec<u8>,
    out: &mut Vec<u8>,
) {
    b64.clear();
    base64_into(png, b64);
    let n_chunks = b64.len().div_ceil(KITTY_CHUNK).max(1);
    for i in 0..n_chunks {
        let chunk = &b64[(i * KITTY_CHUNK).min(b64.len())..((i + 1) * KITTY_CHUNK).min(b64.len())];
        let more = u8::from(i + 1 != n_chunks);
        out.extend_from_slice(b"\x1b_G");
        if i == 0 {
            let _ = write!(
                out,
                "a=T,f=100,t=d,i={id},U=1,c={cols},r={rows},q=2,m={more}"
            );
        } else {
            let _ = write!(out, "m={more}");
        }
        out.push(b';');
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// Deletes image `id` together with its placements and stored data.
pub fn kitty_delete(id: u32, out: &mut Vec<u8>) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
}

/// `DECSC` + absolute cursor move to the 0-based cell `(x, y)`.
pub fn cursor_save_and_move(x: u16, y: u16, out: &mut Vec<u8>) {
    let _ = write!(out, "\x1b7\x1b[{};{}H", u32::from(y) + 1, u32::from(x) + 1);
}

/// `DECRC`.
pub fn cursor_restore(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b8");
}

/// iTerm2 / WezTerm `OSC 1337 File=` inline image stretched over `cols x rows` cells.
pub fn iterm2_inline(png: &[u8], cols: u16, rows: u16, b64: &mut Vec<u8>, out: &mut Vec<u8>) {
    b64.clear();
    base64_into(png, b64);
    let _ = write!(
        out,
        "\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=0:",
        png.len()
    );
    out.extend_from_slice(b64);
    out.push(0x07);
}

/// Reusable scratch state for [`encode_sixel`].
#[derive(Default)]
pub struct SixelScratch {
    histogram: FxHashMap<Rgb, u32>,
    ranked: Vec<(Rgb, u32)>,
    lookup: FxHashMap<Rgb, u8>,
    palette: Vec<Rgb>,
    indices: Vec<u8>,
    band_bits: Vec<u8>,
    band_colors: Vec<u8>,
}

/// Maximum Sixel palette registers used.
pub const SIXEL_MAX_COLORS: usize = 256;

/// Encodes the top `floor(height / 6) * 6` rows of opaque RGBA pixels as a
/// Sixel image. The height is truncated to whole 6-pixel bands so the image
/// never bleeds into the text row below the graph area.
///
/// Quantization is popularity-based: the 256 most frequent colors (row
/// backgrounds and solid track cores) stay exact, anti-aliased fringe pixels
/// snap to their nearest kept color. Returns the encoded height.
pub fn encode_sixel(
    rgba: &[u8],
    width: u32,
    height: u32,
    scratch: &mut SixelScratch,
    out: &mut Vec<u8>,
) -> u32 {
    let w = width as usize;
    let h = (height - height % 6) as usize;
    if w == 0 || h == 0 {
        return 0;
    }
    let px = |i: usize| -> Rgb {
        let o = i * 4;
        Rgb(rgba[o], rgba[o + 1], rgba[o + 2])
    };

    // 1. Histogram + popularity palette.
    // Long background runs dominate the strip: count runs, not pixels.
    scratch.histogram.clear();
    let mut run = (px(0), 0u32);
    for i in 0..w * h {
        let c = px(i);
        if c == run.0 {
            run.1 += 1;
        } else {
            *scratch.histogram.entry(run.0).or_insert(0) += run.1;
            run = (c, 1);
        }
    }
    *scratch.histogram.entry(run.0).or_insert(0) += run.1;
    scratch.ranked.clear();
    scratch
        .ranked
        .extend(scratch.histogram.iter().map(|(&c, &n)| (c, n)));
    scratch.ranked.sort_unstable_by(|a, b| {
        b.1.cmp(&a.1)
            .then((a.0.0, a.0.1, a.0.2).cmp(&(b.0.0, b.0.1, b.0.2)))
    });
    scratch.palette.clear();
    scratch.palette.extend(
        scratch
            .ranked
            .iter()
            .take(SIXEL_MAX_COLORS)
            .map(|&(c, _)| c),
    );

    // 2. Exact + nearest-color lookup for every distinct color.
    scratch.lookup.clear();
    for (i, &c) in scratch.palette.iter().enumerate() {
        scratch.lookup.insert(c, i as u8);
    }
    for &(c, _) in scratch.ranked.iter().skip(SIXEL_MAX_COLORS) {
        let idx = nearest(&scratch.palette, c);
        scratch.lookup.insert(c, idx);
    }
    scratch.indices.clear();
    let lookup = &scratch.lookup;
    let mut last: Option<(Rgb, u8)> = None;
    scratch.indices.extend((0..w * h).map(|i| {
        let c = px(i);
        let idx = match last {
            Some((prev, idx)) if prev == c => idx,
            _ => lookup[&c],
        };
        last = Some((c, idx));
        idx
    }));

    // 3. Header: P2=1 keeps unset bits transparent; raster attributes pin 1:1 pixels.
    let _ = write!(out, "\x1bP0;1;0q\"1;1;{w};{h}");
    for (i, &Rgb(r, g, b)) in scratch.palette.iter().enumerate() {
        let pct = |v: u8| (u32::from(v) * 100 + 127) / 255;
        let _ = write!(out, "#{i};2;{};{};{}", pct(r), pct(g), pct(b));
    }

    // 4. Bands of six pixel rows; one pass per color present in the band.
    let n_colors = scratch.palette.len();
    scratch.band_bits.clear();
    scratch.band_bits.resize(n_colors * w, 0);
    let bands = h / 6;
    for band in 0..bands {
        let y0 = band * 6;
        scratch.band_colors.clear();
        for r in 0..6 {
            let row = &scratch.indices[(y0 + r) * w..(y0 + r + 1) * w];
            for (x, &ci) in row.iter().enumerate() {
                let slot = &mut scratch.band_bits[usize::from(ci) * w + x];
                if *slot == 0 && !scratch.band_colors.contains(&ci) {
                    scratch.band_colors.push(ci);
                }
                *slot |= 1 << r;
            }
        }
        for (k, &ci) in scratch.band_colors.iter().enumerate() {
            if k > 0 {
                out.push(b'$');
            }
            let _ = write!(out, "#{ci}");
            let base = usize::from(ci) * w;
            let bits = &mut scratch.band_bits[base..base + w];
            let mut x = 0;
            while x < w {
                let b = bits[x];
                let mut run = 1;
                while x + run < w && bits[x + run] == b {
                    run += 1;
                }
                let ch = 63 + b;
                if run >= 4 {
                    let _ = write!(out, "!{run}");
                    out.push(ch);
                } else {
                    out.extend(std::iter::repeat_n(ch, run));
                }
                x += run;
            }
            bits.fill(0);
        }
        if band + 1 < bands {
            out.push(b'-');
        }
    }
    out.extend_from_slice(b"\x1b\\");
    h as u32
}

fn nearest(palette: &[Rgb], c: Rgb) -> u8 {
    let (r, g, b) = (i32::from(c.0), i32::from(c.1), i32::from(c.2));
    let mut best = (i32::MAX, 0u8);
    for (i, &p) in palette.iter().enumerate() {
        let dr = r - i32::from(p.0);
        let dg = g - i32::from(p.1);
        let db = b - i32::from(p.2);
        let d = dr * dr * 3 + dg * dg * 4 + db * db * 2;
        if d < best.0 {
            best = (d, i as u8);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference_vectors() {
        let enc = |b: &[u8]| {
            let mut v = Vec::new();
            base64_into(b, &mut v);
            String::from_utf8(v).unwrap()
        };
        assert_eq!(enc(b""), "");
        assert_eq!(enc(b"a"), "YQ==");
        assert_eq!(enc(b"ab"), "YWI=");
        assert_eq!(enc(b"abc"), "YWJj");
        assert_eq!(enc(b"0123456789abcdef"), "MDEyMzQ1Njc4OWFiY2RlZg==");
    }

    #[test]
    fn kitty_placeholder_is_single_cell_wide() {
        use unicode_width::UnicodeWidthStr;
        let mut buf = [0u8; 16];
        let s = kitty_placeholder_into(&mut buf, 0, 2);
        let cps: Vec<u32> = s.chars().map(|c| c as u32).collect();
        assert_eq!(cps, vec![0x10EEEE, 0x0305, 0x030E]);
        assert_eq!(s.width(), 1);
        assert_eq!(KITTY_MAX_CELLS, 297);
    }

    #[test]
    fn kitty_transmit_chunks_payload_and_suppresses_replies() {
        let png = vec![7u8; 10_000];
        let (mut b64, mut out) = (Vec::new(), Vec::new());
        kitty_transmit(&png, 42, 6, 3, &mut b64, &mut out);
        let s = String::from_utf8(out).unwrap();
        let chunks: Vec<&str> = s.split("\x1b\\").filter(|c| !c.is_empty()).collect();
        assert_eq!(chunks.len(), b64.len().div_ceil(KITTY_CHUNK));
        assert!(chunks[0].starts_with("\x1b_Ga=T,f=100,t=d,i=42,U=1,c=6,r=3,q=2,m=1;"));
        assert!(chunks[1].starts_with("\x1b_Gm=1;"));
        assert!(chunks.last().unwrap().starts_with("\x1b_Gm=0;"));
        for c in &chunks {
            let payload = c.split_once(';').unwrap().1;
            assert!(payload.len() <= KITTY_CHUNK);
        }
        let mut del = Vec::new();
        kitty_delete(42, &mut del);
        assert_eq!(del, b"\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    }

    #[test]
    fn iterm2_inline_declares_cell_extent() {
        let (mut b64, mut out) = (Vec::new(), Vec::new());
        iterm2_inline(b"abc", 4, 2, &mut b64, &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]1337;File=inline=1;size=3;width=4;height=2;preserveAspectRatio=0:YWJj\x07"
        );
    }

    #[test]
    fn png_round_trips() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| (i * 11) as u8 | 0x0f).collect();
        let mut out = Vec::new();
        assert!(encode_png(&rgba, 3, 2, &mut out));
        let decoder = png::Decoder::new(std::io::Cursor::new(&out));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(&buf[..info.buffer_size()], &rgba[..]);
    }

    /// Minimal Sixel decoder (palette `#i;2;r;g;b`, `#i` select, `!n` RLE, `$`, `-`).
    fn decode_sixel(data: &[u8]) -> (usize, usize, Vec<Option<Rgb>>) {
        let s = std::str::from_utf8(data).unwrap();
        let body = s
            .strip_prefix("\x1bP0;1;0q\"1;1;")
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap();
        let mut it = body.chars().peekable();
        let num = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
            let mut n = 0usize;
            while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                n = n * 10 + c.to_digit(10).unwrap() as usize;
                it.next();
            }
            n
        };
        let w = num(&mut it);
        assert_eq!(it.next(), Some(';'));
        let h = num(&mut it);
        let mut palette = vec![Rgb(0, 0, 0); 256];
        let mut img = vec![None; w * h];
        let (mut cur, mut x, mut band) = (0usize, 0usize, 0usize);
        while let Some(c) = it.next() {
            match c {
                '#' => {
                    cur = num(&mut it);
                    if it.peek() == Some(&';') {
                        it.next();
                        assert_eq!(num(&mut it), 2);
                        let mut ch = [0u8; 3];
                        for v in &mut ch {
                            it.next();
                            *v = ((num(&mut it) as u32 * 255 + 50) / 100) as u8;
                        }
                        palette[cur] = Rgb(ch[0], ch[1], ch[2]);
                    }
                }
                '$' => x = 0,
                '-' => {
                    x = 0;
                    band += 1;
                }
                '!' => {
                    let n = num(&mut it);
                    let ch = it.next().unwrap();
                    for _ in 0..n {
                        put(&mut img, w, h, x, band, ch, palette[cur]);
                        x += 1;
                    }
                }
                ch => {
                    put(&mut img, w, h, x, band, ch, palette[cur]);
                    x += 1;
                }
            }
        }
        (w, h, img)
    }

    fn put(
        img: &mut [Option<Rgb>],
        w: usize,
        h: usize,
        x: usize,
        band: usize,
        ch: char,
        color: Rgb,
    ) {
        let bits = ch as u32 - 63;
        for r in 0..6 {
            let y = band * 6 + r;
            if bits & (1 << r) != 0 && y < h && x < w {
                img[y * w + x] = Some(color);
            }
        }
    }

    #[test]
    fn sixel_round_trips_and_truncates_to_whole_bands() {
        let (w, h) = (13u32, 20u32);
        let colors = [
            Rgb(0x2b, 0x2b, 0x2b),
            Rgb(0xc7, 0x54, 0x50),
            Rgb(0x53, 0x94, 0xec),
            Rgb(0x62, 0xb5, 0x43),
        ];
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c = colors[((x / 3 + y / 4) % 4) as usize];
                rgba.extend_from_slice(&[c.0, c.1, c.2, 255]);
            }
        }
        let (mut scratch, mut out) = (SixelScratch::default(), Vec::new());
        let encoded_h = encode_sixel(&rgba, w, h, &mut scratch, &mut out);
        assert_eq!(encoded_h, 18);
        let (dw, dh, img) = decode_sixel(&out);
        assert_eq!((dw, dh), (13, 18));
        for y in 0..18 {
            for x in 0..13 {
                let want = colors[((x / 3 + y / 4) % 4) as usize];
                let got = img[(y * 13 + x) as usize].unwrap();
                for (a, b) in [(want.0, got.0), (want.1, got.1), (want.2, got.2)] {
                    let d = i32::from(a) - i32::from(b);
                    assert!(d.abs() <= 2, "({x},{y}) {want:?} vs {got:?}");
                }
            }
        }
    }

    #[test]
    fn sixel_quantizes_more_than_256_colors_to_nearest_popular() {
        let (w, h) = (40u32, 12u32);
        let mut rgba = Vec::new();
        for i in 0..w * h {
            // Background dominates; a gradient of fringe colors is sprinkled in.
            let c = if i % 2 == 0 {
                Rgb(0x2b, 0x2b, 0x2b)
            } else {
                let v = i * 7;
                Rgb(0x10, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8)
            };
            rgba.extend_from_slice(&[c.0, c.1, c.2, 255]);
        }
        let (mut scratch, mut out) = (SixelScratch::default(), Vec::new());
        encode_sixel(&rgba, w, h, &mut scratch, &mut out);
        assert!(scratch.palette.len() <= SIXEL_MAX_COLORS);
        assert_eq!(scratch.palette[0], Rgb(0x2b, 0x2b, 0x2b));
        let (_, _, img) = decode_sixel(&out);
        assert!(img.iter().all(Option::is_some), "every pixel painted");
    }
}
