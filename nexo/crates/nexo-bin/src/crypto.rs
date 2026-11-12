//! Recognised crypto constants: tables and magic strings found in the file's bytes, with the evidence for
//! each (address, how much of the table matched) and the functions that refer to them.
//!
//! The tables are not typed in: they are computed from their definition (AES from the field GF(2^8),
//! SHA-2 from the cube roots of the primes, MD5 from the sine, CRC from its polynomial), so a
//! typing mistake cannot produce a false match. The tests check the first and last words of each
//! against values from the standards written down independently.
//!
//! A hit is a hint that the code uses the algorithm, never proof: a table can sit in a file unused.
//! Hits are ranked by strength: `full` (the whole table, byte for byte), `prefix` (the first words only,
//! the rest altered or cut) and `immediates` (the constants appear as numbers in the code of one function,
//! which is weaker still: four numbers can meet by chance).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use crate::ops::command;
use crate::page::{cut, Output, Row};
use crate::refs::Kind;
use crate::{names, Image, Result};

/// How many referencing functions a row names.
const MAX_USERS: usize = 6;
/// Matches of one pattern kept (a pattern found a hundred times is noise).
const MAX_HITS_PER_PATTERN: usize = 8;

/// One thing to look for.
struct Pattern {
    /// Stable name used for the label and the saved name, like `sha256_K`.
    id: &'static str,
    /// What it is, for people.
    what: &'static str,
    bytes: Vec<u8>,
    /// How many leading bytes count as a prefix match.
    prefix: usize,
}

fn le32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_le_bytes()).collect()
}
fn be32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_be_bytes()).collect()
}
fn le64(v: &[u64]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_le_bytes()).collect()
}
fn be64(v: &[u64]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_be_bytes()).collect()
}

/// Multiply in GF(2^8) with the AES polynomial.
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// The AES S-box: the inverse in GF(2^8) (0 for 0), then the affine map.
pub(crate) fn aes_sbox() -> [u8; 256] {
    let mut inv = [0u8; 256];
    for a in 1..=255u8 {
        inv[a as usize] = (1..=255u8).find(|&b| gmul(a, b) == 1).unwrap_or(0);
    }
    let mut s = [0u8; 256];
    for i in 0..256 {
        let x = inv[i];
        s[i] = x ^ x.rotate_left(1) ^ x.rotate_left(2) ^ x.rotate_left(3) ^ x.rotate_left(4) ^ 0x63;
    }
    s
}

pub(crate) fn aes_inv_sbox() -> [u8; 256] {
    let s = aes_sbox();
    let mut r = [0u8; 256];
    for (i, &v) in s.iter().enumerate() {
        r[v as usize] = i as u8;
    }
    r
}

/// Te0 of the table-driven AES: (2s, s, s, 3s) as a big-endian word.
fn aes_te0() -> [u32; 256] {
    let s = aes_sbox();
    let mut t = [0u32; 256];
    for i in 0..256 {
        let v = s[i];
        t[i] = u32::from_be_bytes([gmul(v, 2), v, v, gmul(v, 3)]);
    }
    t
}

/// Td0: (14s', 9s', 13s', 11s') of the inverse S-box, big-endian.
fn aes_td0() -> [u32; 256] {
    let s = aes_inv_sbox();
    let mut t = [0u32; 256];
    for i in 0..256 {
        let v = s[i];
        t[i] = u32::from_be_bytes([gmul(v, 14), gmul(v, 9), gmul(v, 13), gmul(v, 11)]);
    }
    t
}

/// The first `n` primes.
fn primes(n: usize) -> Vec<u64> {
    let mut v: Vec<u64> = Vec::new();
    let mut c = 2u64;
    while v.len() < n {
        if v.iter().all(|p| !c.is_multiple_of(*p)) {
            v.push(c);
        }
        c += 1;
    }
    v
}

/// Little-endian limbs, enough for the cubes below (about 210 bits).
type Big = [u64; 5];

