//! SEI (§7.3.5): only `decoded_picture_hash` (payloadType 132, §D.3.19) is
//! interpreted — it is the decoder's built-in conformance self-check.

use crate::frame::Picture;
use crate::md5::Md5;
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::vec::Vec;

/// A decoded picture hash from a suffix SEI, one entry per colour plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureHash {
    Md5([[u8; 16]; 3]),
    Crc([u16; 3]),
    Checksum([u32; 3]),
}

/// Parses an SEI RBSP and returns the picture hash if one is present.
pub fn parse_picture_hash(rbsp: &[u8], chroma_format_idc: u8) -> Option<PictureHash> {
    let mut i = 0;
    let planes = if chroma_format_idc == 0 { 1 } else { 3 };
    while i + 2 <= rbsp.len() {
        let mut ptype = 0usize;
        while i < rbsp.len() && rbsp[i] == 0xff {
            ptype += 255;
            i += 1;
        }
        if i >= rbsp.len() {
            return None;
        }
        ptype += rbsp[i] as usize;
        i += 1;
        let mut psize = 0usize;
        while i < rbsp.len() && rbsp[i] == 0xff {
            psize += 255;
            i += 1;
        }
        if i >= rbsp.len() {
            return None;
        }
        psize += rbsp[i] as usize;
        i += 1;
        let payload = rbsp.get(i..i + psize)?;
        if ptype == 132 && !payload.is_empty() {
            let hash_type = payload[0];
            let body = &payload[1..];
            return match hash_type {
                0 => {
                    let mut h = [[0u8; 16]; 3];
                    for p in 0..planes {
                        h[p].copy_from_slice(body.get(p * 16..p * 16 + 16)?);
                    }
                    Some(PictureHash::Md5(h))
                }
                1 => {
                    let mut h = [0u16; 3];
                    for p in 0..planes {
                        h[p] = u16::from_be_bytes([*body.get(p * 2)?, *body.get(p * 2 + 1)?]);
                    }
                    Some(PictureHash::Crc(h))
                }
                2 => {
                    let mut h = [0u32; 3];
                    for p in 0..planes {
                        let b = body.get(p * 4..p * 4 + 4)?;
                        h[p] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                    }
                    Some(PictureHash::Checksum(h))
                }
                _ => None,
            };
        }
        i += psize;
        // rbsp_trailing_bits: stop at the 0x80 stop byte
        if i < rbsp.len() && rbsp[i] == 0x80 && i + 1 == rbsp.len() {
            break;
        }
    }
    None
}

/// Computes the hash of `pic` (full coded size, pre-crop) in the SEI's form.
pub fn compute(pic: &Picture, kind: &PictureHash) -> PictureHash {
    let planes = if pic.chroma_format_idc == 0 { 1 } else { 3 };
    match kind {
        PictureHash::Md5(_) => {
            let mut out = [[0u8; 16]; 3];
            for (p, o) in out.iter_mut().enumerate().take(planes) {
                let pl = &pic.planes[p];
                let depth = if p == 0 { pic.bit_depth_luma } else { pic.bit_depth_chroma };
                let mut m = Md5::new();
                let mut row = Vec::with_capacity(pl.width * 2);
                for y in 0..pl.height {
                    row.clear();
                    for &v in pl.row(y) {
                        if depth > 8 {
                            row.extend_from_slice(&v.to_le_bytes());
                        } else {
                            row.push(v as u8);
                        }
                    }
                    m.update(&row);
                }
                *o = m.finalize();
            }
            PictureHash::Md5(out)
        }
        PictureHash::Crc(_) => {
            let mut out = [0u16; 3];
            for (p, o) in out.iter_mut().enumerate().take(planes) {
                let pl = &pic.planes[p];
                let depth = if p == 0 { pic.bit_depth_luma } else { pic.bit_depth_chroma };
                let mut crc: u32 = 0xffff;
                for y in 0..pl.height {
                    for &v in pl.row(y) {
                        let bytes: &[u8] = if depth > 8 { &v.to_le_bytes() } else { &[v as u8] };
                        for &b in bytes {
                            for bit in 0..8 {
                                let msb = (crc >> 15) & 1;
                                let d = ((b >> (7 - bit)) & 1) as u32;
                                crc = (((crc << 1) + d) & 0xffff) ^ (msb * 0x1021);
                            }
                        }
                    }
                }
                for _ in 0..16 {
                    let msb = (crc >> 15) & 1;
                    crc = ((crc << 1) & 0xffff) ^ (msb * 0x1021);
                }
                *o = crc as u16;
            }
            PictureHash::Crc(out)
        }
        PictureHash::Checksum(_) => {
            let mut out = [0u32; 3];
            for (p, o) in out.iter_mut().enumerate().take(planes) {
                let pl = &pic.planes[p];
                let depth = if p == 0 { pic.bit_depth_luma } else { pic.bit_depth_chroma };
                let mut sum: u32 = 0;
                for y in 0..pl.height {
                    for (x, &v) in pl.row(y).iter().enumerate() {
                        let xor_mask = ((x & 0xff) ^ (y & 0xff) ^ (x >> 8) ^ (y >> 8)) as u32;
                        sum = sum.wrapping_add(((v as u32) & 0xff) ^ xor_mask);
                        if depth > 8 {
                            sum = sum.wrapping_add(((v as u32) >> 8) ^ xor_mask);
                        }
                    }
                }
                *o = sum;
            }
            PictureHash::Checksum(out)
        }
    }
}

/// Which planes differ between the SEI hash and the decoded picture (empty = match).
pub fn mismatched_planes(expected: &PictureHash, pic: &Picture) -> Vec<usize> {
    let planes = if pic.chroma_format_idc == 0 { 1 } else { 3 };
    let got = compute(pic, expected);
    let mut bad = Vec::new();
    for p in 0..planes {
        let same = match (expected, &got) {
            (PictureHash::Md5(a), PictureHash::Md5(b)) => a[p] == b[p],
            (PictureHash::Crc(a), PictureHash::Crc(b)) => a[p] == b[p],
            (PictureHash::Checksum(a), PictureHash::Checksum(b)) => a[p] == b[p],
            _ => false,
        };
        if !same {
            bad.push(p);
        }
    }
    bad
}
