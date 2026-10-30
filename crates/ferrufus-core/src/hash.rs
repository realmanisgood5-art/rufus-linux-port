// SPDX-License-Identifier: GPL-3.0-or-later
//! Checksums of images and files.
//!
//! Rufus computes MD5, SHA-1 and SHA-256 of the selected image, plus SHA-512
//! when "extra hashes" are enabled (Alt-H, `SETTING_ENABLE_EXTRA_HASHES`);
//! see `src/hash.c:HashThread()`. Ferrufus computes the same digests with the
//! RustCrypto implementations, reading the file once for all algorithms, and
//! adds comparison against an expected checksum.

use crate::cancel::CancelToken;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::str::FromStr;

/// Size of the read buffer.
const BUFFER_SIZE: usize = 1024 * 1024;

/// A checksum algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    /// MD5.
    Md5,
    /// SHA-1.
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-512.
    Sha512,
}

impl HashAlgorithm {
    /// All supported algorithms.
    pub const ALL: [Self; 4] = [Self::Md5, Self::Sha1, Self::Sha256, Self::Sha512];

    /// The algorithms Rufus computes by default. SHA-512 is only computed
    /// when extra hashes are enabled.
    pub const UPSTREAM_DEFAULT: [Self; 3] = [Self::Md5, Self::Sha1, Self::Sha256];

    /// The display name, as in Rufus' checksum dialog.
    pub fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha512 => "SHA512",
        }
    }

    /// The digest length in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }

    /// Parses an algorithm name (`md5`, `sha1`, `sha-1`, `sha256`, …), ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().replace('-', "").as_str() {
            "md5" => Some(Self::Md5),
            "sha1" => Some(Self::Sha1),
            "sha256" => Some(Self::Sha256),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    /// The algorithm whose digest has `len` bytes.
    fn from_digest_len(len: usize) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|algorithm| algorithm.digest_len() == len)
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A computed digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Digest {
    /// The algorithm.
    pub algorithm: HashAlgorithm,
    /// The digest, as lowercase hexadecimal.
    pub hex: String,
}

/// Digests of a whole file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HashReport {
    /// Number of bytes hashed.
    pub size: u64,
    /// One digest per requested algorithm, in the order requested.
    pub digests: Vec<Digest>,
}

impl HashReport {
    /// The digest computed with `algorithm`, if it was requested.
    pub fn get(&self, algorithm: HashAlgorithm) -> Option<&Digest> {
        self.digests
            .iter()
            .find(|digest| digest.algorithm == algorithm)
    }

    /// Compares the report with an expected checksum. Returns `None` when
    /// the expected algorithm was not computed.
    pub fn matches(&self, expected: &ExpectedHash) -> Option<bool> {
        self.get(expected.algorithm)
            .map(|digest| digest.hex == expected.hex)
    }
}

/// Progress of a long-running read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes processed so far.
    pub done: u64,
    /// Total bytes, when known.
    pub total: Option<u64>,
}

/// An expected checksum, written `algorithm:hex` or as bare hexadecimal (the
/// algorithm is then inferred from the length).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpectedHash {
    /// The algorithm.
    pub algorithm: HashAlgorithm,
    /// The expected digest, as lowercase hexadecimal.
    pub hex: String,
}

impl FromStr for ExpectedHash {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim();
        let (algorithm, hex) = match text.split_once(':') {
            Some((name, hex)) => {
                let algorithm = HashAlgorithm::from_name(name).ok_or_else(|| {
                    Error::InvalidInput(format!("unknown checksum algorithm `{name}`"))
                })?;
                (Some(algorithm), hex)
            }
            None => (None, text),
        };
        let hex = hex.to_ascii_lowercase();
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) || hex.len() % 2 != 0 {
            return Err(Error::InvalidInput(format!(
                "`{text}` is not a hexadecimal checksum"
            )));
        }
        let len = hex.len() / 2;
        let algorithm = match algorithm {
            Some(algorithm) if algorithm.digest_len() == len => algorithm,
            Some(algorithm) => {
                return Err(Error::InvalidInput(format!(
                    "a {algorithm} checksum has {} hexadecimal digits, not {}",
                    algorithm.digest_len() * 2,
                    hex.len()
                )));
            }
            None => HashAlgorithm::from_digest_len(len).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "no supported checksum has {} hexadecimal digits",
                    hex.len()
                ))
            })?,
        };
        Ok(Self { algorithm, hex })
    }
}