fn big_mul(a: &Big, b: &Big) -> Big {
    let mut out = [0u64; 5];
    for i in 0..5 {
        let mut carry = 0u128;
        for j in 0..5 - i {
            let cur = out[i + j] as u128 + (a[i] as u128) * (b[j] as u128) + carry;
            out[i + j] = cur as u64;
            carry = cur >> 64;
        }
    }
    out
}

fn big_le(a: &Big, b: &Big) -> bool {
    for i in (0..5).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    true
}

/// floor(frac(cbrt(p)) * 2^bits) for bits up to 64, by exact integer arithmetic: the largest x with
/// (int + x / 2^bits)^3 <= p, that is (int * 2^bits + x)^3 <= p * 2^(3*bits).
fn cbrt_frac(p: u64, bits: u32) -> u64 {
    let int = (1u64..).find(|c| (c + 1).pow(3) > p).unwrap_or(1);
    let target = shl(&[p, 0, 0, 0, 0], 3 * bits);
    let (mut lo, mut hi) = (0u64, if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 });
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let x = add_shift(int, mid, bits);
        let cube = big_mul(&big_mul(&x, &x), &x);
        if big_le(&cube, &target) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// sqrt version for the initial hash values.
fn sqrt_frac(p: u64, bits: u32) -> u64 {
    let int = (1u64..).find(|c| (c + 1).pow(2) > p).unwrap_or(1);
    let target = shl(&[p, 0, 0, 0, 0], 2 * bits);
    let (mut lo, mut hi) = (0u64, if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 });
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let x = add_shift(int, mid, bits);
        if big_le(&big_mul(&x, &x), &target) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

fn shl(a: &Big, n: u32) -> Big {
    let mut out = [0u64; 5];
    let (limbs, bits) = ((n / 64) as usize, n % 64);
    for i in 0..5 {
        if i + limbs < 5 {
            out[i + limbs] |= a[i] << bits;
            if bits > 0 && i + limbs + 1 < 5 {
                out[i + limbs + 1] |= a[i] >> (64 - bits);
            }
        }
    }
    out
}

/// `int * 2^bits + frac` as a Big.
fn add_shift(int: u64, frac: u64, bits: u32) -> Big {
    let mut v = shl(&[int, 0, 0, 0, 0], bits);
    let mut carry = frac;
    for limb in v.iter_mut() {
        let (s, c) = limb.overflowing_add(carry);
        *limb = s;
        carry = u64::from(c);
        if carry == 0 {
            break;
        }
    }
    v
}

fn sha256_k() -> Vec<u32> {
    primes(64).into_iter().map(|p| cbrt_frac(p, 32) as u32).collect()
}
fn sha512_k() -> Vec<u64> {
    primes(80).into_iter().map(|p| cbrt_frac(p, 64)).collect()
}
fn sha256_iv() -> Vec<u32> {
    primes(8).into_iter().map(|p| sqrt_frac(p, 32) as u32).collect()
}
/// MD5: floor(2^32 * |sin(i + 1)|).
fn md5_t() -> Vec<u32> {
    (1..=64).map(|i| ((i as f64).sin().abs() * 4294967296.0).floor() as u32).collect()
}

/// CRC table for a reflected polynomial.
fn crc_table_reflected(poly: u32) -> Vec<u32> {
    (0..256u32)
        .map(|i| {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            }
            c
        })
        .collect()
}
/// CRC table for a normal (MSB-first) polynomial.
fn crc_table_normal(poly: u32) -> Vec<u32> {
    (0..256u32)
        .map(|i| {
            let mut c = i << 24;
            for _ in 0..8 {
                c = if c & 0x8000_0000 != 0 { (c << 1) ^ poly } else { c << 1 };
            }
            c
        })
        .collect()
}

