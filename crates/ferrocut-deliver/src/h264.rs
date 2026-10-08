//! H.264 bitstream helpers: Annex-B NAL splitting, AVCC conversion, avcC.

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1f)
}

/// NAL units (without start codes or trailing zero bytes) of an Annex-B
/// buffer.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |&n| n - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

/// NAL units of a length-prefixed (AVCC, 4-byte lengths) buffer.
pub fn split_avcc(mut data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let len = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
        out.push(data.get(4..4 + len)?);
        data = &data[4 + len..];
    }
    Some(out)
}

/// One access unit split for MP4: parameter sets out, everything else as a
/// 4-byte length-prefixed sample.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitAu {
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub sample: Vec<u8>,
    pub idr: bool,
}

pub fn split_au(annexb: &[u8]) -> SplitAu {
    let mut out = SplitAu::default();
    for nal in split_annexb(annexb) {
        match nal_type(nal) {
            NAL_SPS => out.sps.push(nal.to_vec()),
            NAL_PPS => out.pps.push(nal.to_vec()),
            NAL_AUD => {}
            t => {
                out.idr |= t == NAL_IDR;
                out.sample
                    .extend_from_slice(&(nal.len() as u32).to_be_bytes());
                out.sample.extend_from_slice(nal);
            }
        }
    }
    out
}

/// AVCDecoderConfigurationRecord (ISO/IEC 14496-15) for one SPS + PPS,
/// 4-byte NAL lengths. Valid for profiles without the 4:2:0-implied
/// High-profile extension fields (Baseline/Main/Extended).
pub fn avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    assert!(sps.len() >= 4, "SPS too short");
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xFC | 3, 0xE0 | 1];
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(1);
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    v
}

/// (SPS, PPS) back out of an avcC record.
pub fn parse_avcc(rec: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if rec.first() != Some(&1) || rec.get(5)? & 0x1f != 1 {
        return None;
    }
    let sl = u16::from_be_bytes([*rec.get(6)?, *rec.get(7)?]) as usize;
    let sps = rec.get(8..8 + sl)?.to_vec();
    let p = 8 + sl;
    if *rec.get(p)? != 1 {
        return None;
    }
    let pl = u16::from_be_bytes([*rec.get(p + 1)?, *rec.get(p + 2)?]) as usize;
    let pps = rec.get(p + 3..p + 3 + pl)?.to_vec();
    Some((sps, pps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_split_and_avcc_round_trip() {
        let sps = [0x67, 77, 0, 31, 0xAB];
        let pps = [0x68, 0xCE, 0x3C, 0x80];
        let idr = [0x65, 0x88, 0x00, 0x00, 0x03, 0x01];
        let mut au = vec![0, 0, 0, 1];
        au.extend_from_slice(&sps);
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(&pps);
        au.extend_from_slice(&[0, 0, 1]);
        au.extend_from_slice(&idr);
        au.push(0); // trailing_zero_8bits
        let nals = split_annexb(&au);
        assert_eq!(nals, vec![&sps[..], &pps[..], &idr[..]]);
        let s = split_au(&au);
        assert!(s.idr);
        assert_eq!(s.sps, vec![sps.to_vec()]);
        assert_eq!(split_avcc(&s.sample).unwrap(), vec![&idr[..]]);
        assert_eq!(
            parse_avcc(&avcc(&sps, &pps)).unwrap(),
            (sps.to_vec(), pps.to_vec())
        );
    }
}
