//! `hash_file()` against real files.

use ferrufus_core::cancel::CancelToken;
use ferrufus_core::hash::{HashAlgorithm, hash_file};
use ferrufus_core::{Error, ErrorKind};
use std::io::Write;

#[test]
fn hashes_a_file() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"abc").unwrap();
    let report = hash_file(
        file.path(),
        &HashAlgorithm::ALL,
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(report.size, 3);
    assert_eq!(
        report.get(HashAlgorithm::Md5).unwrap().hex,
        "900150983cd24fb0d6963f7d28e17f72"
    );
    assert_eq!(
        report.get(HashAlgorithm::Sha1).unwrap().hex,
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        report.get(HashAlgorithm::Sha256).unwrap().hex,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn missing_files_report_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.iso");
    let error = hash_file(
        &path,
        &[HashAlgorithm::Md5],
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io);
    assert!(
        matches!(&error, Error::Io { context, .. } if context.contains("missing.iso")),
        "{error}"
    );
}
