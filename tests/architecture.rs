//! Architecture guard for the hard rule:
//! the server never imports a concrete driver.

#[test]
fn server_does_not_import_a_concrete_driver() {
    let source = include_str!("../src/server/mod.rs");
    for needle in [
        "crate::camera::qianli",
        "crate::camera::p3",
        "crate::camera::uvc_radiometric",
        "crate::camera::mock",
    ] {
        assert!(
            !source.contains(needle),
            "src/server/mod.rs imports a concrete driver: {needle}"
        );
    }
}