/// Running state of one algorithm.
enum Hasher {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn new(algorithm: HashAlgorithm) -> Self {
        match algorithm {
            HashAlgorithm::Md5 => Self::Md5(md5::Md5::new()),
            HashAlgorithm::Sha1 => Self::Sha1(sha1::Sha1::new()),
            HashAlgorithm::Sha256 => Self::Sha256(sha2::Sha256::new()),
            HashAlgorithm::Sha512 => Self::Sha512(sha2::Sha512::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(h) => h.update(data),
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
            Self::Sha512(h) => h.update(data),
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Md5(h) => to_hex(&h.finalize()),
            Self::Sha1(h) => to_hex(&h.finalize()),
            Self::Sha256(h) => to_hex(&h.finalize()),
            Self::Sha512(h) => to_hex(&h.finalize()),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

/// Hashes everything `reader` produces with each of `algorithms`, in one pass.
///
/// `total` is only used for progress reporting. Interrupted reads are
/// retried; any other read error, or cancellation, aborts with an error and
/// no digest.
pub fn hash_reader(
    mut reader: impl Read,
    algorithms: &[HashAlgorithm],
    total: Option<u64>,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<HashReport> {
    let mut hashers: Vec<(HashAlgorithm, Hasher)> = Vec::new();
    for &algorithm in algorithms {
        if !hashers.iter().any(|(existing, _)| *existing == algorithm) {
            hashers.push((algorithm, Hasher::new(algorithm)));
        }
    }
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut done: u64 = 0;
    progress(Progress { done, total });
    loop {
        cancel.check()?;
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::io(format!("read error after {done} bytes"), e)),
        };
        let chunk = buffer.get(..count).ok_or_else(|| {
            Error::InvalidInput(format!(
                "reader returned {count} bytes for a {BUFFER_SIZE}-byte buffer"
            ))
        })?;
        for (_, hasher) in &mut hashers {
            hasher.update(chunk);
        }
        done = done
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| Error::InvalidInput("input larger than 2^64 bytes".to_string()))?;
        progress(Progress { done, total });
    }
    let digests = hashers
        .into_iter()
        .map(|(algorithm, hasher)| Digest {
            algorithm,
            hex: hasher.finish(),
        })
        .collect();
    Ok(HashReport {
        size: done,
        digests,
    })
}

/// Hashes a file. See [`hash_reader`].
pub fn hash_file(
    path: &Path,
    algorithms: &[HashAlgorithm],
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<HashReport> {
    let file =
        File::open(path).map_err(|e| Error::io(format!("cannot open {}", path.display()), e))?;
    let size = file
        .metadata()
        .map_err(|e| Error::io(format!("cannot read the size of {}", path.display()), e))?
        .len();
    hash_reader(file, algorithms, Some(size), progress, cancel).map_err(|e| match e {
        Error::Io { context, source } => Error::Io {
            context: format!("{}: {context}", path.display()),
            source,
        },
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn hash_bytes(data: &[u8], algorithms: &[HashAlgorithm]) -> HashReport {
        hash_reader(
            Cursor::new(data),
            algorithms,
            None,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap()
    }

    /// The test message of `src/hash.c:TestHashes()`.
    const UPSTREAM_TEST_MSG: &str = concat!(
        "Did you ever hear the tragedy of Darth Plagueis The Wise? ",
        "I thought not. It's not a story the Jedi would tell you. It's a Sith legend. ",
        "Darth Plagueis was a Dark Lord of the Sith, so powerful and so wise he could ",
        "use the Force to influence the midichlorians to create life... He had such a ",
        "knowledge of the dark side that he could even keep the ones he cared about ",
        "from dying. The dark side of the Force is a pathway to many abilities some ",
        "consider to be unnatural. He became so powerful... the only thing he was afraid ",
        "of was losing his power, which eventually, of course, he did. Unfortunately, ",
        "he taught his apprentice everything he knew, then his apprentice killed him ",
        "in his sleep. Ironic. He could save others from death, but not himself.",
    );

    /// `test_hash[][]` of `src/hash.c`, for message prefixes of 0 bytes,
    /// 3 bytes, the block-padding boundary (56 or 112 bytes) and the whole
    /// message.
    const UPSTREAM_VECTORS: [(HashAlgorithm, usize, [&str; 4]); 4] = [
        (
            HashAlgorithm::Md5,
            56,
            [
                "d41d8cd98f00b204e9800998ecf8427e",
                "74cac558072300385f7ab4dff7465e3c",
                "f99d37d3bee20f9c0ca3204991be2698",
                "e0ea372ac14a3574167543b851d4babb",
            ],
        ),
        (
            HashAlgorithm::Sha1,
            56,
            [
                "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                "a5bac908bf3e51ff0036a94d43b4f3bd2d01a75d",
                "8aa6c0064b013b8a6f4e88a0421d39bbf07e2e1b",
                "09463ec0b5917706c9cb1d6b164b2582c04018e0",
            ],
        ),
        (
            HashAlgorithm::Sha256,
            56,
            [
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "62c1a97974dfe6848942794be4f2f027b5f4815e1eb76db63a30f0e290b5c1c4",
                "dbca61af192edba49ea215c49a23feee302c98cc4d2c018347fe78db572f07a5",
                "c9b43c1058bc7f7661619e9d983fc9d31356e97f9195a2405ab972d0737b11bf",
            ],
        ),
        (
            HashAlgorithm::Sha512,
            112,
            [
                "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
                "4913ace12f1169e5a5f524ef87ab8fc39dff0418851fbbbb1f609d3261b2b4072bd1746e6accb91bf38f3b1b3d59b0a60af5de67aab87b76c2456fde523efc1c",
                "33df8a16dd624cbc4613b5ae902b722411c7e90f37dd3947c9a86e01c51ada68fcf5a0cd4ca928d7cc1ed469bb34c2ed008af069d8b28cc4512e6c8b2e7a5592",
                "999b4eae14de584cce5fa5962b768beda076b06df00d384bb502c6389df8159c006a5b94d1324f47e8d7bd2efe9d8d3dc1fa1429798e49826987ab5ae7ed5c21",
            ],
        ),
    ];

    #[test]
    fn upstream_test_vectors() {
        let msg = UPSTREAM_TEST_MSG.as_bytes();
        for (algorithm, boundary, expected) in UPSTREAM_VECTORS {
            for (len, hex) in [0, 3, boundary, msg.len()].into_iter().zip(expected) {
                let report = hash_bytes(&msg[..len], &[algorithm]);
                assert_eq!(report.digests[0].hex, hex, "{algorithm} of {len} bytes");
                assert_eq!(report.size, len as u64);
            }
        }
    }

    #[test]
    fn one_million_a() {
        let data = vec![b'a'; 1_000_000];
        let report = hash_bytes(&data, &HashAlgorithm::ALL);
        let hex: Vec<&str> = report.digests.iter().map(|d| d.hex.as_str()).collect();
        assert_eq!(
            hex,
            [
                "7707d6ae4e027c70eea2a935c2296f21",
                "34aa973cd4c4daa4f61eeb2bdbad27316534016f",
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
                "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b",
            ]
        );
    }

    #[test]
    fn one_pass_equals_separate_passes() {
        let data: Vec<u8> = (0..3 * BUFFER_SIZE + 17)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let together = hash_bytes(&data, &HashAlgorithm::ALL);
        for algorithm in HashAlgorithm::ALL {
            let alone = hash_bytes(&data, &[algorithm]);
            assert_eq!(together.get(algorithm), alone.get(algorithm));
        }
    }

    struct FailingReader {
        remaining: usize,
    }

    impl Read for FailingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("injected failure"));
            }
            let count = buf.len().min(self.remaining);
            buf[..count].fill(0x55);
            self.remaining -= count;
            Ok(count)
        }
    }

    #[test]
    fn read_errors_are_reported_and_produce_no_digest() {
        let reader = FailingReader {
            remaining: BUFFER_SIZE + 10,
        };
        let result = hash_reader(
            reader,
            &HashAlgorithm::ALL,
            None,
            &mut |_| {},
            &CancelToken::new(),
        );
        match result {
            Err(Error::Io { context, source }) => {
                assert!(
                    context.contains(&(BUFFER_SIZE + 10).to_string()),
                    "{context}"
                );
                assert_eq!(source.to_string(), "injected failure");
            }
            other => panic!("expected an I/O error, got {other:?}"),
        }
    }

    struct InterruptedOnce {
        interrupted: bool,
        data: Cursor<Vec<u8>>,
    }

    impl Read for InterruptedOnce {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(ErrorKind::Interrupted.into());
            }
            self.data.read(buf)
        }
    }

