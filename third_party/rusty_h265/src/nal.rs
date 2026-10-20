//! NAL units (§7.3.1), Annex-B framing (Annex B) and emulation prevention.
//!
//! HEVC's NAL header is two bytes: `forbidden_zero_bit(1) nal_unit_type(6)
//! nuh_layer_id(6) nuh_temporal_id_plus1(3)`. The payload escaping is H.264's
//! (`00 00 0x` with `x ≤ 3` gets a `03` inserted). Unescaping records where
//! each emulation-prevention byte sat, because slice `entry_point_offset`s
//! count them (§7.4.7.1: the offsets address the NAL unit bytes, not the RBSP).

// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::vec::Vec;

/// `nal_unit_type` (Table 7-1). Values 0..=63.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NalType {
    TrailN = 0,
    TrailR = 1,
    TsaN = 2,
    TsaR = 3,
    StsaN = 4,
    StsaR = 5,
    RadlN = 6,
    RadlR = 7,
    RaslN = 8,
    RaslR = 9,
    RsvVclN10 = 10,
    RsvVclR11 = 11,
    RsvVclN12 = 12,
    RsvVclR13 = 13,
    RsvVclN14 = 14,
    RsvVclR15 = 15,
    BlaWLp = 16,
    BlaWRadl = 17,
    BlaNLp = 18,
    IdrWRadl = 19,
    IdrNLp = 20,
    CraNut = 21,
    RsvIrapVcl22 = 22,
    RsvIrapVcl23 = 23,
    RsvVcl24 = 24,
    RsvVcl25 = 25,
    RsvVcl26 = 26,
    RsvVcl27 = 27,
    RsvVcl28 = 28,
    RsvVcl29 = 29,
    RsvVcl30 = 30,
    RsvVcl31 = 31,
    Vps = 32,
    Sps = 33,
    Pps = 34,
    Aud = 35,
    Eos = 36,
    Eob = 37,
    Fd = 38,
    PrefixSei = 39,
    SuffixSei = 40,
    Other(u8),
}

impl NalType {
    pub fn from_id(v: u8) -> Self {
        use NalType::*;
        match v & 0x3f {
            0 => TrailN,
            1 => TrailR,
            2 => TsaN,
            3 => TsaR,
            4 => StsaN,
            5 => StsaR,
            6 => RadlN,
            7 => RadlR,
            8 => RaslN,
            9 => RaslR,
            10 => RsvVclN10,
            11 => RsvVclR11,
            12 => RsvVclN12,
            13 => RsvVclR13,
            14 => RsvVclN14,
            15 => RsvVclR15,
            16 => BlaWLp,
            17 => BlaWRadl,
            18 => BlaNLp,
            19 => IdrWRadl,
            20 => IdrNLp,
            21 => CraNut,
            22 => RsvIrapVcl22,
            23 => RsvIrapVcl23,
            24 => RsvVcl24,
            25 => RsvVcl25,
            26 => RsvVcl26,
            27 => RsvVcl27,
            28 => RsvVcl28,
            29 => RsvVcl29,
            30 => RsvVcl30,
            31 => RsvVcl31,
            32 => Vps,
            33 => Sps,
            34 => Pps,
            35 => Aud,
            36 => Eos,
            37 => Eob,
            38 => Fd,
            39 => PrefixSei,
            40 => SuffixSei,
            o => Other(o),
        }
    }

    pub fn id(self) -> u8 {
        match self {
            NalType::Other(v) => v,
            // SAFETY-free: every unit variant carries its discriminant.
            t => t.discriminant(),
        }
    }

