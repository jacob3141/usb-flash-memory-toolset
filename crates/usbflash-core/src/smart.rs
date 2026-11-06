// SPDX-License-Identifier: GPL-3.0-or-later
//! Parser for ATA SMART attribute data.
//!
//! Most USB sticks do not implement SMART at all. Sticks based on SSD controllers
//! and USB-SATA bridges often do, via SCSI/ATA translation (SAT). The transport
//! lives in [`crate::platform`]; this module only interprets the data.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmartAttribute {
    pub id: u8,
    pub name: &'static str,
    pub current: u8,
    pub worst: u8,
    pub raw: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SmartOutcome {
    /// The device did not answer the request, as is normal for most sticks.
    Unavailable {
        reason: String,
    },
    Available {
        attributes: Vec<SmartAttribute>,
    },
}

fn attribute_name(id: u8) -> &'static str {
    match id {
        1 => "Raw read error rate",
        5 => "Reallocated sectors",
        9 => "Power-on hours",
        12 => "Power cycles",
        170 => "Available reserved space",
        171 => "Program fail count",
        172 => "Erase fail count",
        173 => "Average erase count",
        174 => "Unexpected power loss",
        177 => "Wear leveling count",
        180 => "Unused reserved blocks",
        181 => "Program fail count",
        182 => "Erase fail count",
        187 => "Reported uncorrectable errors",
        192 => "Power-off retracts",
        194 => "Temperature",
        196 => "Reallocation events",
        197 => "Pending sectors",
        198 => "Offline uncorrectable sectors",
        199 => "CRC errors",
        202 => "Percent lifetime used",
        231 => "SSD life left",
        233 => "Media wearout indicator",
        241 => "Total host writes",
        242 => "Total host reads",
        _ => "Vendor specific",
    }
}

/// Parses a 512 byte SMART READ DATA response. Returns `None` for invalid data.
pub fn parse(data: &[u8]) -> Option<Vec<SmartAttribute>> {
    if data.len() < 512 {
        return None;
    }
    let sum = data[..512].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    if sum != 0 || data[..512].iter().all(|&b| b == 0) {
        return None;
    }
    let attrs: Vec<_> = data[2..362]
        .as_chunks::<12>()
        .0
        .iter()
        .filter(|e| e[0] != 0)
        .map(|e| {
            let mut raw = [0u8; 8];
            raw[..6].copy_from_slice(&e[5..11]);
            SmartAttribute {
                id: e[0],
                name: attribute_name(e[0]),
                current: e[3],
                worst: e[4],
                raw: u64::from_le_bytes(raw),
            }
        })
        .collect();
    if attrs.is_empty() { None } else { Some(attrs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut d = vec![0u8; 512];
        d[0] = 0x10;
        // Attribute 5 with raw value 3.
        d[2] = 5;
        d[5] = 100;
        d[6] = 99;
        d[7] = 3;
        // Attribute 194 with raw value 40.
        d[14] = 194;
        d[17] = 60;
        d[18] = 55;
        d[19] = 40;
        let sum = d[..511].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        d[511] = 0u8.wrapping_sub(sum);
        d
    }

    #[test]
    fn parses_attributes() {
        let a = parse(&sample()).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].id, 5);
        assert_eq!(a[0].name, "Reallocated sectors");
        assert_eq!((a[0].current, a[0].worst, a[0].raw), (100, 99, 3));
        assert_eq!(a[1].raw, 40);
    }

    #[test]
    fn rejects_bad_checksum_and_empty_data() {
        let mut d = sample();
        d[100] ^= 0xFF;
        assert!(parse(&d).is_none());
        assert!(parse(&[0u8; 512]).is_none());
        assert!(parse(&[0u8; 10]).is_none());
    }
}