fn patterns() -> Vec<Pattern> {
    let mut v: Vec<Pattern> = Vec::new();
    let mut add = |id, what, bytes: Vec<u8>, prefix| v.push(Pattern { id, what, bytes, prefix });
    add("aes_sbox", "AES S-box (256 bytes)", aes_sbox().to_vec(), 16);
    add("aes_inv_sbox", "AES inverse S-box (256 bytes)", aes_inv_sbox().to_vec(), 16);
    let te = aes_te0();
    add("aes_Te0", "AES encryption table Te0 (256 words, big-endian)", be32(&te), 32);
    add("aes_Te0", "AES encryption table Te0 (256 words, little-endian)", le32(&te), 32);
    let td = aes_td0();
    add("aes_Td0", "AES decryption table Td0 (256 words, big-endian)", be32(&td), 32);
    add("aes_Td0", "AES decryption table Td0 (256 words, little-endian)", le32(&td), 32);
    let k = sha256_k();
    add("sha256_K", "SHA-256 round constants K (64 words, little-endian)", le32(&k), 32);
    add("sha256_K", "SHA-256 round constants K (64 words, big-endian)", be32(&k), 32);
    let k5 = sha512_k();
    add("sha512_K", "SHA-512 round constants K (80 words, little-endian)", le64(&k5), 32);
    add("sha512_K", "SHA-512 round constants K (80 words, big-endian)", be64(&k5), 32);
    let t = md5_t();
    add("md5_T", "MD5 sine table T (64 words, little-endian)", le32(&t), 32);
    add("md5_T", "MD5 sine table T (64 words, big-endian)", be32(&t), 32);
    for (id, what, poly) in [
        ("crc32_table", "CRC-32 table (poly 0xEDB88320)", 0xEDB8_8320u32),
        ("crc32c_table", "CRC-32C table (poly 0x82F63B78)", 0x82F6_3B78),
    ] {
        let t = crc_table_reflected(poly);
        add(id, what, le32(&t), 32);
    }
    let t = crc_table_normal(0x04C1_1DB7);
    add("crc32_msb_table", "CRC-32 table, MSB first (poly 0x04C11DB7)", be32(&t), 32);
    add("crc32_msb_table", "CRC-32 table, MSB first (poly 0x04C11DB7)", le32(&t), 32);
    add("chacha_sigma", "ChaCha/Salsa constant \"expand 32-byte k\"", b"expand 32-byte k".to_vec(), 16);
    add("chacha_tau", "ChaCha/Salsa constant \"expand 16-byte k\"", b"expand 16-byte k".to_vec(), 16);
    add(
        "base64_alphabet",
        "base64 alphabet",
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".to_vec(),
        64,
    );
    add(
        "base64url_alphabet",
        "base64 URL-safe alphabet",
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".to_vec(),
        64,
    );
    // the SHA-256 initial values as a table
    let iv = sha256_iv();
    add("sha256_iv", "SHA-256 initial hash values (8 words, little-endian)", le32(&iv), 32);
    add("sha256_iv", "SHA-256 initial hash values (8 words, big-endian)", be32(&iv), 32);
    v
}

/// Constants that the code carries as numbers: a set is a hit when `need` of them appear in one function.
struct ImmSet {
    id: &'static str,
    what: &'static str,
    values: &'static [u32],
    need: usize,
}

const IMM_SETS: [ImmSet; 3] = [
    ImmSet {
        id: "sha1_K",
        what: "SHA-1 round constants",
        values: &[0x5a82_7999, 0x6ed9_eba1, 0x8f1b_bcdc, 0xca62_c1d6],
        need: 3,
    },
    ImmSet {
        id: "md5_sha1_iv",
        what: "MD5/SHA-1 initial values",
        values: &[0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
        need: 4,
    },
    ImmSet {
        id: "sha256_iv_imm",
        what: "SHA-256 initial values",
        values: &[
            0x6a09_e667,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
            0x510e_527f,
            0x9b05_688c,
            0x1f83_d9ab,
            0x5be0_cd19,
        ],
        need: 5,
    },
];

/// One finding.
#[derive(Debug, Clone)]
pub struct CryptoHit {
    pub id: &'static str,
    pub what: String,
    pub addr: u64,
    /// `full`, `prefix` or `immediates`.
    pub strength: &'static str,
    /// Bytes matched (0 for immediates).
    pub len: usize,
    pub section: String,
    pub evidence: String,
    /// Functions that refer to it, by first reference: (start, reference address).
    pub users: Vec<(u64, u64)>,
}

fn find_all(hay: &[u8], needle: &[u8], limit: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= hay.len() && out.len() < limit {
        match hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first) {
            Some(p) => {
                i += p;
                if &hay[i..i + needle.len()] == needle {
                    out.push(i);
                }
                i += 1;
            }
            None => break,
        }
    }
    out
}