    fn discriminant(self) -> u8 {
        use NalType::*;
        match self {
            TrailN => 0,
            TrailR => 1,
            TsaN => 2,
            TsaR => 3,
            StsaN => 4,
            StsaR => 5,
            RadlN => 6,
            RadlR => 7,
            RaslN => 8,
            RaslR => 9,
            RsvVclN10 => 10,
            RsvVclR11 => 11,
            RsvVclN12 => 12,
            RsvVclR13 => 13,
            RsvVclN14 => 14,
            RsvVclR15 => 15,
            BlaWLp => 16,
            BlaWRadl => 17,
            BlaNLp => 18,
            IdrWRadl => 19,
            IdrNLp => 20,
            CraNut => 21,
            RsvIrapVcl22 => 22,
            RsvIrapVcl23 => 23,
            RsvVcl24 => 24,
            RsvVcl25 => 25,
            RsvVcl26 => 26,
            RsvVcl27 => 27,
            RsvVcl28 => 28,
            RsvVcl29 => 29,
            RsvVcl30 => 30,
            RsvVcl31 => 31,
            Vps => 32,
            Sps => 33,
            Pps => 34,
            Aud => 35,
            Eos => 36,
            Eob => 37,
            Fd => 38,
            PrefixSei => 39,
            SuffixSei => 40,
            Other(v) => v,
        }
    }

    /// VCL NAL unit (a coded slice segment): types 0..=31.
    pub fn is_vcl(self) -> bool {
        self.id() < 32
    }

    /// Intra random access point: BLA, IDR, CRA and the reserved IRAP types (16..=23).
    pub fn is_irap(self) -> bool {
        (16..=23).contains(&self.id())
    }

    pub fn is_idr(self) -> bool {
        matches!(self, NalType::IdrWRadl | NalType::IdrNLp)
    }

    pub fn is_bla(self) -> bool {
        matches!(self, NalType::BlaWLp | NalType::BlaWRadl | NalType::BlaNLp)
    }

    pub fn is_cra(self) -> bool {
        self == NalType::CraNut
    }

    pub fn is_rasl(self) -> bool {
        matches!(self, NalType::RaslN | NalType::RaslR)
    }

    pub fn is_radl(self) -> bool {
        matches!(self, NalType::RadlN | NalType::RadlR)
    }

    /// Sub-layer non-reference picture (§3.135): an even VCL type ≤ 14.
    pub fn is_sub_layer_non_ref(self) -> bool {
        let id = self.id();
        id <= 14 && id % 2 == 0
    }
}

/// The two-byte NAL unit header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    pub nal_type: NalType,
    pub layer_id: u8,
    /// `TemporalId = nuh_temporal_id_plus1 - 1`.
    pub temporal_id: u8,
}

impl NalHeader {
    /// Parses the header from the first two bytes of a NAL unit.
    pub fn parse(nal: &[u8]) -> Option<Self> {
        let b0 = *nal.first()?;
        let b1 = *nal.get(1)?;
        if b0 & 0x80 != 0 {
            return None; // forbidden_zero_bit
        }
        let tid_plus1 = b1 & 0x07;
        if tid_plus1 == 0 {
            return None;
        }
        Some(NalHeader {
            nal_type: NalType::from_id((b0 >> 1) & 0x3f),
            layer_id: ((b0 & 1) << 5) | (b1 >> 3),
            temporal_id: tid_plus1 - 1,
        })
    }
}

/// An unescaped NAL payload plus the positions where emulation-prevention
/// bytes were removed.
#[derive(Debug, Clone, Default)]
pub struct Rbsp {
    /// Payload bytes with emulation prevention removed (header excluded).
    pub data: Vec<u8>,
    /// For each removed `03`, its index in the **escaped** payload (header
    /// excluded), ascending. Empty for most NALs.
    pub epb_pos: Vec<usize>,
}

impl Rbsp {
    /// Maps an offset in the escaped payload to the RBSP offset (§7.4.7.1
    /// entry points count the escaped bytes).
    pub fn escaped_to_rbsp(&self, escaped: usize) -> usize {
        let removed = self.epb_pos.iter().take_while(|&&p| p < escaped).count();
        escaped - removed
    }
}