    #[test]
    fn interrupted_reads_are_retried() {
        let reader = InterruptedOnce {
            interrupted: false,
            data: Cursor::new(b"abc".to_vec()),
        };
        let report = hash_reader(
            reader,
            &[HashAlgorithm::Md5],
            None,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(report.digests[0].hex, "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn cancellation_stops_hashing() {
        let cancel = CancelToken::new();
        let data = vec![0u8; 4 * BUFFER_SIZE];
        let mut calls = 0;
        let mut progress = |p: Progress| {
            calls += 1;
            if p.done >= BUFFER_SIZE as u64 {
                cancel.cancel();
            }
        };
        let result = hash_reader(
            Cursor::new(data),
            &HashAlgorithm::ALL,
            None,
            &mut progress,
            &cancel,
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(
            calls <= 3,
            "hashing continued after cancellation ({calls} progress calls)"
        );
    }

    #[test]
    fn progress_reaches_the_total() {
        let data = vec![1u8; BUFFER_SIZE + 5];
        let mut last = None;
        hash_reader(
            Cursor::new(&data),
            &[HashAlgorithm::Sha256],
            Some(data.len() as u64),
            &mut |p| last = Some(p),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(
            last,
            Some(Progress {
                done: data.len() as u64,
                total: Some(data.len() as u64)
            })
        );
    }

    #[test]
    fn expected_hashes_are_parsed() {
        let md5: ExpectedHash = "900150983CD24FB0D6963F7D28E17F72".parse().unwrap();
        assert_eq!(md5.algorithm, HashAlgorithm::Md5);
        assert_eq!(md5.hex, "900150983cd24fb0d6963f7d28e17f72");
        let sha1: ExpectedHash = "sha-1:a9993e364706816aba3e25717850c26c9cd0d89d"
            .parse()
            .unwrap();
        assert_eq!(sha1.algorithm, HashAlgorithm::Sha1);
        assert!("sha256:abcd".parse::<ExpectedHash>().is_err());
        assert!("crc32:abcd".parse::<ExpectedHash>().is_err());
        assert!("xyz".parse::<ExpectedHash>().is_err());
        assert!("abc".parse::<ExpectedHash>().is_err());
        assert!("".parse::<ExpectedHash>().is_err());
    }

    #[test]
    fn reports_compare_with_expected_hashes() {
        let report = hash_bytes(b"abc", &[HashAlgorithm::Md5, HashAlgorithm::Sha256]);
        let good: ExpectedHash = "md5:900150983cd24fb0d6963f7d28e17f72".parse().unwrap();
        let bad: ExpectedHash = "md5:00000000000000000000000000000000".parse().unwrap();
        let missing: ExpectedHash = "sha1:a9993e364706816aba3e25717850c26c9cd0d89d"
            .parse()
            .unwrap();
        assert_eq!(report.matches(&good), Some(true));
        assert_eq!(report.matches(&bad), Some(false));
        assert_eq!(report.matches(&missing), None);
    }

    #[test]
    fn algorithm_names_round_trip() {
        for algorithm in HashAlgorithm::ALL {
            assert_eq!(HashAlgorithm::from_name(algorithm.name()), Some(algorithm));
        }
        assert_eq!(
            HashAlgorithm::from_name("SHA-512"),
            Some(HashAlgorithm::Sha512)
        );
        assert_eq!(HashAlgorithm::from_name("crc32"), None);
    }
}