/// Every recognised constant in the file.
pub fn scan(img: &Image) -> Vec<CryptoHit> {
    let mut hits: Vec<CryptoHit> = Vec::new();
    let pats = patterns();
    let refs = img.refs();
    let users_of = |addr: u64, len: usize| -> Vec<(u64, u64)> {
        let mut by_fn: BTreeMap<u64, u64> = BTreeMap::new();
        for r in refs.within(addr, len.max(1) as u64) {
            if matches!(r.kind, Kind::Read | Kind::Addr | Kind::Base | Kind::Ptr | Kind::Write) {
                if let Some(f) = img.func_at(r.src) {
                    let e = by_fn.entry(f.start).or_insert(r.src);
                    *e = (*e).min(r.src);
                }
            }
        }
        let mut v: Vec<(u64, u64)> = by_fn.into_iter().collect();
        v.sort_by_key(|&(_, s)| s);
        v
    };
    let secs: Vec<_> = img.sections.iter().filter(|s| s.alloc && s.file_size > 0).collect();
    for p in &pats {
        let mut found: BTreeSet<u64> = BTreeSet::new();
        let mut local: Vec<CryptoHit> = Vec::new();
        for s in &secs {
            let bytes = img.section_bytes(s);
            for off in find_all(bytes, &p.bytes, MAX_HITS_PER_PATTERN) {
                let addr = s.addr + off as u64;
                if found.insert(addr) {
                    local.push(CryptoHit {
                        id: p.id,
                        what: p.what.to_string(),
                        addr,
                        strength: "full",
                        len: p.bytes.len(),
                        section: s.name.clone(),
                        evidence: format!("all {} bytes equal the table at {addr:#x} in {}", p.bytes.len(), s.name),
                        users: users_of(addr, p.bytes.len()),
                    });
                }
            }
        }
        if p.prefix < p.bytes.len() {
            for s in &secs {
                let bytes = img.section_bytes(s);
                for off in find_all(bytes, &p.bytes[..p.prefix], MAX_HITS_PER_PATTERN) {
                    let addr = s.addr + off as u64;
                    if found.insert(addr) {
                        // how far the table goes on matching
                        let tail = &bytes[off..];
                        let same = tail.iter().zip(&p.bytes).take_while(|(a, b)| a == b).count();
                        local.push(CryptoHit {
                            id: p.id,
                            what: p.what.to_string(),
                            addr,
                            strength: "prefix",
                            len: same,
                            section: s.name.clone(),
                            evidence: format!(
                                "the first {same} of {} bytes equal the table at {addr:#x} in {}; the rest differs or is cut off",
                                p.bytes.len(),
                                s.name
                            ),
                            users: users_of(addr, same),
                        });
                    }
                }
            }
        }
        hits.extend(local);
    }
    // a magic string inside a longer string of the same file is still a hit; two ids at one address are one finding
    // (the same bytes matched as little- and big-endian cannot both be a full table, so this only merges prefixes)
    hits.sort_by_key(|h| (h.addr, std::cmp::Reverse(h.len)));
    hits.dedup_by(|b, a| a.addr == b.addr && a.id == b.id);
    // immediates in code
    for set in &IMM_SETS {
        let mut per_fn: BTreeMap<u64, BTreeMap<u32, u64>> = BTreeMap::new();
        for s in img.sections.iter().filter(|s| s.exec && s.file_size > 0) {
            let bytes = img.section_bytes(s);
            for &v in set.values {
                for off in find_all(bytes, &v.to_le_bytes(), 4096) {
                    let addr = s.addr + off as u64;
                    if let Some(f) = img.func_at(addr) {
                        per_fn.entry(f.start).or_default().entry(v).or_insert(addr);
                    }
                }
            }
        }
        for (f, seen) in per_fn {
            if seen.len() >= set.need {
                let list: Vec<String> = seen.iter().map(|(v, a)| format!("{v:#010x} at {a:#x}")).collect();
                hits.push(CryptoHit {
                    id: set.id,
                    what: set.what.to_string(),
                    addr: *seen.values().min().unwrap_or(&f),
                    strength: "immediates",
                    len: 0,
                    section: img.section_at(f).map(|s| s.name.clone()).unwrap_or_default(),
                    evidence: format!(
                        "{} of {} constants appear as numbers in {}: {}",
                        seen.len(),
                        set.values.len(),
                        img.func_at(f).map_or("?", |x| img.func_name(x)),
                        list.join(", ")
                    ),
                    users: vec![(f, *seen.values().min().unwrap_or(&f))],
                });
            }
        }
    }
    hits.sort_by_key(|h| {
        (
            match h.strength {
                "full" => 0,
                "prefix" => 1,
                _ => 2,
            },
            h.addr,
        )
    });
    hits
}

