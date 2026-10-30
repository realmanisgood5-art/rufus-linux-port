//! Optional checks against real images. Set `FERRUFUS_TEST_ISOS` to a
//! directory of `.iso` files to run them; nothing is committed and the
//! test does nothing without it. Images are only read.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::image::{InspectOptions, inspect};

#[test]
fn real_isos_inspect_and_scan() {
    let Some(dir) = std::env::var_os("FERRUFUS_TEST_ISOS") else {
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso")))
        .collect();
    paths.sort();
    for path in paths {
        let report = inspect(&path, &InspectOptions::default()).unwrap();
        println!(
            "== {}\n{}",
            path.display(),
            serde_json::to_string_pretty(&report).unwrap()
        );
        assert!(report.is_iso, "{} should be an ISO", path.display());
    }
}