/// Removes emulation-prevention bytes from an escaped payload.
pub fn unescape(ebsp: &[u8]) -> Rbsp {
    let mut out = Vec::with_capacity(ebsp.len());
    let mut epb_pos = Vec::new();
    let mut zeros = 0usize;
    let mut i = 0;
    while i < ebsp.len() {
        let b = ebsp[i];
        if zeros >= 2 && b == 0x03 {
            // §7.4.2: every `00 00 03` carries an emulation_prevention_three_byte
            // (a conforming encoder never produces `00 00 03 xx` with xx > 3
            // any other way, and `cabac_zero_words` end a NAL in `00 00 03`).
            epb_pos.push(i);
            zeros = 0;
            i += 1;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
        i += 1;
    }
    Rbsp { data: out, epb_pos }
}

/// Inserts emulation-prevention bytes (used by tests to build streams).
pub fn escape_into(rbsp: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0usize;
    for &b in rbsp {
        if zeros >= 2 && b <= 0x03 {
            out.push(0x03);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// Splits an Annex-B byte stream into NAL byte slices (header + escaped
/// payload), stripping start codes and trailing zero bytes.
pub fn split_annex_b(stream: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut prev: Option<usize> = None;
    let mut i = 0;
    while let Some(w) = stream.get(i..i + 3) {
        if w[0] == 0 && w[1] == 0 && w[2] == 1 {
            if let Some(s) = prev {
                let mut end = i;
                // trailing_zero_8bits / the leading zero of a 4-byte start code
                while end > s && stream[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    nals.push(&stream[s..end]);
                }
            }
            prev = Some(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    if let Some(s) = prev {
        let mut end = stream.len();
        while end > s && stream[end - 1] == 0 {
            end -= 1;
        }
        if end > s {
            nals.push(&stream[s..end]);
        }
    }
    nals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields() {
        // type 33 (SPS), layer 0, tid 0: 0100_0010 0000_0001
        let h = NalHeader::parse(&[0x42, 0x01]).unwrap();
        assert_eq!(h.nal_type, NalType::Sps);
        assert_eq!(h.layer_id, 0);
        assert_eq!(h.temporal_id, 0);
        // type 1 (TRAIL_R), tid 2
        let h = NalHeader::parse(&[0x02, 0x03]).unwrap();
        assert_eq!(h.nal_type, NalType::TrailR);
        assert_eq!(h.temporal_id, 2);
        assert!(NalHeader::parse(&[0x82, 0x01]).is_none());
        assert!(NalHeader::parse(&[0x02, 0x00]).is_none());
    }

    #[test]
    fn type_roundtrip() {
        for v in 0..64u8 {
            assert_eq!(NalType::from_id(v).id(), v);
        }
        assert!(NalType::CraNut.is_irap());
        assert!(NalType::IdrNLp.is_irap());
        assert!(!NalType::TrailR.is_irap());
        assert!(NalType::TrailN.is_sub_layer_non_ref());
        assert!(!NalType::TrailR.is_sub_layer_non_ref());
        assert!(NalType::RaslN.is_sub_layer_non_ref());
    }

    #[test]
    fn unescape_records_positions() {
        let rbsp = [0u8, 0, 1, 5, 0, 0, 0, 9];
        let mut esc = Vec::new();
        escape_into(&rbsp, &mut esc);
        assert_eq!(esc, vec![0, 0, 3, 1, 5, 0, 0, 3, 0, 9]);
        let r = unescape(&esc);
        assert_eq!(r.data, rbsp);
        assert_eq!(r.epb_pos, vec![2, 7]);
        assert_eq!(r.escaped_to_rbsp(0), 0);
        assert_eq!(r.escaped_to_rbsp(3), 2);
        assert_eq!(r.escaped_to_rbsp(9), 7);
    }

    #[test]
    fn split_strips_start_codes_and_zero_padding() {
        let s = [0, 0, 0, 1, 0x42, 1, 0xAA, 0, 0, 1, 0x44, 1, 0xBB, 0, 0];
        let n = split_annex_b(&s);
        assert_eq!(n, vec![&[0x42u8, 1, 0xAA][..], &[0x44u8, 1, 0xBB][..]]);
    }
}