/// What `--save` did for one hit.
fn save(img: &Image, data_dir: &Path, hits: &[CryptoHit]) -> Vec<String> {
    let mut log: Vec<String> = Vec::new();
    let mut taken: BTreeSet<String> = img.user.names.values().cloned().collect();
    for h in hits {
        let label = h.what.split(" (").next().unwrap_or(&h.what).to_string();
        if h.strength != "immediates" {
            if let Some(old) = img.user_name(h.addr) {
                log.push(format!("kept the name {old} at {:#x}", h.addr));
            } else {
                let mut n = h.id.to_string();
                let mut k = 2;
                while taken.contains(&n) || img.symbols.iter().any(|s| s.name == n) {
                    n = format!("{}_{k}", h.id);
                    k += 1;
                }
                match names::set_name(img, data_dir, h.addr, &n) {
                    Ok(_) => {
                        taken.insert(n.clone());
                        log.push(format!("named {:#x} {n}", h.addr));
                    }
                    Err(e) => log.push(format!("not named {:#x}: {e}", h.addr)),
                }
            }
        }
        let note = match h.strength {
            "full" => format!("uses {label} (table at {:#x}, full match)", h.addr),
            "prefix" => format!("may use {label} (table at {:#x}, prefix match only)", h.addr),
            _ => format!("carries {} as immediates", h.what),
        };
        for &(f, _) in &h.users {
            if img.note_at(f).is_some() {
                log.push(format!("kept the note at {f:#x}"));
                continue;
            }
            match names::set_note(img, data_dir, f, &note) {
                Ok(_) => log.push(format!("noted {f:#x}: {note}")),
                Err(e) => log.push(format!("not noted {f:#x}: {e}")),
            }
        }
    }
    log
}

/// The report. With `data_dir`, the tables are also named and their users noted (never replacing a name or note).
pub fn crypto_window(img: &Image, data_dir: Option<&Path>, from: Option<u64>, budget: usize) -> Result<Output> {
    let hits = scan(img);
    let mut items: Vec<Row> = Vec::new();
    let head = if hits.is_empty() {
        "crypto: no known constant table found in the loaded sections (AES, SHA-1/2, MD5, CRC-32, ChaCha, base64). \
         A program can still compute or unpack them at run time."
            .to_string()
    } else {
        let n = |s: &str| hits.iter().filter(|h| h.strength == s).count();
        format!(
            "crypto: {} finding{} ({} full, {} prefix, {} immediates). A finding is a hint that the code uses it, not proof.",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" },
            n("full"),
            n("prefix"),
            n("immediates")
        )
    };
    items.push(Row { key: 0, text: head, json: None });
    for h in &hits {
        let users: Vec<String> = h
            .users
            .iter()
            .take(MAX_USERS)
            .map(|&(f, at)| format!("{} {f:#x} (at {at:#x})", img.func_at(f).map_or("?", |x| img.func_name(x))))
            .collect();
        let more = h.users.len().saturating_sub(MAX_USERS);
        let used = if users.is_empty() {
            "no code reference found (the table may be reached through a base pointer)".to_string()
        } else {
            format!(
                "used by {}{}",
                users.join(", "),
                if more > 0 { format!(" and {more} more") } else { String::new() }
            )
        };
        let place = if h.strength == "immediates" { "in code" } else { "table" };
        let text = format!(
            "{:#010x}  {:<10}  {}  {place}\n    evidence: {}\n    {used}",
            h.addr, h.strength, h.what, h.evidence
        );
        items.push(Row {
            key: 0,
            text,
            json: Some(json!({"id": h.id, "what": h.what, "addr": h.addr, "strength": h.strength, "len": h.len,
                "section": h.section, "evidence": h.evidence,
                "users": h.users.iter().map(|&(f, at)| json!({"func": f, "at": at})).collect::<Vec<_>>()})),
        });
    }
    let mut saved = Vec::new();
    if let Some(dir) = data_dir {
        saved = save(img, dir, &hits);
        items.push(Row {
            key: 0,
            text: format!("--save: {} change{}", saved.len(), if saved.len() == 1 { "" } else { "s" }),
            json: None,
        });
        for l in &saved {
            items.push(Row { key: 0, text: format!("  {l}"), json: None });
        }
    } else if !hits.is_empty() {
        items.push(Row {
            key: 0,
            text: "add --save to name the tables and note the functions that use them".into(),
            json: None,
        });
    }
    let (rows, tail) = cut(&items, from, budget, |r| r.clone());
    let meta = json!({"findings": hits.len(), "saved": saved});
    Ok(Output {
        op: "crypto",
        meta,
        rows,
        tail,
        command: command(img, "crypto", if data_dir.is_some() { "--save" } else { "" }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computed_tables_start_and_end_with_the_values_of_the_standards() {
        let s = aes_sbox();
        assert_eq!((s[0], s[1], s[0x53], s[255]), (0x63, 0x7c, 0xed, 0x16));
        let i = aes_inv_sbox();
        assert_eq!((i[0], i[0x63], i[255]), (0x52, 0x00, 0x7d));
        let te = aes_te0();
        assert_eq!((te[0], te[1], te[255]), (0xc663_63a5, 0xf87c_7c84, 0x2c16_163a));
        let td = aes_td0();
        assert_eq!((td[0], td[1]), (0x51f4_a750, 0x7e41_6553));
        let k = sha256_k();
        assert_eq!((k[0], k[1], k[63]), (0x428a_2f98, 0x7137_4491, 0xc671_78f2));
        let k5 = sha512_k();
        assert_eq!((k5[0], k5[79]), (0x428a_2f98_d728_ae22, 0x6c44_198c_4a47_5817));
        let iv = sha256_iv();
        assert_eq!((iv[0], iv[7]), (0x6a09_e667, 0x5be0_cd19));
        let t = md5_t();
        assert_eq!((t[0], t[1], t[63]), (0xd76a_a478, 0xe8c7_b756, 0xeb86_d391));
        let c = crc_table_reflected(0xEDB8_8320);
        assert_eq!((c[1], c[255]), (0x7707_3096, 0x2d02_ef8d));
        assert_eq!(crc_table_reflected(0x82F6_3B78)[1], 0xf26b_8303);
        assert_eq!(crc_table_normal(0x04C1_1DB7)[1], 0x04c1_1db7);
    }
}
